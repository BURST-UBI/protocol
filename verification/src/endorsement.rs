//! Endorsement engine — manages the endorsement phase of verification.

use crate::error::VerificationError;
use crate::state::{Endorsement, VerificationState};
use burst_types::{Timestamp, WalletAddress};

pub struct EndorsementEngine;

impl EndorsementEngine {
    /// Submit an endorsement for a target wallet.
    ///
    /// The endorser permanently burns their BRN (handled by BRN engine externally).
    pub fn submit_endorsement(
        &self,
        state: &mut VerificationState,
        endorser: WalletAddress,
        burn_amount: u128,
        now: Timestamp,
    ) -> Result<(), VerificationError> {
        if state.endorsements.iter().any(|e| e.endorser == endorser) {
            return Err(VerificationError::AlreadyEndorsed(endorser.to_string()));
        }
        state.endorsements.push(Endorsement {
            endorser,
            burn_amount,
            timestamp: now,
        });
        Ok(())
    }

    /// Check whether the endorsement threshold has been met.
    pub fn check_threshold(&self, state: &VerificationState, threshold: u32) -> bool {
        state.endorsements.len() as u32 >= threshold
    }

    /// Total BRN burned in endorsements for this wallet.
    pub fn total_burned(&self, state: &VerificationState) -> u128 {
        state.endorsements.iter().map(|e| e.burn_amount).sum()
    }

    /// Invalidate endorsements from a specific endorser (e.g., if they were revoked).
    ///
    /// Returns the number of endorsements removed.
    pub fn invalidate_endorser(
        &self,
        state: &mut VerificationState,
        endorser: &WalletAddress,
    ) -> u32 {
        let before = state.endorsements.len();
        state.endorsements.retain(|e| &e.endorser != endorser);
        (before - state.endorsements.len()) as u32
    }

    /// Check if any endorsers in the current verification state have been revoked.
    /// Returns a list of revoked endorser addresses that need invalidation.
    pub fn find_revoked_endorsers<'a>(
        &self,
        state: &'a VerificationState,
        is_revoked: impl Fn(&WalletAddress) -> bool,
    ) -> Vec<&'a WalletAddress> {
        state
            .endorsements
            .iter()
            .filter(|e| is_revoked(&e.endorser))
            .map(|e| &e.endorser)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::VerificationPhase;
    use std::collections::HashSet;

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(&format!("brst_{s}"))
    }

    fn new_state(target: &str) -> VerificationState {
        VerificationState {
            target: addr(target),
            phase: VerificationPhase::Endorsing,
            endorsements: Vec::new(),
            selected_verifiers: Vec::new(),
            votes: Vec::new(),
            revote_count: 0,
            excluded_verifiers: HashSet::new(),
            started_at: Timestamp::new(0),
        }
    }

    #[test]
    fn submit_single_endorsement() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("e1"), 100, Timestamp::new(1))
            .unwrap();
        assert_eq!(state.endorsements.len(), 1);
        assert_eq!(state.endorsements[0].endorser, addr("e1"));
        assert_eq!(state.endorsements[0].burn_amount, 100);
    }

    #[test]
    fn duplicate_endorsement_rejected() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("e1"), 100, Timestamp::new(1))
            .unwrap();
        let result = engine.submit_endorsement(&mut state, addr("e1"), 200, Timestamp::new(2));
        assert!(result.is_err());
        assert_eq!(state.endorsements.len(), 1);
    }

    #[test]
    fn threshold_check() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        assert!(!engine.check_threshold(&state, 3));

        engine
            .submit_endorsement(&mut state, addr("e1"), 10, Timestamp::new(1))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("e2"), 10, Timestamp::new(2))
            .unwrap();
        assert!(!engine.check_threshold(&state, 3));

        engine
            .submit_endorsement(&mut state, addr("e3"), 10, Timestamp::new(3))
            .unwrap();
        assert!(engine.check_threshold(&state, 3));
    }

    #[test]
    fn total_burned_sums_all() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("e1"), 100, Timestamp::new(1))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("e2"), 250, Timestamp::new(2))
            .unwrap();
        assert_eq!(engine.total_burned(&state), 350);
    }

    #[test]
    fn invalidate_endorser_removes_correct_entries() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("e1"), 100, Timestamp::new(1))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("e2"), 200, Timestamp::new(2))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("e3"), 300, Timestamp::new(3))
            .unwrap();

        let removed = engine.invalidate_endorser(&mut state, &addr("e2"));
        assert_eq!(removed, 1);
        assert_eq!(state.endorsements.len(), 2);
        assert!(state.endorsements.iter().all(|e| e.endorser != addr("e2")));
    }

    #[test]
    fn invalidate_nonexistent_endorser_returns_zero() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("e1"), 100, Timestamp::new(1))
            .unwrap();
        let removed = engine.invalidate_endorser(&mut state, &addr("nobody"));
        assert_eq!(removed, 0);
        assert_eq!(state.endorsements.len(), 1);
    }

    #[test]
    fn find_revoked_endorsers_filters_correctly() {
        let engine = EndorsementEngine;
        let mut state = new_state("target");
        engine
            .submit_endorsement(&mut state, addr("good"), 10, Timestamp::new(1))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("bad1"), 10, Timestamp::new(2))
            .unwrap();
        engine
            .submit_endorsement(&mut state, addr("bad2"), 10, Timestamp::new(3))
            .unwrap();

        let revoked_set: HashSet<WalletAddress> =
            [addr("bad1"), addr("bad2")].into_iter().collect();

        let found = engine.find_revoked_endorsers(&state, |a| revoked_set.contains(a));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|a| revoked_set.contains(a)));
    }
}
