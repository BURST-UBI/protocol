use super::*;
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_types::{BlockHash, Timestamp, WalletAddress};

/// Engines reconstructed by replay and checked against the staged database.
/// Publish them only after commit and force-sync, while holding the locks for
/// runtime authority, engines and admission state.
pub struct RecoveredRuntime {
    pub brn: burst_brn::BrnEngine,
    pub trst: burst_trst::TrstEngine,
    pub verification: burst_verification::VerificationOrchestrator,
    pub governance: burst_governance::GovernanceEngine,
    pub delegation: burst_governance::DelegationEngine,
    pub constitution: burst_consti::ConstiEngine,
    pub params: ProtocolParams,
    pub anchor: CheckpointAnchor,
    pub ledger_counts: (u64, u64, u64),
    pub frontiers: Vec<(WalletAddress, BlockHash)>,
    pub cache_at: Timestamp,
}

impl super::economics::AppliedEconomics {
    pub(super) fn into_runtime(
        mut self,
        transaction: &LmdbWriteTransaction<'_>,
        anchor: CheckpointAnchor,
        cache_at: Timestamp,
    ) -> Result<RecoveredRuntime, StoreError> {
        let staged_trst = self.verify_runtime_state(transaction)?;
        if self.state.payment_epoch != anchor.hash() || cache_at.as_secs() < anchor.cutoff() {
            return Err(invalid(
                "recovery runtime has a different epoch or stale clock",
            ));
        }
        self.state
            .trst
            .set_expiry_period(self.state.params.trst_expiry_secs, cache_at)
            .map_err(invalid)?;
        if !staged_trst.wallet_snapshot_eq(&self.state.trst)
            || staged_trst.merger_graph != self.state.trst.merger_graph
        {
            return Err(invalid("recovery runtime differs from staged TRST caches"));
        }
        drop(staged_trst);
        let mut frontiers = Vec::new();
        for actual in transaction.account_entries()? {
            let actual = actual?;
            let mut expected = self
                .state
                .accounts
                .get(&actual.address)
                .ok_or_else(|| invalid("recovery runtime account is missing"))?
                .clone();
            expected.confirmation_height = actual.confirmation_height;
            if self.state.trst.get_portfolio(&expected.address).is_some() {
                crate::ledger_updater::update_token_counters(
                    &mut expected,
                    &self.state.trst,
                    cache_at,
                )
                .map_err(invalid)?;
            } else {
                expected.expired_trst = 0;
                expected.revoked_trst = 0;
            }
            if actual != expected {
                return Err(invalid(
                    "recovery runtime differs from staged account caches",
                ));
            }
            if actual.block_count > 0 {
                frontiers.push((actual.address, actual.head));
            }
        }
        let governance = self
            .state
            .governance
            .ok_or_else(|| invalid("recovery runtime lacks governance state"))?;
        frontiers.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        Ok(RecoveredRuntime {
            brn: self.state.brn,
            trst: self.state.trst,
            verification: self.state.verification,
            governance: governance.governance,
            delegation: governance.delegation,
            constitution: governance.constitution,
            params: self.state.params,
            anchor,
            ledger_counts: transaction.ledger_counts()?,
            frontiers,
            cache_at,
        })
    }
}
