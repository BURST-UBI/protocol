//! Keyed local governance state. Records neither authenticate state nor authorize activation.
use super::*;
use bincode::Options;
use imbl::ordmap::DiffItem;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceRecord {
    Proposal(Box<Proposal>),
    VoteBucket {
        proposal: TxHash,
        promotion: bool,
    },
    Vote {
        proposal: TxHash,
        promotion: bool,
        voter: WalletAddress,
        vote: GovernanceVote,
    },
    Endorsement {
        proposal: TxHash,
        count: u32,
        burned: u128,
    },
    Timing {
        proposal: TxHash,
        timing: crate::GovernanceTiming,
    },
    KeyBallot {
        proposal: TxHash,
        owner: WalletAddress,
        grant: burst_types::BlockHash,
        vote: GovernanceVote,
    },
    PendingEffects {
        changes: Vec<(crate::GovernableParam, u128)>,
        amendments: Vec<ProposalContent>,
        evictions: Vec<(WalletAddress, bool)>,
    },
}

impl GovernanceRecord {
    pub const STORAGE_NAMESPACE: &'static str = "governance_state/";
    pub const STORAGE_PREFIX: &'static str = "governance_state/v1/";

    fn byte_limit(key: &str) -> usize {
        match key.strip_prefix(Self::STORAGE_PREFIX) {
            // Pending effects share the aggregate undo size limit.
            Some("0") => super::snapshot_undo::MAX_BYTES,
            // Local proposal records allow the full content plus metadata;
            // network transport still uses bounded chunks.
            Some(suffix) if suffix.starts_with("1/") => {
                crate::content::MAX_CONTENT_BYTES + burst_types::MAX_P2P_MESSAGE_BYTES
            }
            _ => burst_types::MAX_P2P_MESSAGE_BYTES,
        }
    }

    fn validate_content(&self) -> Result<(), String> {
        match self {
            Self::Proposal(proposal) => proposal.content.validate_content(),
            Self::PendingEffects { amendments, .. } => {
                amendments.iter().try_for_each(ProposalContent::validate_content)
            }
            _ => Ok(()),
        }
        .map_err(|error| error.to_string())
    }

    /// Local proposal conflict scope; pending effects require a global boundary.
    pub fn proposal_scope(&self) -> Option<TxHash> {
        match self {
            Self::Proposal(p) => Some(p.hash),
            Self::VoteBucket { proposal, .. }
            | Self::Vote { proposal, .. }
            | Self::Endorsement { proposal, .. }
            | Self::Timing { proposal, .. }
            | Self::KeyBallot { proposal, .. } => Some(*proposal),
            Self::PendingEffects { .. } => None,
        }
    }

    pub fn storage_key(&self) -> Result<String, String> {
        let suffix = match self {
            Self::PendingEffects { .. } => "0".to_owned(),
            Self::Proposal(p) => format!("1/{}", hex::encode(p.hash.as_bytes())),
            Self::VoteBucket {
                proposal,
                promotion,
            } => format!(
                "2/{}/{}",
                u8::from(*promotion),
                hex::encode(proposal.as_bytes())
            ),
            Self::Vote {
                proposal,
                promotion,
                voter,
                ..
            } => format!(
                "3/{}/{}/{}",
                u8::from(*promotion),
                hex::encode(proposal.as_bytes()),
                hex::encode(voter.as_str().as_bytes())
            ),
            Self::Endorsement { proposal, .. } => format!("4/{}", hex::encode(proposal.as_bytes())),
            Self::Timing { proposal, .. } => format!("5/{}", hex::encode(proposal.as_bytes())),
            Self::KeyBallot {
                proposal,
                owner,
                grant,
                ..
            } => format!(
                "6/{}/{}/{}",
                hex::encode(proposal.as_bytes()),
                hex::encode(owner.as_str().as_bytes()),
                hex::encode(grant.as_bytes())
            ),
        };
        let key = format!("{}{suffix}", Self::STORAGE_PREFIX);
        if key.len() > 511 {
            return Err("governance record key exceeds LMDB limit".into());
        }
        Ok(key)
    }

    pub fn storage_entry(&self) -> Result<(String, Vec<u8>), String> {
        let key = self.storage_key()?;
        self.validate_content()?;
        if bincode::serialized_size(&(1u8, self)).map_err(|e| e.to_string())?
            > Self::byte_limit(&key) as u64
        {
            return Err("governance record exceeds byte limit".into());
        }
        Ok((
            key,
            bincode::serialize(&(1u8, self)).map_err(|e| e.to_string())?,
        ))
    }

    pub fn from_storage_entry(key: &str, bytes: &[u8]) -> Result<Self, String> {
        let limit = Self::byte_limit(key);
        if !key.starts_with(Self::STORAGE_PREFIX)
            || key.len() > 511
            || bytes.len() > limit
        {
            return Err("invalid governance record bounds or namespace".into());
        }
        let (version, record): (u8, Self) = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(limit as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| e.to_string())?;
        if version != 1 || record.storage_key()? != key {
            return Err("governance record version or key mismatch".into());
        }
        record.validate_content()?;
        Ok(record)
    }
}

type Visitor<'a> =
    dyn FnMut(Option<GovernanceRecord>, Option<GovernanceRecord>) -> Result<(), String> + 'a;

fn changed<K: Ord + Clone, V: Clone + PartialEq>(
    before: &OrdMap<K, V>,
    after: &OrdMap<K, V>,
    mut visit: impl FnMut(&K, Option<&V>, Option<&V>) -> Result<(), String>,
) -> Result<(), String> {
    for change in before.diff(after) {
        match change {
            DiffItem::Add(k, v) => visit(k, None, Some(v))?,
            DiffItem::Remove(k, v) => visit(k, Some(v), None)?,
            DiffItem::Update {
                old: (k, a),
                new: (_, b),
            } => visit(k, Some(a), Some(b))?,
        }
    }
    Ok(())
}

fn votes(
    before: &OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    after: &OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    promotion: bool,
    visit: &mut Visitor<'_>,
) -> Result<(), String> {
    changed(before, after, |proposal, a, b| {
        let bucket = || GovernanceRecord::VoteBucket {
            proposal: *proposal,
            promotion,
        };
        if a.is_some() != b.is_some() {
            visit(a.map(|_| bucket()), b.map(|_| bucket()))?;
        }
        let empty = OrdMap::new();
        changed(
            a.unwrap_or(&empty),
            b.unwrap_or(&empty),
            |voter, old, new| {
                let record = |vote: &GovernanceVote| GovernanceRecord::Vote {
                    proposal: *proposal,
                    promotion,
                    voter: voter.clone(),
                    vote: *vote,
                };
                visit(old.map(record), new.map(record))
            },
        )
    })
}

impl GovernanceEngine {
    /// Whether undo must compare the complete governance state because an
    /// engine-wide effect queue changed.
    pub fn pending_effects_differ(&self, other: &Self) -> bool {
        self.pending_changes != other.pending_changes
            || self.pending_amendments != other.pending_amendments
            || self.pending_evictions != other.pending_evictions
    }

    /// Decode bounded aggregate snapshots and visit their changed local rows.
    /// Used by rollback planning; this does not authenticate either snapshot.
    pub fn visit_snapshot_record_delta(
        before: &[u8],
        after: &[u8],
        visit: impl FnMut(Option<GovernanceRecord>, Option<GovernanceRecord>) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = super::snapshot_undo::decode(before)?;
        let after = super::snapshot_undo::decode(after)?;
        after.visit_record_delta(&before, visit)
    }

    /// An absent image means no initialized governance state, distinct from an empty engine.
    pub fn visit_optional_snapshot_record_delta(
        before: Option<&[u8]>,
        after: Option<&[u8]>,
        mut visit: impl FnMut(Option<GovernanceRecord>, Option<GovernanceRecord>) -> Result<(), String>,
    ) -> Result<(), String> {
        match (before, after) {
            (Some(before), Some(after)) => Self::visit_snapshot_record_delta(before, after, visit),
            (None, Some(after)) => {
                super::snapshot_undo::decode(after)?.visit_records(|row| visit(None, Some(row)))
            }
            (Some(before), None) => {
                super::snapshot_undo::decode(before)?.visit_records(|row| visit(Some(row), None))
            }
            (None, None) => Ok(()),
        }
    }

    fn pending_record(&self) -> GovernanceRecord {
        GovernanceRecord::PendingEffects {
            changes: self.pending_changes.as_ref().clone(),
            amendments: self.pending_amendments.as_ref().clone(),
            evictions: self.pending_evictions.as_ref().clone(),
        }
    }

    /// Identity check for a candidate derived from a cached storage image.
    /// Maps and pending effects share roots; detached effects retain value semantics.
    pub fn shares_record_storage_with(&self, other: &Self) -> bool {
        self.proposals.ptr_eq(&other.proposals)
            && self.exploration_votes.ptr_eq(&other.exploration_votes)
            && self.promotion_votes.ptr_eq(&other.promotion_votes)
            && self.endorsement_counts.ptr_eq(&other.endorsement_counts)
            && self.timings.ptr_eq(&other.timings)
            && self.key_ballots.ptr_eq(&other.key_ballots)
            && !self.pending_effects_differ(other)
    }

    /// Visit exact changed records, skipping shared ordered-map subtrees.
    /// Shared pending effects skip payload comparison; detached effects compare in full.
    /// A callback error stops traversal but does not undo earlier callbacks.
    /// Callers writing records must stage those writes atomically.
    pub fn visit_record_delta(
        &self,
        before: &Self,
        mut visit: impl FnMut(Option<GovernanceRecord>, Option<GovernanceRecord>) -> Result<(), String>,
    ) -> Result<(), String> {
        changed(&before.proposals, &self.proposals, |_, a, b| {
            visit(
                a.map(|p| GovernanceRecord::Proposal(Box::new(p.clone()))),
                b.map(|p| GovernanceRecord::Proposal(Box::new(p.clone()))),
            )
        })?;
        votes(
            &before.exploration_votes,
            &self.exploration_votes,
            false,
            &mut visit,
        )?;
        votes(
            &before.promotion_votes,
            &self.promotion_votes,
            true,
            &mut visit,
        )?;
        changed(
            &before.endorsement_counts,
            &self.endorsement_counts,
            |proposal, a, b| {
                let record = |&(count, burned): &(u32, u128)| GovernanceRecord::Endorsement {
                    proposal: *proposal,
                    count,
                    burned,
                };
                visit(a.map(record), b.map(record))
            },
        )?;
        changed(&before.timings, &self.timings, |proposal, a, b| {
            let record = |timing: &crate::GovernanceTiming| GovernanceRecord::Timing {
                proposal: *proposal,
                timing: timing.clone(),
            };
            visit(a.map(record), b.map(record))
        })?;
        changed(
            &before.key_ballots,
            &self.key_ballots,
            |(proposal, owner, grant), a, b| {
                let record = |vote: &GovernanceVote| GovernanceRecord::KeyBallot {
                    proposal: *proposal,
                    owner: owner.clone(),
                    grant: *grant,
                    vote: *vote,
                };
                visit(a.map(record), b.map(record))
            },
        )?;
        if self.pending_changes != before.pending_changes
            || self.pending_amendments != before.pending_amendments
            || self.pending_evictions != before.pending_evictions
        {
            visit(Some(before.pending_record()), Some(self.pending_record()))?;
        }
        Ok(())
    }

    /// Full export is for migration/checkpoint boundaries, not each vote.
    pub fn visit_records(
        &self,
        mut visit: impl FnMut(GovernanceRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        visit(self.pending_record())?;
        self.visit_record_delta(&Self::new(), |_, new| {
            if let Some(record) = new {
                if !matches!(record, GovernanceRecord::PendingEffects { .. }) {
                    visit(record)?;
                }
            }
            Ok(())
        })
    }

    /// Reconstruct local state, preserving empty ballot buckets and all pending effects.
    /// Inputs must already have passed `GovernanceRecord::from_storage_entry` or
    /// equivalent validation. This checks duplicate keys and required buckets,
    /// not content validity or cross-record protocol consistency. The caller must
    /// authenticate the checkpoint or use a trusted atomic database view.
    pub fn from_records(
        records: impl IntoIterator<Item = Result<GovernanceRecord, String>>,
    ) -> Result<Self, String> {
        let mut engine = Self::new();
        let mut seen = BTreeSet::new();
        let mut buckets = BTreeSet::new();
        let mut pending = false;
        for record in records {
            let record = record?;
            if !seen.insert(record.storage_key()?) {
                return Err("duplicate governance record".into());
            }
            match record {
                GovernanceRecord::Proposal(p) => {
                    engine.proposals.insert(p.hash, *p);
                }
                GovernanceRecord::VoteBucket {
                    proposal,
                    promotion,
                } => {
                    buckets.insert((proposal, promotion));
                    let map = if promotion {
                        &mut engine.promotion_votes
                    } else {
                        &mut engine.exploration_votes
                    };
                    map.entry(proposal).or_default();
                }
                GovernanceRecord::Vote {
                    proposal,
                    promotion,
                    voter,
                    vote,
                } => {
                    let map = if promotion {
                        &mut engine.promotion_votes
                    } else {
                        &mut engine.exploration_votes
                    };
                    map.entry(proposal).or_default().insert(voter, vote);
                }
                GovernanceRecord::Endorsement {
                    proposal,
                    count,
                    burned,
                } => {
                    engine.endorsement_counts.insert(proposal, (count, burned));
                }
                GovernanceRecord::Timing { proposal, timing } => {
                    engine.timings.insert(proposal, timing);
                }
                GovernanceRecord::KeyBallot {
                    proposal,
                    owner,
                    grant,
                    vote,
                } => {
                    engine.key_ballots.insert((proposal, owner, grant), vote);
                }
                GovernanceRecord::PendingEffects {
                    changes,
                    amendments,
                    evictions,
                } => {
                    pending = true;
                    engine.pending_changes = Arc::new(changes);
                    engine.pending_amendments = Arc::new(amendments);
                    engine.pending_evictions = Arc::new(evictions);
                }
            }
        }
        if !pending {
            return Err("missing governance pending-effects record".into());
        }
        for (promotion, map) in [
            (false, &engine.exploration_votes),
            (true, &engine.promotion_votes),
        ] {
            if map.keys().any(|id| !buckets.contains(&(*id, promotion))) {
                return Err("missing governance ballot bucket".into());
            }
        }
        Ok(engine)
    }
}
