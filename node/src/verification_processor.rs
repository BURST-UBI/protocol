//! End-to-end verification orchestrator and verifier pool management.
//!
//! Coordinates the full UHV (Unique Humanity Verification) flow:
//! 1. Check endorsement threshold
//! 2. Fetch drand randomness
//! 3. Select verifiers from the pool
//! 4. Collect verification votes
//! 5. Determine outcome

use burst_types::WalletAddress;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// VerificationOutcome
// ---------------------------------------------------------------------------

/// The outcome of processing verification votes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// Not enough votes have been cast yet.
    Pending,
    /// The subject wallet was verified as a unique human.
    Verified,
    /// The subject wallet was rejected (not considered a unique human).
    Rejected,
}

// ---------------------------------------------------------------------------
// VerificationProcessor
// ---------------------------------------------------------------------------

/// Orchestrates the end-to-end verification flow.
///
/// The processor is parameterized by protocol-level configuration:
/// - `endorsement_threshold`: minimum endorsements before voting begins
/// - `verifier_count`: how many verifiers to select per verification
/// - `vote_threshold`: fraction of verifiers that must participate (e.g. 0.67)
pub struct VerificationProcessor {
    /// Minimum number of endorsements required to begin verification.
    endorsement_threshold: u32,
    /// Number of verifiers to select from the pool for each verification.
    verifier_count: u32,
    /// Fraction of verifiers that must have voted before the outcome is decided.
    /// For example, 0.67 means at least 67% of selected verifiers must vote.
    vote_threshold: f64,
}

impl VerificationProcessor {
    /// Create a new processor with the given configuration.
    pub fn new(endorsement_threshold: u32, verifier_count: u32, vote_threshold: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&vote_threshold),
            "vote_threshold must be between 0.0 and 1.0"
        );
        Self {
            endorsement_threshold,
            verifier_count,
            vote_threshold,
        }
    }

    /// Check if an account has enough endorsements to proceed to verification.
    pub fn check_endorsements(&self, endorsement_count: u32) -> bool {
        endorsement_count >= self.endorsement_threshold
    }

    /// Return the configured number of verifiers to select.
    pub fn verifier_count(&self) -> u32 {
        self.verifier_count
    }

    /// Process verification votes and determine the outcome.
    ///
    /// The outcome is [`VerificationOutcome::Pending`] until the participation
    /// threshold is met. Once enough votes are in, a simple majority decides
    /// between [`VerificationOutcome::Verified`] and [`VerificationOutcome::Rejected`].
    pub fn process_votes(
        &self,
        votes_for: u32,
        votes_against: u32,
        total_verifiers: u32,
    ) -> VerificationOutcome {
        let total_votes = votes_for + votes_against;
        let required = (total_verifiers as f64 * self.vote_threshold).ceil() as u32;

        if total_votes < required {
            return VerificationOutcome::Pending;
        }

        if votes_for > votes_against {
            VerificationOutcome::Verified
        } else {
            VerificationOutcome::Rejected
        }
    }
}

// ---------------------------------------------------------------------------
// VerifierPool
// ---------------------------------------------------------------------------

/// Tracks which accounts have opted in as verifiers.
///
/// Verifiers must hold a minimum BRN balance (which they stake implicitly).
/// The pool provides the set of eligible addresses used by
/// [`burst_vrf::select_verifiers`] when a new verification round begins.
pub struct VerifierPool {
    /// Set of addresses currently opted in.
    opted_in: HashSet<WalletAddress>,
    /// Minimum BRN balance required to opt in as a verifier.
    min_brn_stake: u128,
}

impl VerifierPool {
    /// Create a new empty pool with the given minimum stake requirement.
    pub fn new(min_brn_stake: u128) -> Self {
        Self {
            opted_in: HashSet::new(),
            min_brn_stake,
        }
    }

    /// Opt in as a verifier. Fails if the BRN balance is below the minimum.
    pub fn opt_in(
        &mut self,
        address: WalletAddress,
        brn_balance: u128,
    ) -> Result<(), String> {
        if brn_balance < self.min_brn_stake {
            return Err(format!(
                "insufficient BRN: have {}, need {}",
                brn_balance, self.min_brn_stake
            ));
        }
        self.opted_in.insert(address);
        Ok(())
    }

    /// Opt out of the verifier pool.
    pub fn opt_out(&mut self, address: &WalletAddress) {
        self.opted_in.remove(address);
    }

    /// Check whether an address is currently opted in.
    pub fn is_verifier(&self, address: &WalletAddress) -> bool {
        self.opted_in.contains(address)
    }

    /// Return all opted-in verifier addresses as a sorted vector.
    ///
    /// The result is sorted to ensure deterministic iteration order across nodes.
    pub fn pool(&self) -> Vec<WalletAddress> {
        let mut addrs: Vec<_> = self.opted_in.iter().cloned().collect();
        addrs.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        addrs
    }

    /// Number of currently opted-in verifiers.
    pub fn count(&self) -> usize {
        self.opted_in.len()
    }

    /// Return the minimum BRN stake requirement.
    pub fn min_stake(&self) -> u128 {
        self.min_brn_stake
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{}", name))
    }

    // -- VerificationProcessor tests --

    #[test]
    fn test_check_endorsements_below_threshold() {
        let proc = VerificationProcessor::new(3, 5, 0.67);
        assert!(!proc.check_endorsements(0));
        assert!(!proc.check_endorsements(2));
    }

    #[test]
    fn test_check_endorsements_meets_threshold() {
        let proc = VerificationProcessor::new(3, 5, 0.67);
        assert!(proc.check_endorsements(3));
        assert!(proc.check_endorsements(10));
    }

    #[test]
    fn test_process_votes_pending() {
        let proc = VerificationProcessor::new(3, 5, 0.67);
        // 5 verifiers, need ceil(5 * 0.67) = 4 votes
        assert_eq!(
            proc.process_votes(1, 1, 5),
            VerificationOutcome::Pending
        );
        assert_eq!(
            proc.process_votes(2, 0, 5),
            VerificationOutcome::Pending
        );
    }

    #[test]
    fn test_process_votes_verified() {
        let proc = VerificationProcessor::new(3, 5, 0.67);
        // 4 votes total, 3 for > 1 against
        assert_eq!(
            proc.process_votes(3, 1, 5),
            VerificationOutcome::Verified
        );
    }

    #[test]
    fn test_process_votes_rejected() {
        let proc = VerificationProcessor::new(3, 5, 0.67);
        // 4 votes total, 1 for < 3 against
        assert_eq!(
            proc.process_votes(1, 3, 5),
            VerificationOutcome::Rejected
        );
    }

    #[test]
    fn test_process_votes_tie_rejected() {
        let proc = VerificationProcessor::new(3, 5, 0.5);
        // Tie: votes_for == votes_against → not strictly greater, so rejected
        assert_eq!(
            proc.process_votes(2, 2, 5),
            VerificationOutcome::Rejected
        );
    }

    #[test]
    fn test_process_votes_all_for() {
        let proc = VerificationProcessor::new(1, 3, 1.0);
        assert_eq!(
            proc.process_votes(3, 0, 3),
            VerificationOutcome::Verified
        );
    }

    // -- VerifierPool tests --

    #[test]
    fn test_opt_in_success() {
        let mut pool = VerifierPool::new(100);
        assert!(pool.opt_in(addr("alice"), 200).is_ok());
        assert!(pool.is_verifier(&addr("alice")));
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn test_opt_in_insufficient_brn() {
        let mut pool = VerifierPool::new(100);
        let result = pool.opt_in(addr("alice"), 50);
        assert!(result.is_err());
        assert!(!pool.is_verifier(&addr("alice")));
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn test_opt_in_exact_minimum() {
        let mut pool = VerifierPool::new(100);
        assert!(pool.opt_in(addr("alice"), 100).is_ok());
        assert!(pool.is_verifier(&addr("alice")));
    }

    #[test]
    fn test_opt_out() {
        let mut pool = VerifierPool::new(100);
        pool.opt_in(addr("alice"), 200).unwrap();
        pool.opt_in(addr("bob"), 300).unwrap();
        assert_eq!(pool.count(), 2);

        pool.opt_out(&addr("alice"));
        assert!(!pool.is_verifier(&addr("alice")));
        assert!(pool.is_verifier(&addr("bob")));
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn test_opt_out_nonexistent_is_noop() {
        let mut pool = VerifierPool::new(100);
        pool.opt_out(&addr("nobody")); // should not panic
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn test_pool_returns_sorted_addresses() {
        let mut pool = VerifierPool::new(0);
        pool.opt_in(addr("charlie"), 0).unwrap();
        pool.opt_in(addr("alice"), 0).unwrap();
        pool.opt_in(addr("bob"), 0).unwrap();

        let addrs = pool.pool();
        assert_eq!(addrs.len(), 3);
        assert_eq!(addrs[0].as_str(), "brst_alice");
        assert_eq!(addrs[1].as_str(), "brst_bob");
        assert_eq!(addrs[2].as_str(), "brst_charlie");
    }

    #[test]
    fn test_duplicate_opt_in() {
        let mut pool = VerifierPool::new(0);
        pool.opt_in(addr("alice"), 100).unwrap();
        pool.opt_in(addr("alice"), 200).unwrap(); // idempotent
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn test_min_stake() {
        let pool = VerifierPool::new(500);
        assert_eq!(pool.min_stake(), 500);
    }

    // -- Integration: VerifierPool + select_verifiers --

    #[test]
    fn test_pool_with_verifier_selection() {
        let mut pool = VerifierPool::new(10);
        for i in 0..10 {
            pool.opt_in(addr(&format!("user{}", i)), 100).unwrap();
        }

        let addrs = pool.pool();
        let selected = burst_vrf::select_verifiers(b"some_randomness", &addrs, 3);
        assert_eq!(selected.len(), 3);

        // All selected must be in the pool
        for s in &selected {
            assert!(pool.is_verifier(s));
        }
    }
}
