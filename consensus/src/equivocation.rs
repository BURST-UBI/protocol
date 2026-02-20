//! Equivocation detection — catch representatives that vote for conflicting blocks.
//!
//! In ORV consensus, a representative may only vote for **one** block per
//! election (identified by the election root — typically the first fork block's
//! previous hash). If a representative sends votes for two different blocks in
//! the same election it constitutes *equivocation* — evidence of either
//! malicious intent or a compromised key.
//!
//! Equivocating representatives are penalized by having their votes ignored
//! for a configurable duration. The equivocation proof is stored so it can be
//! broadcast to other nodes.

use std::collections::HashMap;

use burst_types::{BlockHash, WalletAddress};

/// Cryptographic proof that a representative voted for two different blocks
/// in the same election.
#[derive(Clone, Debug)]
pub struct EquivocationProof {
    /// The representative that equivocated.
    pub representative: WalletAddress,
    /// First block the representative voted for.
    pub block_a: BlockHash,
    /// Second (conflicting) block the representative voted for.
    pub block_b: BlockHash,
    /// The election root (identifies the election/fork).
    pub election_root: BlockHash,
    /// Timestamp when the equivocation was detected (seconds since epoch).
    pub detected_at: u64,
}

/// Detects when a representative votes for conflicting blocks in the same election.
pub struct EquivocationDetector {
    /// (representative, election_root) → first block hash they voted for.
    votes: HashMap<(WalletAddress, BlockHash), BlockHash>,
    /// All detected equivocation proofs.
    proofs: Vec<EquivocationProof>,
    /// How long (seconds) a penalized representative's votes are ignored.
    penalty_duration_secs: u64,
    /// Representative → penalty expiry timestamp.
    penalties: HashMap<WalletAddress, u64>,
}

impl EquivocationDetector {
    /// Create a new detector with the given penalty duration (in seconds).
    pub fn new(penalty_duration_secs: u64) -> Self {
        Self {
            votes: HashMap::new(),
            proofs: Vec::new(),
            penalty_duration_secs,
            penalties: HashMap::new(),
        }
    }

    /// Record a vote from a representative in an election.
    ///
    /// Returns `Some(EquivocationProof)` if this vote conflicts with a
    /// previously recorded vote from the same representative in the same
    /// election. Returns `None` otherwise.
    pub fn record_vote(
        &mut self,
        rep: &WalletAddress,
        election_root: &BlockHash,
        voted_for: &BlockHash,
        now: u64,
    ) -> Option<EquivocationProof> {
        let key = (rep.clone(), *election_root);

        match self.votes.get(&key) {
            Some(existing) if existing != voted_for => {
                let proof = EquivocationProof {
                    representative: rep.clone(),
                    block_a: *existing,
                    block_b: *voted_for,
                    election_root: *election_root,
                    detected_at: now,
                };
                self.proofs.push(proof.clone());
                self.penalties
                    .insert(rep.clone(), now + self.penalty_duration_secs);
                Some(proof)
            }
            Some(_) => {
                // Duplicate vote for the same block — not equivocation.
                None
            }
            None => {
                self.votes.insert(key, *voted_for);
                None
            }
        }
    }

    /// Check whether a representative is currently penalized.
    pub fn is_penalized(&self, rep: &WalletAddress, now: u64) -> bool {
        self.penalties
            .get(rep)
            .map_or(false, |&expires| now < expires)
    }

    /// Return all equivocation proofs collected so far.
    pub fn proofs(&self) -> &[EquivocationProof] {
        &self.proofs
    }

    /// Remove expired penalties.
    pub fn prune_penalties(&mut self, now: u64) {
        self.penalties.retain(|_, &mut expires| now < expires);
    }

    /// Number of currently tracked vote entries.
    pub fn tracked_votes(&self) -> usize {
        self.votes.len()
    }

    /// Number of active penalties.
    pub fn active_penalties(&self) -> usize {
        self.penalties.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, WalletAddress};

    fn rep(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn hash(val: u8) -> BlockHash {
        BlockHash::new([val; 32])
    }

    #[test]
    fn test_no_equivocation_on_first_vote() {
        let mut det = EquivocationDetector::new(3600);
        let result = det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        assert!(result.is_none());
    }

    #[test]
    fn test_duplicate_vote_no_equivocation() {
        let mut det = EquivocationDetector::new(3600);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = det.record_vote(&rep("alice"), &hash(1), &hash(10), 1001);
        assert!(result.is_none());
        assert!(det.proofs().is_empty());
    }

    #[test]
    fn test_equivocation_detected() {
        let mut det = EquivocationDetector::new(3600);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let proof = det
            .record_vote(&rep("alice"), &hash(1), &hash(20), 1001)
            .expect("should detect equivocation");

        assert_eq!(proof.representative, rep("alice"));
        assert_eq!(proof.block_a, hash(10));
        assert_eq!(proof.block_b, hash(20));
        assert_eq!(proof.election_root, hash(1));
        assert_eq!(det.proofs().len(), 1);
    }

    #[test]
    fn test_penalty_applied_on_equivocation() {
        let mut det = EquivocationDetector::new(3600);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        det.record_vote(&rep("alice"), &hash(1), &hash(20), 1000);

        assert!(det.is_penalized(&rep("alice"), 1000));
        assert!(det.is_penalized(&rep("alice"), 4599));
        assert!(!det.is_penalized(&rep("alice"), 4600));
    }

    #[test]
    fn test_different_elections_no_equivocation() {
        let mut det = EquivocationDetector::new(3600);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = det.record_vote(&rep("alice"), &hash(2), &hash(20), 1001);
        assert!(result.is_none());
    }

    #[test]
    fn test_different_reps_no_equivocation() {
        let mut det = EquivocationDetector::new(3600);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = det.record_vote(&rep("bob"), &hash(1), &hash(20), 1001);
        assert!(result.is_none());
    }

    #[test]
    fn test_prune_expired_penalties() {
        let mut det = EquivocationDetector::new(100);
        det.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        det.record_vote(&rep("alice"), &hash(1), &hash(20), 1000);

        assert_eq!(det.active_penalties(), 1);
        det.prune_penalties(1100); // penalty expires at 1100
        assert_eq!(det.active_penalties(), 0);
    }

    #[test]
    fn test_non_penalized_rep() {
        let det = EquivocationDetector::new(3600);
        assert!(!det.is_penalized(&rep("bob"), 1000));
    }
}
