use crate::{DelegationEngine, GovernanceEngine, GovernancePhase};
use burst_types::{ProtocolParams, Timestamp, TxHash, WalletAddress};

impl GovernanceEngine {
    /// Project elapsed phases for display and candidate preparation. A local
    /// clock and a possibly incomplete electorate cannot close the stored ballot.
    /// This copy neither applies activation effects nor authenticates a result.
    pub fn project_at(
        &self,
        now: Timestamp,
        params: &ProtocolParams,
        delegation: &DelegationEngine,
        verified: &[WalletAddress],
    ) -> Self {
        let mut projected = self.clone();
        for original in self.all_proposals() {
            let mut next = original.clone();
            for _ in 0..3 {
                if !matches!(
                    next.phase,
                    GovernancePhase::Proposal
                        | GovernancePhase::Exploration
                        | GovernancePhase::Cooldown
                ) {
                    break;
                }
                if self
                    .try_advance(&mut next, now, params, Some((delegation, verified)))
                    .is_err()
                {
                    break;
                }
            }
            *projected
                .get_proposal_mut(&original.hash)
                .expect("cloned proposal") = next;
        }
        projected
    }

    pub fn due_activation_hashes(&self, now: Timestamp) -> Vec<TxHash> {
        let mut due: Vec<_> = self
            .all_proposals()
            .filter(|proposal| {
                proposal.phase == GovernancePhase::Activation
                    && proposal.activation_at.is_some_and(|at| at <= now)
            })
            .collect();
        due.sort_by_key(|proposal| {
            (
                proposal.activation_at.map(|at| at.as_secs()),
                *proposal.hash.as_bytes(),
            )
        });
        due.into_iter().map(|proposal| proposal.hash).collect()
    }
}
