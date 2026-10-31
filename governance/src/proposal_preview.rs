use crate::{DelegationEngine, GovernanceEngine, GovernancePhase, Proposal};
use burst_types::{ProtocolParams, Timestamp, WalletAddress};

impl GovernanceEngine {
    /// Project one selected proposal without cloning unrelated governance state.
    /// The caller must select from this engine so its stored timing and ballots
    /// describe the same read cut.
    pub fn project_proposal_at(
        &self,
        original: &Proposal,
        now: Timestamp,
        params: &ProtocolParams,
        delegation: &DelegationEngine,
        verified: &[WalletAddress],
    ) -> Proposal {
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
        next
    }

    /// Whether this display projection can reach a tally at `now`. Invalid
    /// timing and failed support transitions stop projection before counting.
    pub fn projection_requires_electorate(
        &self,
        original: &Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> bool {
        let mut opened;
        let proposal = if original.phase == GovernancePhase::Proposal {
            opened = original.clone();
            if self.try_advance(&mut opened, now, params, None).is_err() {
                return false;
            }
            &opened
        } else {
            original
        };
        proposal.phase == GovernancePhase::Exploration
            && proposal.exploration_started_at.is_some_and(|started| {
                self.timing(proposal, params)
                    .and_then(|timing| timing.counting_at(started))
                    .is_ok_and(|at| now >= at)
            })
    }

    /// Exclusive deadline for reusing an already projected proposal with the
    /// same engine, parameters, delegation and electorate. None means time alone
    /// cannot change it. Invalid or already-due active phases are not reusable.
    /// Callers must also reject timestamps before the original projection time.
    pub fn projection_valid_until(
        &self,
        projected: &Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Option<Timestamp> {
        let deadline = match projected.phase {
            GovernancePhase::Proposal => self
                .timing(projected, params)
                .and_then(|timing| timing.proposal_end(projected.created_at))
                .ok(),
            GovernancePhase::Exploration => projected.exploration_started_at.and_then(|started| {
                self.timing(projected, params)
                    .and_then(|timing| timing.counting_at(started))
                    .ok()
            }),
            GovernancePhase::Cooldown => projected.activation_at.or_else(|| {
                projected.cooldown_started_at.map(|started| {
                    Timestamp::new(
                        started
                            .as_secs()
                            .saturating_add(params.governance_cooldown_duration_secs),
                    )
                })
            }),
            _ => return None,
        };
        Some(deadline.filter(|at| *at > now).unwrap_or(now))
    }
}
