//! Amendment storage and atomic in-memory document patches.
//!
//! Amendments can add, modify, or repeal articles in the constitution.
//! Voting uses the governed `consti_supermajority_bps` threshold.

use crate::amendment::{Amendment, AmendmentOp};
use crate::document::{Article, ConstiDocument, VersionEntry};
use crate::error::ConstiError;
use burst_governance::proposal::GovernancePhase;
use burst_types::{TxHash, WalletAddress};
use std::collections::HashMap;

mod retention;
mod snapshot_undo;
mod snapshot_view;
pub use snapshot_view::SnapshotView;

/// A vote on a constitutional amendment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConstiVote {
    Yea,
    Nay,
    Abstain,
}

/// An approved patch cannot be applied to the constitution at activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AmendmentConflict {
    ArticleNotFound(u64),
    ArticleAlreadyRepealed(u64),
}

/// Stores amendments, local votes and the current document. Activation callers
/// must supply governance authorization; this engine does not verify signatures
/// or a certified counting/activation boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ConstiEngine(std::sync::Arc<ConstiState>);

/// Shared engine representation; fields remain private to the engine.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConstiState {
    /// All submitted amendments indexed by their transaction hash.
    amendments: HashMap<TxHash, Amendment>,
    /// Votes per amendment: amendment_hash → (voter → vote).
    votes: HashMap<TxHash, HashMap<WalletAddress, ConstiVote>>,
    /// The current constitution document (canonical state).
    document: ConstiDocument,
    conflicts: HashMap<TxHash, AmendmentConflict>,
}

impl std::ops::Deref for ConstiEngine {
    type Target = ConstiState;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ConstiEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl ConstiEngine {
    /// Whether two immutable candidates share the same complete backing state.
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    /// Create a new consti engine with empty state.
    pub fn new() -> Self {
        Self(std::sync::Arc::new(ConstiState {
            amendments: HashMap::new(),
            votes: HashMap::new(),
            document: ConstiDocument::genesis(),
            conflicts: HashMap::new(),
        }))
    }

    /// Seed the launch articles rather than an empty document.
    pub fn bootstrap() -> Self {
        Self(std::sync::Arc::new(ConstiState {
            amendments: HashMap::new(),
            votes: HashMap::new(),
            document: ConstiDocument::bootstrap(),
            conflicts: HashMap::new(),
        }))
    }

    /// Submit a constitutional amendment.
    ///
    /// Validate the patch against the supplied document, then store it by hash.
    /// The supplied document may differ from this engine's document; callers
    /// choose the authoritative view and enforce proposal identity.
    pub fn submit_amendment(
        &mut self,
        amendment: Amendment,
        document: &ConstiDocument,
    ) -> Result<TxHash, ConstiError> {
        Self::apply_amendment(&amendment, &mut document.clone())?;

        let hash = amendment.hash;
        self.amendments.insert(hash, amendment);
        Ok(hash)
    }

    /// Vote on a constitutional amendment.
    ///
    /// Tracks votes in a per-amendment HashMap. Each voter can only vote once.
    pub fn vote_amendment(
        &mut self,
        amendment_hash: &TxHash,
        voter: &WalletAddress,
        vote: ConstiVote,
    ) -> Result<(), ConstiError> {
        let amendment = self
            .amendments
            .get(amendment_hash)
            .ok_or_else(|| ConstiError::AmendmentNotFound(amendment_hash.to_string()))?;

        if amendment.phase != GovernancePhase::Exploration
            && amendment.phase != GovernancePhase::Promotion
        {
            return Err(ConstiError::WrongPhase);
        }

        let votes = self.votes.entry(*amendment_hash).or_default();

        if votes.contains_key(voter) {
            return Err(ConstiError::AlreadyVoted(voter.to_string()));
        }

        votes.insert(voter.clone(), vote);

        let amendment = self.amendments.get_mut(amendment_hash).unwrap();
        match vote {
            ConstiVote::Yea => amendment.votes_yea += 1,
            ConstiVote::Nay => amendment.votes_nay += 1,
            ConstiVote::Abstain => amendment.votes_abstain += 1,
        }

        Ok(())
    }

    /// Apply the patch and mark it activated. Reject an already activated or
    /// conflicted stored amendment. The caller must establish approval and
    /// activation timing; this method does not check a quorum certificate.
    pub fn activate_amendment(&mut self, amendment: &Amendment) -> Result<(), ConstiError> {
        if self.amendments.get(&amendment.hash).is_some_and(|saved| {
            matches!(
                saved.phase,
                GovernancePhase::Activated | GovernancePhase::Conflicted
            )
        }) {
            return Err(ConstiError::WrongPhase);
        }
        Self::apply_amendment(amendment, &mut self.document)?;
        let mut activated = amendment.clone();
        activated.phase = GovernancePhase::Activated;
        self.amendments.insert(amendment.hash, activated);
        Ok(())
    }

    /// Resolve an approved patch atomically. Target conflicts are terminal;
    /// invalid content, corrupt state and arithmetic failures remain errors.
    pub fn resolve_approved_amendment(
        &mut self,
        amendment: &Amendment,
    ) -> Result<Option<AmendmentConflict>, ConstiError> {
        let conflict = match self.activate_amendment(amendment) {
            Ok(()) => return Ok(None),
            Err(ConstiError::ArticleNotFound(number)) => AmendmentConflict::ArticleNotFound(number),
            Err(ConstiError::ArticleAlreadyRepealed(number)) => {
                AmendmentConflict::ArticleAlreadyRepealed(number)
            }
            Err(error) => return Err(error),
        };
        let mut saved = amendment.clone();
        saved.phase = GovernancePhase::Conflicted;
        self.amendments.insert(saved.hash, saved);
        self.conflicts.insert(amendment.hash, conflict);
        Ok(Some(conflict))
    }

    pub fn activation_conflict(&self, hash: &TxHash) -> Option<AmendmentConflict> {
        self.conflicts.get(hash).copied()
    }

    /// Apply an amendment to the given constitution document.
    pub fn apply_amendment_to_document(
        &self,
        amendment: &Amendment,
        document: &mut ConstiDocument,
    ) -> Result<(), ConstiError> {
        Self::apply_amendment(amendment, document)
    }

    fn apply_amendment(
        amendment: &Amendment,
        document: &mut ConstiDocument,
    ) -> Result<(), ConstiError> {
        let content = if amendment.operations.is_empty() {
            burst_governance::ProposalContent::ConstitutionalAmendment {
                title: amendment.title.clone(),
                text: amendment.text.clone(),
            }
        } else {
            burst_governance::ProposalContent::ConstitutionalDiff {
                title: amendment.title.clone(),
                operations: amendment.operations.clone(),
            }
        };
        content
            .validate_content()
            .map_err(|error| ConstiError::Other(error.to_string()))?;
        let mut candidate = document.clone();
        Self::apply_operations(amendment, &mut candidate)?;
        *document = candidate;
        Ok(())
    }

    fn apply_operations(
        amendment: &Amendment,
        document: &mut ConstiDocument,
    ) -> Result<(), ConstiError> {
        let new_version = document
            .version
            .checked_add(1)
            .ok_or_else(|| ConstiError::Other("constitution version overflow".into()))?;

        if amendment.operations.is_empty() {
            // The whole-text form adds one article.
            document.push_article(Article {
                number: 0, // assigned by push_article
                title: amendment.title.clone(),
                text: amendment.text.clone(),
                introduced_by_amendment: new_version,
                repealed: false,
            });
        } else {
            for op in &amendment.operations {
                match op {
                    AmendmentOp::AddArticle { title, text } => {
                        document.push_article(Article {
                            number: 0, // assigned by push_article
                            title: title.clone(),
                            text: text.clone(),
                            introduced_by_amendment: new_version,
                            repealed: false,
                        });
                    }
                    AmendmentOp::ModifyArticle {
                        article_number,
                        new_text,
                    } => {
                        if let Some(article) = document.get_article_mut(*article_number) {
                            article.text = new_text.clone();
                            article.introduced_by_amendment = new_version;
                        } else {
                            return Err(ConstiError::ArticleNotFound(*article_number));
                        }
                    }
                    AmendmentOp::RepealArticle { article_number } => {
                        let article = document
                            .get_article_including_repealed(*article_number)
                            .ok_or(ConstiError::ArticleNotFound(*article_number))?;
                        if article.repealed {
                            return Err(ConstiError::ArticleAlreadyRepealed(*article_number));
                        }
                        document.repeal_article(*article_number);
                        if let Some(&pos) = document.article_index.get(article_number) {
                            if let Some(a) = document.articles.get_mut(pos) {
                                a.introduced_by_amendment = new_version;
                            }
                        }
                    }
                }
            }
        }

        document.version = new_version;
        document.version_history.push(VersionEntry {
            version: new_version,
            description: amendment.title.clone(),
        });

        Ok(())
    }

    /// Get the current constitution.
    pub fn get_constitution(&self) -> &ConstiDocument {
        &self.document
    }

    /// Get a reference to a stored amendment by hash.
    pub fn get_amendment(&self, hash: &TxHash) -> Option<&Amendment> {
        self.amendments.get(hash)
    }

    /// Get a mutable reference to a stored amendment by hash.
    pub fn get_amendment_mut(&mut self, hash: &TxHash) -> Option<&mut Amendment> {
        self.amendments.get_mut(hash)
    }

    /// Get all votes for an amendment.
    pub fn get_votes(&self, hash: &TxHash) -> Option<&HashMap<WalletAddress, ConstiVote>> {
        self.votes.get(hash)
    }

    /// Compare yea/(yea+nay), truncated to basis points, with a supplied threshold.
    /// Zero votes produce zero basis points. This helper checks neither quorum
    /// nor voter eligibility and does not load the governed threshold.
    /// Callers must bound counts so u32 addition and multiplication by 10,000 fit.
    pub fn check_supermajority(
        &self,
        votes_yea: u32,
        votes_nay: u32,
        supermajority_bps: u32,
    ) -> Result<(), ConstiError> {
        let total_yea_nay = votes_yea + votes_nay;
        let actual_bps = (votes_yea * 10000).checked_div(total_yea_nay).unwrap_or(0);

        if actual_bps < supermajority_bps {
            Err(ConstiError::SupermajorityNotMet {
                have_bps: actual_bps,
                need_bps: supermajority_bps,
            })
        } else {
            Ok(())
        }
    }
}

impl Default for ConstiEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_governance::proposal::GovernancePhase;
    use burst_types::{Timestamp, TxHash, WalletAddress};

    #[test]
    fn shared_constitution_clone_detaches_and_preserves_wire_layout() {
        let original = ConstiEngine::bootstrap();
        let mut candidate = original.clone();
        assert!(std::sync::Arc::ptr_eq(&original.0, &candidate.0));
        let encoded = bincode::serialize(&original).unwrap();
        let legacy: ConstiState = bincode::deserialize(&encoded).unwrap();
        assert_eq!(legacy, *original.0);
        assert_eq!(bincode::serialize(original.0.as_ref()).unwrap(), encoded);
        let restored: ConstiEngine =
            bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        assert_eq!(restored, original);
        candidate
            .activate_amendment(&create_test_amendment(
                "Shared-state amendment".into(),
                "Preserve the original constitution.".into(),
            ))
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(&original.0, &candidate.0));
        assert_eq!(original, restored);
        assert_ne!(candidate.document, original.document);
    }

    fn test_wallet() -> WalletAddress {
        WalletAddress::new("brst_test123456789012345678901234567890")
    }

    fn voter_wallet(id: u32) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>75}", id))
    }

    fn unique_hash(seed: u8) -> TxHash {
        TxHash::new([seed; 32])
    }

    fn create_test_amendment(title: String, text: String) -> Amendment {
        Amendment {
            hash: TxHash::ZERO,
            proposer: test_wallet(),
            title,
            text,
            phase: GovernancePhase::Proposal,
            votes_yea: 0,
            votes_nay: 0,
            votes_abstain: 0,
            created_at: Timestamp::EPOCH,
            operations: Vec::new(),
        }
    }

    fn create_diff_amendment(title: String, ops: Vec<AmendmentOp>) -> Amendment {
        Amendment {
            hash: TxHash::ZERO,
            proposer: test_wallet(),
            title,
            text: String::new(),
            phase: GovernancePhase::Exploration,
            votes_yea: 0,
            votes_nay: 0,
            votes_abstain: 0,
            created_at: Timestamp::EPOCH,
            operations: ops,
        }
    }

    #[test]
    fn approved_conflicts_are_atomic_terminal_and_preserve_votes() {
        for repeat_repeal in [false, true] {
            let mut engine = ConstiEngine::bootstrap();
            let mut repeal = create_diff_amendment(
                "Repeal".into(),
                vec![AmendmentOp::RepealArticle { article_number: 1 }],
            );
            repeal.hash = unique_hash(1);
            let mut later = create_diff_amendment(
                "Independent patch".into(),
                vec![
                    AmendmentOp::AddArticle {
                        title: "Must not leak".into(),
                        text: "new".into(),
                    },
                    if repeat_repeal {
                        AmendmentOp::RepealArticle { article_number: 1 }
                    } else {
                        AmendmentOp::ModifyArticle {
                            article_number: 1,
                            new_text: "updated".into(),
                        }
                    },
                ],
            );
            later.hash = unique_hash(2);
            later.votes_yea = 9;
            later.votes_nay = 1;
            // Both patches are valid before either activates.
            engine
                .submit_amendment(repeal.clone(), &engine.get_constitution().clone())
                .unwrap();
            engine
                .submit_amendment(later.clone(), &engine.get_constitution().clone())
                .unwrap();
            assert_eq!(engine.resolve_approved_amendment(&repeal).unwrap(), None);
            let before = serde_json::to_value(engine.get_constitution()).unwrap();
            let conflict = if repeat_repeal {
                AmendmentConflict::ArticleAlreadyRepealed(1)
            } else {
                AmendmentConflict::ArticleNotFound(1)
            };
            assert_eq!(
                engine.resolve_approved_amendment(&later).unwrap(),
                Some(conflict)
            );
            assert_eq!(
                serde_json::to_value(engine.get_constitution()).unwrap(),
                before
            );
            let saved = engine.get_amendment(&later.hash).unwrap();
            assert_eq!(saved.phase, GovernancePhase::Conflicted);
            assert_eq!((saved.votes_yea, saved.votes_nay), (9, 1));
            assert_eq!(engine.activation_conflict(&later.hash), Some(conflict));
            assert!(matches!(
                engine.resolve_approved_amendment(&later),
                Err(ConstiError::WrongPhase)
            ));
            assert!(matches!(
                engine.activate_amendment(&later),
                Err(ConstiError::WrongPhase)
            ));
            let mut independent = create_diff_amendment(
                "Unaffected".into(),
                vec![AmendmentOp::ModifyArticle {
                    article_number: 2,
                    new_text: "still works".into(),
                }],
            );
            independent.hash = unique_hash(3);
            assert_eq!(
                engine.resolve_approved_amendment(&independent).unwrap(),
                None
            );
            assert_eq!(engine.get_constitution().version, 2);
        }
    }

    #[test]
    fn overlapping_modifications_apply_in_activation_order() {
        let mut engine = ConstiEngine::bootstrap();
        for (seed, text) in [(1, "First approved text"), (2, "Later approved text")] {
            let mut amendment = create_diff_amendment(
                "Modify".into(),
                vec![AmendmentOp::ModifyArticle {
                    article_number: 1,
                    new_text: text.into(),
                }],
            );
            amendment.hash = unique_hash(seed);
            assert_eq!(engine.resolve_approved_amendment(&amendment).unwrap(), None);
            assert_eq!(engine.get_constitution().get_article(1).unwrap().text, text);
        }
        assert_eq!(engine.get_constitution().version, 2);
    }

    #[test]
    fn activation_errors_other_than_target_conflicts_remain_errors() {
        let mut engine = ConstiEngine::bootstrap();
        let mut amendment = create_diff_amendment(
            "Update".into(),
            vec![AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "updated".into(),
            }],
        );
        engine.document.version = u64::MAX;
        assert!(matches!(
            engine.resolve_approved_amendment(&amendment),
            Err(ConstiError::Other(_))
        ));
        engine.document.version = 0;
        amendment.title.clear();
        assert!(engine.resolve_approved_amendment(&amendment).is_err());
        assert!(engine.activation_conflict(&amendment.hash).is_none());
        assert!(engine.get_amendment(&amendment.hash).is_none());
        assert_eq!(engine.get_constitution().version, 0);
    }

    #[test]
    fn failed_diff_is_atomic_and_sequential_edits_use_the_candidate_state() {
        let mut engine = ConstiEngine::bootstrap();
        let before = serde_json::to_value(engine.get_constitution()).unwrap();
        let invalid = create_diff_amendment(
            "Invalid".into(),
            vec![
                AmendmentOp::ModifyArticle {
                    article_number: 1,
                    new_text: "changed".into(),
                },
                AmendmentOp::RepealArticle { article_number: 2 },
                AmendmentOp::AddArticle {
                    title: "Added".into(),
                    text: "new".into(),
                },
                AmendmentOp::ModifyArticle {
                    article_number: 999,
                    new_text: "missing".into(),
                },
            ],
        );
        assert!(engine.activate_amendment(&invalid).is_err());
        assert_eq!(
            serde_json::to_value(engine.get_constitution()).unwrap(),
            before
        );
        let valid = create_diff_amendment(
            "Sequential".into(),
            vec![
                AmendmentOp::AddArticle {
                    title: "Added".into(),
                    text: "new".into(),
                },
                AmendmentOp::ModifyArticle {
                    article_number: 4,
                    new_text: "modified".into(),
                },
                AmendmentOp::RepealArticle { article_number: 2 },
            ],
        );
        engine
            .submit_amendment(valid.clone(), &engine.get_constitution().clone())
            .unwrap();
        engine.activate_amendment(&valid).unwrap();
        assert_eq!(engine.get_constitution().version, 1);
        assert_eq!(
            engine.get_constitution().get_article(4).unwrap().text,
            "modified"
        );
        assert!(engine.get_constitution().get_article(2).is_none());
        let before = serde_json::to_value(engine.get_constitution()).unwrap();
        assert!(engine.activate_amendment(&valid).is_err());
        assert_eq!(
            serde_json::to_value(engine.get_constitution()).unwrap(),
            before
        );
    }

    #[test]
    fn test_activate_amendment_adds_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();
        let amendment = create_test_amendment(
            "Test Article".to_string(),
            "This is a test article.".to_string(),
        );

        assert_eq!(document.article_count(), 0);

        engine
            .apply_amendment_to_document(&amendment, &mut document)
            .unwrap();

        assert_eq!(document.article_count(), 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.title, "Test Article");
        assert_eq!(article.text, "This is a test article.");
        assert_eq!(article.number, 1);
        assert!(!article.repealed);
    }

    #[test]
    fn test_activate_amendment_increments_version() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();
        let amendment = create_test_amendment(
            "Test Article".to_string(),
            "This is a test article.".to_string(),
        );

        assert_eq!(document.version, 0);

        engine
            .apply_amendment_to_document(&amendment, &mut document)
            .unwrap();

        assert_eq!(document.version, 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.introduced_by_amendment, 1);

        let amendment2 = create_test_amendment(
            "Second Article".to_string(),
            "This is the second article.".to_string(),
        );
        engine
            .apply_amendment_to_document(&amendment2, &mut document)
            .unwrap();

        assert_eq!(document.version, 2);
        let article2 = document.get_article(2).unwrap();
        assert_eq!(article2.introduced_by_amendment, 2);
    }

    #[test]
    fn test_genesis_is_empty() {
        let engine = ConstiEngine::new();
        let constitution = engine.get_constitution();

        assert_eq!(constitution.version, 0);
        assert_eq!(constitution.article_count(), 0);
        assert!(constitution.articles.is_empty());
    }

    #[test]
    fn test_diff_add_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();
        let amendment = create_diff_amendment(
            "Add Privacy Article".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Right to Privacy".to_string(),
                text: "Every participant has the right to financial privacy.".to_string(),
            }],
        );

        engine
            .apply_amendment_to_document(&amendment, &mut document)
            .unwrap();

        assert_eq!(document.article_count(), 1);
        assert_eq!(document.version, 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.title, "Right to Privacy");
        assert!(!article.repealed);
    }

    #[test]
    fn test_diff_add_multiple_articles() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();
        let amendment = create_diff_amendment(
            "Initial Articles".to_string(),
            vec![
                AmendmentOp::AddArticle {
                    title: "Article One".to_string(),
                    text: "First article text.".to_string(),
                },
                AmendmentOp::AddArticle {
                    title: "Article Two".to_string(),
                    text: "Second article text.".to_string(),
                },
            ],
        );

        engine
            .apply_amendment_to_document(&amendment, &mut document)
            .unwrap();

        assert_eq!(document.article_count(), 2);
        assert_eq!(document.version, 1);
        assert!(document.get_article(1).is_some());
        assert!(document.get_article(2).is_some());
    }

    #[test]
    fn test_diff_modify_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Verification Standards".to_string(),
                text: "Original text.".to_string(),
            }],
        );
        engine
            .apply_amendment_to_document(&add, &mut document)
            .unwrap();
        assert_eq!(document.get_article(1).unwrap().text, "Original text.");

        let modify = create_diff_amendment(
            "Update Verification".to_string(),
            vec![AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "Updated and improved text.".to_string(),
            }],
        );
        engine
            .apply_amendment_to_document(&modify, &mut document)
            .unwrap();

        assert_eq!(document.version, 2);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.text, "Updated and improved text.");
        assert_eq!(article.introduced_by_amendment, 2);
        assert!(!article.repealed);
    }

    #[test]
    fn test_diff_modify_nonexistent_article_fails() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let modify = create_diff_amendment(
            "Bad Modify".to_string(),
            vec![AmendmentOp::ModifyArticle {
                article_number: 99,
                new_text: "This should fail.".to_string(),
            }],
        );

        let result = engine.apply_amendment_to_document(&modify, &mut document);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_repeal_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Obsolete Rule".to_string(),
                text: "This rule is temporary.".to_string(),
            }],
        );
        engine
            .apply_amendment_to_document(&add, &mut document)
            .unwrap();
        assert_eq!(document.article_count(), 1);

        let repeal = create_diff_amendment(
            "Repeal Obsolete".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        engine
            .apply_amendment_to_document(&repeal, &mut document)
            .unwrap();

        assert_eq!(document.version, 2);
        assert_eq!(document.article_count(), 0);
        assert_eq!(document.total_article_count(), 1);
        assert!(document.get_article(1).is_none());
        let article = document.get_article_including_repealed(1).unwrap();
        assert!(article.repealed);
        assert_eq!(article.introduced_by_amendment, 2);
    }

    #[test]
    fn test_diff_repeal_already_repealed_fails() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Temporary".to_string(),
                text: "Temp text.".to_string(),
            }],
        );
        engine
            .apply_amendment_to_document(&add, &mut document)
            .unwrap();

        let repeal = create_diff_amendment(
            "First Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        engine
            .apply_amendment_to_document(&repeal, &mut document)
            .unwrap();

        let repeal2 = create_diff_amendment(
            "Double Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        let result = engine.apply_amendment_to_document(&repeal2, &mut document);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_repeal_nonexistent_article_fails() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let repeal = create_diff_amendment(
            "Bad Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 99 }],
        );
        let result = engine.apply_amendment_to_document(&repeal, &mut document);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_complex_amendment() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let setup = create_diff_amendment(
            "Foundation".to_string(),
            vec![
                AmendmentOp::AddArticle {
                    title: "Equal Rights".to_string(),
                    text: "All participants have equal BRN accrual rights.".to_string(),
                },
                AmendmentOp::AddArticle {
                    title: "Verification Process".to_string(),
                    text: "Verification requires endorsers and verifiers.".to_string(),
                },
            ],
        );
        engine
            .apply_amendment_to_document(&setup, &mut document)
            .unwrap();
        assert_eq!(document.article_count(), 2);

        // Complex amendment: modify one, repeal another, add a new one
        let complex = create_diff_amendment(
            "Reform Package".to_string(),
            vec![
                AmendmentOp::ModifyArticle {
                    article_number: 1,
                    new_text: "All verified participants have equal BRN accrual rights."
                        .to_string(),
                },
                AmendmentOp::RepealArticle { article_number: 2 },
                AmendmentOp::AddArticle {
                    title: "New Verification".to_string(),
                    text: "Verification uses decentralized endorsement.".to_string(),
                },
            ],
        );
        engine
            .apply_amendment_to_document(&complex, &mut document)
            .unwrap();

        assert_eq!(document.version, 2);
        // 2 active articles (article 1 modified, article 2 repealed, article 3 added)
        assert_eq!(document.article_count(), 2);
        assert_eq!(document.total_article_count(), 3);

        let a1 = document.get_article(1).unwrap();
        assert_eq!(
            a1.text,
            "All verified participants have equal BRN accrual rights."
        );
        assert!(document.get_article(2).is_none()); // repealed
        let a3 = document.get_article(3).unwrap();
        assert_eq!(a3.title, "New Verification");
    }

    #[test]
    fn test_version_history() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        engine
            .apply_amendment_to_document(
                &create_test_amendment("First".to_string(), "Text 1.".to_string()),
                &mut document,
            )
            .unwrap();
        engine
            .apply_amendment_to_document(
                &create_test_amendment("Second".to_string(), "Text 2.".to_string()),
                &mut document,
            )
            .unwrap();

        assert_eq!(document.version_history.len(), 2);
        assert_eq!(document.version_history[0].version, 1);
        assert_eq!(document.version_history[0].description, "First");
        assert_eq!(document.version_history[1].version, 2);
        assert_eq!(document.version_history[1].description, "Second");
    }

    #[test]
    fn test_submit_amendment_validates_add() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let amendment = create_diff_amendment(
            "Good Add".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Title".to_string(),
                text: "Text".to_string(),
            }],
        );
        assert!(engine.submit_amendment(amendment, &document).is_ok());
    }

    #[test]
    fn test_submit_amendment_rejects_empty_title() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let amendment = create_diff_amendment(
            "Bad Add".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "".to_string(),
                text: "Text".to_string(),
            }],
        );
        assert!(engine.submit_amendment(amendment, &document).is_err());
    }

    #[test]
    fn test_submit_amendment_rejects_modify_nonexistent() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let amendment = create_diff_amendment(
            "Bad Modify".to_string(),
            vec![AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "New text".to_string(),
            }],
        );
        assert!(engine.submit_amendment(amendment, &document).is_err());
    }

    #[test]
    fn test_submit_amendment_rejects_repeal_nonexistent() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let amendment = create_diff_amendment(
            "Bad Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        assert!(engine.submit_amendment(amendment, &document).is_err());
    }

    #[test]
    fn test_vote_amendment() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let mut amendment = create_diff_amendment(
            "Vote Test".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Test".to_string(),
                text: "Text".to_string(),
            }],
        );
        amendment.hash = unique_hash(1);
        amendment.phase = GovernancePhase::Exploration;
        let hash = engine.submit_amendment(amendment, &document).unwrap();

        engine
            .vote_amendment(&hash, &voter_wallet(1), ConstiVote::Yea)
            .unwrap();
        engine
            .vote_amendment(&hash, &voter_wallet(2), ConstiVote::Nay)
            .unwrap();
        engine
            .vote_amendment(&hash, &voter_wallet(3), ConstiVote::Abstain)
            .unwrap();

        let stored = engine.get_amendment(&hash).unwrap();
        assert_eq!(stored.votes_yea, 1);
        assert_eq!(stored.votes_nay, 1);
        assert_eq!(stored.votes_abstain, 1);
    }

    #[test]
    fn test_vote_amendment_duplicate_rejected() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let mut amendment = create_diff_amendment(
            "Vote Test".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Test".to_string(),
                text: "Text".to_string(),
            }],
        );
        amendment.hash = unique_hash(2);
        amendment.phase = GovernancePhase::Exploration;
        let hash = engine.submit_amendment(amendment, &document).unwrap();

        engine
            .vote_amendment(&hash, &voter_wallet(1), ConstiVote::Yea)
            .unwrap();
        let result = engine.vote_amendment(&hash, &voter_wallet(1), ConstiVote::Nay);
        assert!(result.is_err());
    }

    #[test]
    fn test_vote_amendment_wrong_phase() {
        let mut engine = ConstiEngine::new();
        let document = ConstiDocument::genesis();

        let mut amendment = create_diff_amendment(
            "Wrong Phase Test".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Test".to_string(),
                text: "Text".to_string(),
            }],
        );
        amendment.hash = unique_hash(3);
        amendment.phase = GovernancePhase::Proposal; // not a voting phase
        let hash = engine.submit_amendment(amendment, &document).unwrap();

        let result = engine.vote_amendment(&hash, &voter_wallet(1), ConstiVote::Yea);
        assert!(result.is_err());
    }

    #[test]
    fn test_check_supermajority_90_percent() {
        let engine = ConstiEngine::new();

        // 90% threshold = 9000 bps
        // 91 yea / 9 nay = 91% = 9100 bps → passes
        assert!(engine.check_supermajority(91, 9, 9000).is_ok());

        // 89 yea / 11 nay = 89% = 8900 bps → fails
        assert!(engine.check_supermajority(89, 11, 9000).is_err());

        // Exact boundary: 90 yea / 10 nay = 90% = 9000 bps → passes
        assert!(engine.check_supermajority(90, 10, 9000).is_ok());
    }

    #[test]
    fn test_check_supermajority_zero_votes() {
        let engine = ConstiEngine::new();
        // No votes → 0 bps → fails
        assert!(engine.check_supermajority(0, 0, 9000).is_err());
    }

    #[test]
    fn test_full_amendment_lifecycle() {
        let mut engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        let mut amendment = create_diff_amendment(
            "Privacy Right".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Right to Privacy".to_string(),
                text: "Participants have the right to financial privacy.".to_string(),
            }],
        );
        amendment.hash = unique_hash(10);
        amendment.phase = GovernancePhase::Exploration;
        let hash = engine.submit_amendment(amendment, &document).unwrap();

        // Vote (90% supermajority for consti)
        for i in 0..91 {
            engine
                .vote_amendment(&hash, &voter_wallet(i), ConstiVote::Yea)
                .unwrap();
        }
        for i in 91..100 {
            engine
                .vote_amendment(&hash, &voter_wallet(i), ConstiVote::Nay)
                .unwrap();
        }

        let stored = engine.get_amendment(&hash).unwrap();
        assert!(engine
            .check_supermajority(stored.votes_yea, stored.votes_nay, 9000)
            .is_ok());

        engine
            .apply_amendment_to_document(engine.get_amendment(&hash).unwrap(), &mut document)
            .unwrap();

        assert_eq!(document.version, 1);
        assert_eq!(document.article_count(), 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.title, "Right to Privacy");
    }
}
