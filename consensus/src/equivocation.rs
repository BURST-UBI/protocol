//! Local detection of conflicting votes for one election root.
//! Callers authenticate votes before recording them. Observations retain hashes,
//! not signatures, and cannot serve as transferable cryptographic proofs.
//! This standalone helper is not wired into the node's receive path. It has no
//! final/soft distinction: callers must exclude legitimate soft-vote changes.

use std::collections::HashMap;

use burst_types::{BlockHash, WalletAddress};

/// Local record of conflicting block hashes reported for one representative.
#[derive(Clone, Debug)]
pub struct EquivocationObservation {
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
    /// All detected equivocation observations.
    observations: Vec<EquivocationObservation>,
    /// Duration of the local penalty marker; callers decide whether to enforce it.
    penalty_duration_secs: u64,
    /// Representative → penalty expiry timestamp.
    penalties: HashMap<WalletAddress, u64>,
}

impl EquivocationDetector {
    /// Create a new detector with the given penalty duration (in seconds).
    pub fn new(penalty_duration_secs: u64) -> Self {
        Self {
            votes: HashMap::new(),
            observations: Vec::new(),
            penalty_duration_secs,
            penalties: HashMap::new(),
        }
    }

    /// Record a vote from a representative in an election.
    ///
    /// Returns `Some(EquivocationObservation)` if this vote conflicts with a
    /// previously recorded vote from the same representative in the same
    /// election. Returns `None` otherwise. The first hash remains the baseline:
    /// repeated conflicts append observations and extend the penalty. Vote and
    /// observation history are retained for the lifetime of the detector.
    pub fn record_vote(
        &mut self,
        rep: &WalletAddress,
        election_root: &BlockHash,
        voted_for: &BlockHash,
        now: u64,
    ) -> Option<EquivocationObservation> {
        let key = (rep.clone(), *election_root);

        match self.votes.get(&key) {
            Some(existing) if existing != voted_for => {
                let observation = EquivocationObservation {
                    representative: rep.clone(),
                    block_a: *existing,
                    block_b: *voted_for,
                    election_root: *election_root,
                    detected_at: now,
                };
                self.observations.push(observation.clone());
                self.penalties
                    .insert(rep.clone(), now + self.penalty_duration_secs);
                Some(observation)
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
            .is_some_and(|&expires| now < expires)
    }

    /// Return all equivocation observations collected so far.
    pub fn observations(&self) -> &[EquivocationObservation] {
        &self.observations
    }

    /// Remove expired penalties.
    pub fn prune_penalties(&mut self, now: u64) {
        self.penalties.retain(|_, &mut expires| now < expires);
    }

    /// Number of currently tracked vote entries.
    pub fn tracked_votes(&self) -> usize {
        self.votes.len()
    }

    /// Stored penalties, including expired entries until `prune_penalties` runs.
    pub fn tracked_penalties(&self) -> usize {
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
        let mut detector = EquivocationDetector::new(3600);
        let result = detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        assert!(result.is_none());
    }

    #[test]
    fn test_duplicate_vote_no_equivocation() {
        let mut detector = EquivocationDetector::new(3600);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1001);
        assert!(result.is_none());
        assert!(detector.observations().is_empty());
    }

    #[test]
    fn test_equivocation_detected() {
        let mut detector = EquivocationDetector::new(3600);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let observation = detector
            .record_vote(&rep("alice"), &hash(1), &hash(20), 1001)
            .expect("should detect equivocation");

        assert_eq!(observation.representative, rep("alice"));
        assert_eq!(observation.block_a, hash(10));
        assert_eq!(observation.block_b, hash(20));
        assert_eq!(observation.election_root, hash(1));
        assert_eq!(detector.observations().len(), 1);
    }

    #[test]
    fn test_penalty_applied_on_equivocation() {
        let mut detector = EquivocationDetector::new(3600);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        detector.record_vote(&rep("alice"), &hash(1), &hash(20), 1000);

        assert!(detector.is_penalized(&rep("alice"), 1000));
        assert!(detector.is_penalized(&rep("alice"), 4599));
        assert!(!detector.is_penalized(&rep("alice"), 4600));
    }

    #[test]
    fn test_different_elections_no_equivocation() {
        let mut detector = EquivocationDetector::new(3600);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = detector.record_vote(&rep("alice"), &hash(2), &hash(20), 1001);
        assert!(result.is_none());
    }

    #[test]
    fn test_different_reps_no_equivocation() {
        let mut detector = EquivocationDetector::new(3600);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        let result = detector.record_vote(&rep("bob"), &hash(1), &hash(20), 1001);
        assert!(result.is_none());
    }

    #[test]
    fn test_prune_expired_penalties() {
        let mut detector = EquivocationDetector::new(100);
        detector.record_vote(&rep("alice"), &hash(1), &hash(10), 1000);
        detector.record_vote(&rep("alice"), &hash(1), &hash(20), 1000);

        assert_eq!(detector.tracked_penalties(), 1);
        detector.prune_penalties(1100); // penalty expires at 1100
        assert_eq!(detector.tracked_penalties(), 0);
    }

    #[test]
    fn test_non_penalized_rep() {
        let detector = EquivocationDetector::new(3600);
        assert!(!detector.is_penalized(&rep("bob"), 1000));
    }
}
