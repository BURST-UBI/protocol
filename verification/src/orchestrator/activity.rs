//! Read-only activity view; no pending events, appeal or selection history.
use super::*;

pub struct ActivitySnapshot {
    states: crate::tracked_map::TrackedMap<WalletAddress, VerificationState>,
    challenges: crate::tracked_map::TrackedMap<WalletAddress, Challenge>,
}

impl VerificationOrchestrator {
    /// Shares two persistent roots so callers can release the live engine lock
    /// before scanning. The view retains replaced values until it is dropped.
    pub fn activity_snapshot(&self) -> ActivitySnapshot {
        ActivitySnapshot {
            states: self.states.untracked_clone(),
            challenges: self.active_challenges.untracked_clone(),
        }
    }
}

impl ActivitySnapshot {
    pub fn states(&self) -> impl Iterator<Item = &VerificationState> {
        self.states.values()
    }
    pub fn get_state(&self, wallet: &WalletAddress) -> Option<&VerificationState> {
        self.states.get(wallet)
    }
    pub fn active_challenges(&self) -> impl ExactSizeIterator<Item = &Challenge> {
        self.challenges.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_view_shares_roots_and_preserves_its_cut_after_mutation() {
        let target = WalletAddress::new("brst_activity_target");
        let challenger = WalletAddress::new("brst_activity_challenger");
        let mut engine = VerificationOrchestrator::new();
        engine.initialize_genesis(&target);
        let challenge = engine.challenges.initiate(challenger, target.clone(), 7,
            burst_types::ChallengeReason::Fraud, Timestamp::new(100));
        engine.active_challenges.insert(target.clone(), challenge);
        engine.states.begin_changes();
        let before = engine.activity_snapshot();
        assert!(before.states.ptr_eq(&engine.states));
        assert!(before.challenges.ptr_eq(&engine.active_challenges));
        let encoded = bincode::serialize(before.get_state(&target).unwrap()).unwrap();
        engine.states.get_mut(&target).unwrap().phase = VerificationPhase::Challenged;
        engine.active_challenges.remove(&target);
        engine.pending_events.push(VerificationEvent::WalletUnverified { wallet: target.clone() });
        let after = engine.activity_snapshot();
        assert_eq!(before.states().count(), 1);
        assert_eq!(before.active_challenges().len(), 1);
        assert_eq!(before.active_challenges().next().unwrap().stake_amount, 7);
        assert_eq!(bincode::serialize(before.get_state(&target).unwrap()).unwrap(), encoded);
        assert_eq!(after.get_state(&target).unwrap().phase, VerificationPhase::Challenged);
        assert_eq!(after.active_challenges().len(), 0);
        assert_eq!(engine.pending_events.len(), 1);
    }
}
