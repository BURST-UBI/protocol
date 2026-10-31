use super::*;
use crate::verification_commit::ledger::ReplayedLedger;

impl ConfirmedReplay {
    pub(crate) fn upheld_targets(&self) -> Vec<WalletAddress> {
        let mut targets: Vec<_> = self
            .accounts
            .keys()
            .filter(|target| {
                let verdict = self
                    .verification
                    .pending_challenge_verdict(target, &self.params);
                verdict.is_some()
                    && (self.verification.active_appeal(target).is_some()
                        || verdict == Some(burst_verification::ChallengeResult::FraudConfirmed))
            })
            .cloned()
            .collect();
        targets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        targets
    }

    /// Apply the challenge outcome to this replay candidate. Discard the
    /// candidate on error; earlier settlement changes are not rolled back.
    pub(crate) fn apply_upheld(
        &mut self,
        history: &impl LifecycleHistory,
        params: &ProtocolParams,
        target: &WalletAddress,
    ) -> Result<BlockHash, ReplayError> {
        let applied = history.applied_blocks();
        let current = history.first_block()?;
        let mut ledger = ReplayedLedger {
            bootstrap: &mut self.bootstrap_phase,
            bootstrap_threshold: self.params.bootstrap_exit_threshold,
            mutations: self.mutations.as_mut(),
            accounts: &mut self.accounts,
            pending: &mut self.pending,
            metadata: &mut self.lifecycle_metadata,
            applied: &applied,
            current: &current,
        };
        let root = crate::verification_commit::settle_upheld_candidate(
            &mut ledger,
            target,
            self.cutoff,
            params,
            &mut self.verification,
            (&mut self.brn, &mut self.trst),
            self.network,
        )
        .map_err(invalid)?;
        Ok(root)
    }
}
