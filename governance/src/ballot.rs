//! Prepare a reproducible voting result from replay-derived inputs.
//! A result commitment is not a certificate and grants no activation authority.

use crate::{
    DelegationEngine, GovernanceEngine, GovernanceError, GovernancePhase, Proposal, ProposalContent,
};
use burst_types::{BlockHash, ProtocolParams, Timestamp, TxHash, WalletAddress};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
struct BallotReceipt {
    proposal: TxHash,
    history: BlockHash,
    cutoff: Timestamp,
    parameters: BlockHash,
    electorate: BlockHash,
    eligible: u32,
    yea: u32,
    nay: u32,
    abstain: u32,
    phase: GovernancePhase,
    activation_at: Option<Timestamp>,
}

#[derive(Clone, Debug)]
pub struct PreparedBallot {
    receipt: BallotReceipt,
    next: Proposal,
}

impl PreparedBallot {
    /// Close paid support that failed the proposal threshold, or a withdrawal.
    /// This receipt does not authorize voting without enough endorsements.
    pub fn close_unvoted(
        proposal: &Proposal,
        history: BlockHash,
        cutoff: Timestamp,
        params: &ProtocolParams,
        timing: crate::GovernanceTiming,
    ) -> Result<Self, GovernanceError> {
        let due = timing.counting_at(timing.proposal_end(proposal.created_at)?)?;
        if history.is_zero()
            || proposal.hash.is_zero()
            || cutoff != due
            || !matches!(
                proposal.phase,
                GovernancePhase::Proposal | GovernancePhase::Withdrawn
            )
            || (proposal.phase == GovernancePhase::Proposal
                && proposal.endorsement_count >= params.governance_proposal_endorsements)
            || (proposal.endorsement_count == 0 && params.governance_proposal_endorsements != 0)
        {
            return Err(GovernanceError::Other(
                "invalid unvoted proposal closure".into(),
            ));
        }
        let mut next = proposal.clone();
        next.activation_at = None;
        if next.phase == GovernancePhase::Proposal {
            next.phase = GovernancePhase::Rejected;
        }
        let receipt = BallotReceipt {
            proposal: proposal.hash,
            history,
            cutoff,
            parameters: params.params_hash(),
            electorate: BlockHash::ZERO,
            eligible: 0,
            yea: 0,
            nay: 0,
            abstain: 0,
            phase: next.phase.clone(),
            activation_at: None,
        };
        Ok(Self { receipt, next })
    }

    pub fn next_proposal(&self) -> &Proposal {
        &self.next
    }

    pub fn commitment(&self) -> Result<BlockHash, GovernanceError> {
        let bytes = bincode::serialize(&self.receipt)
            .map_err(|error| GovernanceError::Other(error.to_string()))?;
        Ok(BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/governance-ballot/v1\0",
            &bytes,
        ])))
    }
}

/// The cut closes voting after the propagation buffer. Checked arithmetic
/// refuses a deadline that cannot be represented instead of saturating it.
pub fn counting_time(
    proposal: &Proposal,
    params: &ProtocolParams,
) -> Result<Timestamp, GovernanceError> {
    let start = proposal
        .exploration_started_at
        .ok_or_else(|| GovernanceError::Other("voting start is missing".into()))?;
    counting_time_from(
        start,
        matches!(proposal.content, ProposalContent::Emergency { .. }),
        params,
    )
}

pub fn counting_time_from(
    start: Timestamp,
    emergency: bool,
    params: &ProtocolParams,
) -> Result<Timestamp, GovernanceError> {
    let duration = if emergency {
        params.emergency_voting_duration_secs
    } else {
        params.governance_exploration_duration_secs
    };
    start
        .as_secs()
        .checked_add(duration)
        .and_then(|end| end.checked_add(params.governance_propagation_buffer_secs))
        .map(Timestamp::new)
        .ok_or_else(|| GovernanceError::Other("voting deadline overflows".into()))
}

impl GovernanceEngine {
    /// Inputs must come from replay of the same closed history, including
    /// verified eligibility and delegation. This method does not authenticate
    /// that history, mutate the engine or authorize publishing the result.
    pub fn prepare_ballot_cut(
        &self,
        proposal: &TxHash,
        history: BlockHash,
        cutoff: Timestamp,
        params: &ProtocolParams,
        delegation: &DelegationEngine,
        verified: &[WalletAddress],
    ) -> Result<PreparedBallot, GovernanceError> {
        let current = self
            .get_proposal(proposal)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal.to_string()))?;
        if current.phase != GovernancePhase::Exploration {
            return Err(GovernanceError::WrongPhase);
        }
        if history.is_zero() || proposal.is_zero() || self.counting_time(current, params)? != cutoff
        {
            return Err(GovernanceError::Other(
                "ballot requires its exact nonzero history and counting cut".into(),
            ));
        }
        let eligible = u32::try_from(verified.len())
            .map_err(|_| GovernanceError::Other("electorate exceeds supported size".into()))?;
        let mut keys = BTreeSet::new();
        for address in verified {
            let key = burst_crypto::decode_address(address.as_str())
                .ok_or_else(|| GovernanceError::Other("invalid electorate address".into()))?;
            if !keys.insert(key) {
                return Err(GovernanceError::Other("duplicate electorate member".into()));
            }
        }
        let mut electorate = b"BURST/governance-electorate/v1\0".to_vec();
        electorate.extend_from_slice(&eligible.to_be_bytes());
        for key in keys {
            electorate.extend_from_slice(&key);
        }
        let mut next = current.clone();
        match self.try_advance(&mut next, cutoff, params, Some((delegation, verified))) {
            Ok(GovernancePhase::Cooldown | GovernancePhase::Activation) => (),
            Err(
                GovernanceError::QuorumNotMet { .. } | GovernanceError::SupermajorityNotMet { .. },
            ) if next.phase == GovernancePhase::Rejected => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(GovernanceError::Other("unexpected ballot outcome".into())),
        }
        if matches!(
            next.phase,
            GovernancePhase::Cooldown | GovernancePhase::Activation
        ) {
            let cooldown = if next.phase == GovernancePhase::Cooldown {
                params.governance_cooldown_duration_secs
            } else {
                0
            };
            next.activation_at = Some(Timestamp::new(
                cutoff
                    .as_secs()
                    .checked_add(cooldown)
                    .and_then(|at| at.checked_add(params.governance_propagation_buffer_secs))
                    .ok_or_else(|| {
                        GovernanceError::Other("activation timestamp overflows".into())
                    })?,
            ));
        }
        let receipt = BallotReceipt {
            proposal: *proposal,
            history,
            cutoff,
            parameters: params.params_hash(),
            electorate: BlockHash::new(burst_crypto::blake2b_256(&electorate)),
            eligible,
            yea: next.exploration_votes_yea,
            nay: next.exploration_votes_nay,
            abstain: next.exploration_votes_abstain,
            phase: next.phase.clone(),
            activation_at: next.activation_at,
        };
        Ok(PreparedBallot { receipt, next })
    }
}

#[cfg(test)]
mod tests;
