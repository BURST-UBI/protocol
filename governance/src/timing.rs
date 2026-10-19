use crate::{GovernanceError, ProposalContent};
use burst_types::{ProtocolParams, Timestamp};
use serde::{Deserialize, Serialize};

/// Proposal and voting windows fixed by the rules at submission. A ballot
/// certificate separately fixes cooldown and activation under its counting rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceTiming {
    pub proposal_secs: u64,
    pub voting_secs: u64,
    pub buffer_secs: u64,
}

impl GovernanceTiming {
    pub fn new(params: &ProtocolParams, content: &ProposalContent) -> Self {
        Self {
            proposal_secs: params.governance_proposal_duration_secs,
            voting_secs: if matches!(content, ProposalContent::Emergency { .. }) {
                params.emergency_voting_duration_secs
            } else {
                params.governance_exploration_duration_secs
            },
            buffer_secs: params.governance_propagation_buffer_secs,
        }
    }

    fn after(at: Timestamp, duration: u64) -> Result<Timestamp, GovernanceError> {
        at.as_secs()
            .checked_add(duration)
            .map(Timestamp::new)
            .ok_or_else(|| GovernanceError::Other("governance deadline overflows".into()))
    }

    pub fn proposal_end(self, created: Timestamp) -> Result<Timestamp, GovernanceError> {
        Self::after(created, self.proposal_secs)
    }

    pub fn voting_end(self, started: Timestamp) -> Result<Timestamp, GovernanceError> {
        Self::after(started, self.voting_secs)
    }

    pub fn counting_at(self, started: Timestamp) -> Result<Timestamp, GovernanceError> {
        Self::after(self.voting_end(started)?, self.buffer_secs)
    }
}
