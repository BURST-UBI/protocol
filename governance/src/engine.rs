//! Core governance engine — manages proposals through the 4-phase lifecycle.

use crate::error::GovernanceError;
use crate::proposal::{GovernancePhase, Proposal};
use burst_types::{Timestamp, TxHash};

pub struct GovernanceEngine;

impl GovernanceEngine {
    /// Submit a new proposal (enters Proposal phase).
    pub fn submit_proposal(&self, _proposal: Proposal) -> Result<(), GovernanceError> {
        todo!("validate proposer is verified, store proposal")
    }

    /// Endorse a proposal (burn BRN to advance past spam filter).
    pub fn endorse_proposal(
        &self,
        _proposal_hash: &TxHash,
        _brn_burned: u128,
    ) -> Result<(), GovernanceError> {
        todo!("increment endorsement count, check if threshold met to advance to Voting")
    }

    /// Cast a vote on a proposal.
    pub fn cast_vote(
        &self,
        _proposal_hash: &TxHash,
        _voter: &burst_types::WalletAddress,
        _vote: crate::proposal::ProposalContent,
        _now: Timestamp,
    ) -> Result<(), GovernanceError> {
        todo!("validate voter is verified, check phase is Voting, record vote")
    }

    /// Advance a proposal to the next phase if conditions are met.
    pub fn try_advance(
        &self,
        _proposal: &mut Proposal,
        _now: Timestamp,
        _params: &burst_types::ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        todo!("check phase duration, quorum, supermajority, advance accordingly")
    }

    /// Activate a proposal — apply the parameter change to the protocol.
    pub fn activate(
        &self,
        _proposal: &Proposal,
        _params: &mut burst_types::ProtocolParams,
    ) -> Result<(), GovernanceError> {
        todo!("apply parameter change to ProtocolParams")
    }
}
