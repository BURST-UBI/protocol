//! Verification voting — verifiers cast votes on wallet legitimacy.

use crate::error::VerificationError;
use crate::state::{VerificationState, VerifierVote, VerificationPhase};
use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// A verifier's vote on a wallet's humanity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    /// The wallet holder is a unique human (requires BRN stake).
    Legitimate,
    /// The wallet holder is not a unique human (requires BRN stake).
    Illegitimate,
    /// Abstain — counts as illegitimate but requires no stake.
    /// Voting Neither excessively incurs penalties.
    Neither,
}

/// The outcome of tallying verification votes.
#[derive(Clone, Debug)]
pub enum VotingOutcome {
    /// Verification passed (≥ threshold voted Legitimate).
    Verified,
    /// Verification failed (< threshold voted Legitimate).
    Failed,
    /// Need a revote (inconclusive or not enough votes).
    Revote,
}

/// Engine for managing verification votes.
pub struct VerificationVoting;

impl VerificationVoting {
    /// Cast a vote as a selected verifier.
    pub fn cast_vote(
        &self,
        state: &mut VerificationState,
        verifier: WalletAddress,
        vote: Vote,
        stake_amount: u128,
        now: Timestamp,
    ) -> Result<(), VerificationError> {
        if !state.selected_verifiers.contains(&verifier) {
            return Err(VerificationError::NotSelected(verifier.to_string()));
        }
        if state.votes.iter().any(|v| v.verifier == verifier) {
            return Err(VerificationError::AlreadyVoted(verifier.to_string()));
        }
        state.votes.push(VerifierVote {
            verifier,
            vote,
            stake_amount,
            timestamp: now,
        });
        Ok(())
    }

    /// Tally votes and determine the outcome.
    ///
    /// Threshold is in basis points (e.g., 9000 = 90%).
    pub fn tally(&self, state: &VerificationState, threshold_bps: u32) -> VotingOutcome {
        let total = state.votes.len() as u32;
        if total == 0 {
            return VotingOutcome::Revote;
        }
        let legitimate = state.votes.iter().filter(|v| v.vote == Vote::Legitimate).count() as u32;
        let percentage_bps = (legitimate * 10_000) / total;

        if percentage_bps >= threshold_bps {
            VotingOutcome::Verified
        } else {
            VotingOutcome::Failed
        }
    }

    /// Get the verifiers who voted against the outcome (losers forfeit stakes).
    pub fn get_dissenters<'a>(
        &self,
        state: &'a VerificationState,
        outcome_was_legitimate: bool,
    ) -> Vec<&'a VerifierVote> {
        state.votes.iter().filter(|v| {
            if outcome_was_legitimate {
                v.vote != Vote::Legitimate
            } else {
                v.vote == Vote::Legitimate
            }
        }).collect()
    }

    /// Transition to a revote with new verifiers.
    pub fn initiate_revote(
        &self,
        state: &mut VerificationState,
        max_revotes: u32,
    ) -> Result<(), VerificationError> {
        if state.revote_count >= max_revotes {
            return Err(VerificationError::MaxRevotesExceeded(max_revotes));
        }
        state.revote_count += 1;
        state.votes.clear();
        state.selected_verifiers.clear();
        state.phase = VerificationPhase::Voting;
        Ok(())
    }
}
