//! Consti engine — manages constitutional amendments with diff-based operations.
//!
//! Amendments can add, modify, or repeal articles in the constitution.
//! Uses a 90% supermajority threshold (from `consti_supermajority_bps` in params).

use crate::amendment::{Amendment, AmendmentOp};
use crate::document::{Article, ConstiDocument, VersionEntry};
use crate::error::ConstiError;
use burst_governance::proposal::GovernancePhase;
use burst_types::{TxHash, WalletAddress};
use std::collections::HashMap;

/// A vote on a constitutional amendment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstiVote {
    Yea,
    Nay,
    Abstain,
}

/// The constitutional engine manages amendments through their lifecycle,
/// tracking votes and applying diff-based changes to the constitution.
pub struct ConstiEngine {
    /// All submitted amendments indexed by their transaction hash.
    amendments: HashMap<TxHash, Amendment>,
    /// Votes per amendment: amendment_hash → (voter → vote).
    votes: HashMap<TxHash, HashMap<WalletAddress, ConstiVote>>,
    /// The current constitution document (canonical state).
    document: ConstiDocument,
}

impl ConstiEngine {
    /// Create a new consti engine with empty state.
    pub fn new() -> Self {
        Self {
            amendments: HashMap::new(),
            votes: HashMap::new(),
            document: ConstiDocument::genesis(),
        }
    }

    /// Submit a constitutional amendment.
    ///
    /// Validates that the amendment operations are well-formed against the
    /// current constitution state. For example, you can't modify or repeal
    /// an article that doesn't exist.
    pub fn submit_amendment(
        &mut self,
        amendment: Amendment,
        document: &ConstiDocument,
    ) -> Result<TxHash, ConstiError> {
        // Validate operations against current document state
        for op in &amendment.operations {
            match op {
                AmendmentOp::AddArticle { title, text } => {
                    if title.is_empty() || text.is_empty() {
                        return Err(ConstiError::Other(
                            "article title and text must not be empty".to_string(),
                        ));
                    }
                }
                AmendmentOp::ModifyArticle {
                    article_number,
                    new_text,
                } => {
                    if new_text.is_empty() {
                        return Err(ConstiError::Other(
                            "new article text must not be empty".to_string(),
                        ));
                    }
                    if !document.has_active_article(*article_number) {
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
                }
            }
        }

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

        // Update aggregate counts on the amendment
        let amendment = self.amendments.get_mut(amendment_hash).unwrap();
        match vote {
            ConstiVote::Yea => amendment.votes_yea += 1,
            ConstiVote::Nay => amendment.votes_nay += 1,
            ConstiVote::Abstain => amendment.votes_abstain += 1,
        }

        Ok(())
    }

    /// Activate an amendment — apply diff operations to the constitution document.
    ///
    /// If the amendment has diff operations, applies them. Otherwise falls back
    /// to legacy behavior (add an article from the title/text fields).
    /// Apply an amendment to the engine's internal constitution.
    pub fn activate_amendment_internal(
        &mut self,
        amendment: &Amendment,
    ) -> Result<(), ConstiError> {
        Self::apply_amendment(amendment, &mut self.document)
    }

    /// Apply an amendment to the given constitution document.
    pub fn activate_amendment(
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
        let new_version = document.version + 1;

        if amendment.operations.is_empty() {
            // Legacy behavior: add a single article from title/text
            let article_number = document.next_article_number();
            let new_article = Article {
                number: article_number,
                title: amendment.title.clone(),
                text: amendment.text.clone(),
                introduced_by_amendment: new_version,
                repealed: false,
            };
            document.articles.push(new_article);
        } else {
            // Diff-based: apply each operation
            for op in &amendment.operations {
                match op {
                    AmendmentOp::AddArticle { title, text } => {
                        let article_number = document.next_article_number();
                        let new_article = Article {
                            number: article_number,
                            title: title.clone(),
                            text: text.clone(),
                            introduced_by_amendment: new_version,
                            repealed: false,
                        };
                        document.articles.push(new_article);
                    }
                    AmendmentOp::ModifyArticle {
                        article_number,
                        new_text,
                    } => {
                        if let Some(article) = document
                            .articles
                            .iter_mut()
                            .find(|a| a.number == *article_number && !a.repealed)
                        {
                            article.text = new_text.clone();
                            article.introduced_by_amendment = new_version;
                        } else {
                            return Err(ConstiError::ArticleNotFound(*article_number));
                        }
                    }
                    AmendmentOp::RepealArticle { article_number } => {
                        if let Some(article) = document
                            .articles
                            .iter_mut()
                            .find(|a| a.number == *article_number)
                        {
                            if article.repealed {
                                return Err(ConstiError::ArticleAlreadyRepealed(*article_number));
                            }
                            article.repealed = true;
                            article.introduced_by_amendment = new_version;
                        } else {
                            return Err(ConstiError::ArticleNotFound(*article_number));
                        }
                    }
                }
            }
        }

        // Record version history
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
    pub fn get_votes(
        &self,
        hash: &TxHash,
    ) -> Option<&HashMap<WalletAddress, ConstiVote>> {
        self.votes.get(hash)
    }

    /// Check if a constitutional supermajority is met.
    ///
    /// The Consti has its own separate threshold (`consti_supermajority_bps`)
    /// independent from the parameter governance threshold (`governance_supermajority_bps`).
    /// Per the whitepaper: "The Consti threshold is changed by hitting that same
    /// Consti threshold (not the parameter threshold)." This self-referential
    /// property is enforced by `GovernanceEngine::get_required_supermajority`,
    /// which routes `ConstiSupermajorityBps` changes through the current
    /// `consti_supermajority_bps` value.
    pub fn check_supermajority(
        &self,
        votes_yea: u32,
        votes_nay: u32,
        supermajority_bps: u32,
    ) -> Result<(), ConstiError> {
        let total_yea_nay = votes_yea + votes_nay;
        let actual_bps = if total_yea_nay > 0 {
            (votes_yea * 10000) / total_yea_nay
        } else {
            0
        };

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

    // ── Legacy activate (backward compatibility) ────────────────────

    #[test]
    fn test_activate_amendment_adds_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();
        let amendment = create_test_amendment(
            "Test Article".to_string(),
            "This is a test article.".to_string(),
        );

        assert_eq!(document.article_count(), 0);

        engine.activate_amendment(&amendment, &mut document).unwrap();

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

        engine.activate_amendment(&amendment, &mut document).unwrap();

        assert_eq!(document.version, 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.introduced_by_amendment, 1);

        // Activate another amendment
        let amendment2 = create_test_amendment(
            "Second Article".to_string(),
            "This is the second article.".to_string(),
        );
        engine.activate_amendment(&amendment2, &mut document).unwrap();

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

    // ── Diff-based: AddArticle ──────────────────────────────────────

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

        engine.activate_amendment(&amendment, &mut document).unwrap();

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

        engine.activate_amendment(&amendment, &mut document).unwrap();

        assert_eq!(document.article_count(), 2);
        assert_eq!(document.version, 1);
        assert!(document.get_article(1).is_some());
        assert!(document.get_article(2).is_some());
    }

    // ── Diff-based: ModifyArticle ───────────────────────────────────

    #[test]
    fn test_diff_modify_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        // First add an article
        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Verification Standards".to_string(),
                text: "Original text.".to_string(),
            }],
        );
        engine.activate_amendment(&add, &mut document).unwrap();
        assert_eq!(document.get_article(1).unwrap().text, "Original text.");

        // Now modify it
        let modify = create_diff_amendment(
            "Update Verification".to_string(),
            vec![AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "Updated and improved text.".to_string(),
            }],
        );
        engine.activate_amendment(&modify, &mut document).unwrap();

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

        let result = engine.activate_amendment(&modify, &mut document);
        assert!(result.is_err());
    }

    // ── Diff-based: RepealArticle ───────────────────────────────────

    #[test]
    fn test_diff_repeal_article() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        // Add an article
        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Obsolete Rule".to_string(),
                text: "This rule is temporary.".to_string(),
            }],
        );
        engine.activate_amendment(&add, &mut document).unwrap();
        assert_eq!(document.article_count(), 1);

        // Repeal it
        let repeal = create_diff_amendment(
            "Repeal Obsolete".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        engine.activate_amendment(&repeal, &mut document).unwrap();

        assert_eq!(document.version, 2);
        // Active count is 0 (repealed)
        assert_eq!(document.article_count(), 0);
        // But total count is still 1
        assert_eq!(document.total_article_count(), 1);
        // get_article skips repealed
        assert!(document.get_article(1).is_none());
        // Can still access including repealed
        let article = document.get_article_including_repealed(1).unwrap();
        assert!(article.repealed);
        assert_eq!(article.introduced_by_amendment, 2);
    }

    #[test]
    fn test_diff_repeal_already_repealed_fails() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        // Add and repeal
        let add = create_diff_amendment(
            "Initial".to_string(),
            vec![AmendmentOp::AddArticle {
                title: "Temporary".to_string(),
                text: "Temp text.".to_string(),
            }],
        );
        engine.activate_amendment(&add, &mut document).unwrap();

        let repeal = create_diff_amendment(
            "First Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        engine.activate_amendment(&repeal, &mut document).unwrap();

        // Try to repeal again
        let repeal2 = create_diff_amendment(
            "Double Repeal".to_string(),
            vec![AmendmentOp::RepealArticle { article_number: 1 }],
        );
        let result = engine.activate_amendment(&repeal2, &mut document);
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
        let result = engine.activate_amendment(&repeal, &mut document);
        assert!(result.is_err());
    }

    // ── Complex multi-operation amendments ───────────────────────────

    #[test]
    fn test_diff_complex_amendment() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        // Setup: add two articles
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
        engine.activate_amendment(&setup, &mut document).unwrap();
        assert_eq!(document.article_count(), 2);

        // Complex amendment: modify one, repeal another, add a new one
        let complex = create_diff_amendment(
            "Reform Package".to_string(),
            vec![
                AmendmentOp::ModifyArticle {
                    article_number: 1,
                    new_text: "All verified participants have equal BRN accrual rights.".to_string(),
                },
                AmendmentOp::RepealArticle { article_number: 2 },
                AmendmentOp::AddArticle {
                    title: "New Verification".to_string(),
                    text: "Verification uses decentralized endorsement.".to_string(),
                },
            ],
        );
        engine.activate_amendment(&complex, &mut document).unwrap();

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

    // ── Version history tracking ────────────────────────────────────

    #[test]
    fn test_version_history() {
        let engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        engine
            .activate_amendment(
                &create_test_amendment("First".to_string(), "Text 1.".to_string()),
                &mut document,
            )
            .unwrap();
        engine
            .activate_amendment(
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

    // ── Submit amendment validation ─────────────────────────────────

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

    // ── Vote amendment ──────────────────────────────────────────────

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

        // Cast votes
        engine.vote_amendment(&hash, &voter_wallet(1), ConstiVote::Yea).unwrap();
        engine.vote_amendment(&hash, &voter_wallet(2), ConstiVote::Nay).unwrap();
        engine.vote_amendment(&hash, &voter_wallet(3), ConstiVote::Abstain).unwrap();

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

        engine.vote_amendment(&hash, &voter_wallet(1), ConstiVote::Yea).unwrap();
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

    // ── Supermajority check ─────────────────────────────────────────

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

    // ── Full lifecycle with engine ──────────────────────────────────

    #[test]
    fn test_full_amendment_lifecycle() {
        let mut engine = ConstiEngine::new();
        let mut document = ConstiDocument::genesis();

        // Submit
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

        // Check supermajority
        let stored = engine.get_amendment(&hash).unwrap();
        assert!(engine
            .check_supermajority(stored.votes_yea, stored.votes_nay, 9000)
            .is_ok());

        // Activate
        engine
            .activate_amendment(engine.get_amendment(&hash).unwrap(), &mut document)
            .unwrap();

        assert_eq!(document.version, 1);
        assert_eq!(document.article_count(), 1);
        let article = document.get_article(1).unwrap();
        assert_eq!(article.title, "Right to Privacy");
    }
}
