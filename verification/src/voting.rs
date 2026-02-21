//! Verification voting — verifiers cast votes on wallet legitimacy.

use crate::error::VerificationError;
use crate::state::{VerificationPhase, VerificationState, VerifierVote};
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
    /// If the threshold is not reached but revotes remain, returns `Revote`.
    /// Once `max_revotes` is exhausted, returns `Failed`.
    pub fn tally(
        &self,
        state: &VerificationState,
        threshold_bps: u32,
        max_revotes: u32,
    ) -> VotingOutcome {
        let total = state.votes.len() as u32;
        if total == 0 {
            return VotingOutcome::Revote;
        }
        let legitimate = state
            .votes
            .iter()
            .filter(|v| v.vote == Vote::Legitimate)
            .count() as u32;
        let percentage_bps = (legitimate * 10_000) / total;

        if percentage_bps >= threshold_bps {
            VotingOutcome::Verified
        } else if state.revote_count < max_revotes {
            VotingOutcome::Revote
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
        state
            .votes
            .iter()
            .filter(|v| {
                if outcome_was_legitimate {
                    v.vote != Vote::Legitimate
                } else {
                    v.vote == Vote::Legitimate
                }
            })
            .collect()
    }

    /// Apply timeout for absent verifiers — those who were selected but
    /// haven't voted by the deadline. Their vote defaults to Neither.
    ///
    /// Returns the number of absent verifiers whose votes were set to Neither.
    pub fn apply_timeout_defaults(&self, state: &mut VerificationState, now: Timestamp) -> u32 {
        let mut absent_count = 0u32;
        let voted_verifiers: std::collections::HashSet<&WalletAddress> =
            state.votes.iter().map(|v| &v.verifier).collect();

        let absent: Vec<WalletAddress> = state
            .selected_verifiers
            .iter()
            .filter(|v| !voted_verifiers.contains(v))
            .cloned()
            .collect();

        for verifier in absent {
            state.votes.push(VerifierVote {
                verifier,
                vote: Vote::Neither,
                stake_amount: 0,
                timestamp: now,
            });
            absent_count += 1;
        }

        absent_count
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
        state
            .excluded_verifiers
            .extend(state.selected_verifiers.drain(..));
        state.votes.clear();
        state.revote_count += 1;
        state.phase = VerificationPhase::Voting;
        Ok(())
    }
}

/// Action returned when a Neither-vote penalty is applied.
#[derive(Clone, Debug)]
pub struct NeitherPenaltyAction {
    /// The verifier being penalized.
    pub verifier: WalletAddress,
    /// Timestamp (seconds) until which the verifier is excluded from selection.
    pub cooldown_until: u64,
    /// Whether pending verification rewards are forfeited.
    pub forfeited_rewards: bool,
}

/// Tracks per-verifier Neither vote history for penalty enforcement.
///
/// The whitepaper states: "Voting Neither excessively incurs penalties."
/// Specifically, if a verifier votes Neither on more than 50% of their
/// assigned verifications in a rolling window, they are penalized.
pub struct NeitherVoteTracker {
    /// Per-verifier vote history: (total_assignments, neither_count)
    history: std::collections::HashMap<String, (u32, u32)>,
    /// Penalty threshold in basis points (5000 = 50%)
    penalty_threshold_bps: u32,
}

impl NeitherVoteTracker {
    /// Create a new tracker with the given penalty threshold.
    pub fn new(penalty_threshold_bps: u32) -> Self {
        Self {
            history: std::collections::HashMap::new(),
            penalty_threshold_bps,
        }
    }

    /// Record a vote for a verifier.
    pub fn record_vote(&mut self, verifier: &WalletAddress, vote: Vote) {
        let entry = self.history.entry(verifier.to_string()).or_insert((0, 0));
        entry.0 += 1;
        if vote == Vote::Neither {
            entry.1 += 1;
        }
    }

    /// Check if a verifier has exceeded the Neither vote penalty threshold.
    pub fn is_penalized(&self, verifier: &WalletAddress) -> bool {
        match self.history.get(verifier.as_str()) {
            Some((total, neither)) if *total > 0 => {
                let neither_bps = (*neither as u64 * 10_000) / (*total as u64);
                neither_bps > self.penalty_threshold_bps as u64
            }
            _ => false,
        }
    }

    /// Get the Neither vote ratio for a verifier in basis points.
    pub fn neither_ratio_bps(&self, verifier: &WalletAddress) -> u32 {
        match self.history.get(verifier.as_str()) {
            Some((total, neither)) if *total > 0 => {
                ((*neither as u64 * 10_000) / (*total as u64)) as u32
            }
            _ => 0,
        }
    }

    /// Get the total number of assignments for a verifier.
    pub fn total_assignments(&self, verifier: &WalletAddress) -> u32 {
        self.history
            .get(verifier.as_str())
            .map(|(t, _)| *t)
            .unwrap_or(0)
    }

    /// Get the total number of Neither votes for a verifier.
    pub fn neither_count(&self, verifier: &WalletAddress) -> u32 {
        self.history
            .get(verifier.as_str())
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    /// Apply a penalty for excessive Neither voting.
    ///
    /// Resets the vote history and returns a penalty action describing
    /// the cooldown period and reward forfeiture.
    pub fn apply_neither_penalty(
        &mut self,
        verifier: &WalletAddress,
        current_time_secs: u64,
        cooldown_secs: u64,
    ) -> NeitherPenaltyAction {
        self.reset(verifier);
        NeitherPenaltyAction {
            verifier: verifier.clone(),
            cooldown_until: current_time_secs + cooldown_secs,
            forfeited_rewards: true,
        }
    }

    /// Reset the rolling window for a verifier (e.g., after penalty is applied).
    pub fn reset(&mut self, verifier: &WalletAddress) {
        self.history.remove(verifier.as_str());
    }

    /// Number of tracked verifiers.
    pub fn tracked_count(&self) -> usize {
        self.history.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(s: &str) -> WalletAddress {
        WalletAddress::new(format!(
            "brst_{}",
            s.repeat(60 / s.len() + 1)[..60].to_string()
        ))
    }

    #[test]
    fn neither_tracker_no_votes_not_penalized() {
        let tracker = NeitherVoteTracker::new(5000);
        assert!(!tracker.is_penalized(&test_addr("v1")));
        assert_eq!(tracker.neither_ratio_bps(&test_addr("v1")), 0);
    }

    #[test]
    fn neither_tracker_below_threshold() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Legitimate);
        tracker.record_vote(&v, Vote::Legitimate);
        tracker.record_vote(&v, Vote::Neither);
        // 1/3 = 3333 bps < 5000
        assert!(!tracker.is_penalized(&v));
    }

    #[test]
    fn neither_tracker_above_threshold() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Legitimate);
        // 2/3 = 6666 bps > 5000
        assert!(tracker.is_penalized(&v));
    }

    #[test]
    fn neither_tracker_exact_threshold_not_penalized() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Legitimate);
        // 1/2 = 5000 bps = 5000 (not exceeded, equal)
        assert!(!tracker.is_penalized(&v));
    }

    #[test]
    fn neither_tracker_reset_clears_history() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Neither);
        assert!(tracker.is_penalized(&v));
        tracker.reset(&v);
        assert!(!tracker.is_penalized(&v));
        assert_eq!(tracker.total_assignments(&v), 0);
    }

    #[test]
    fn neither_tracker_multiple_verifiers() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v1 = test_addr("v1");
        let v2 = test_addr("v2");
        tracker.record_vote(&v1, Vote::Neither);
        tracker.record_vote(&v1, Vote::Neither);
        tracker.record_vote(&v2, Vote::Legitimate);
        tracker.record_vote(&v2, Vote::Legitimate);
        assert!(tracker.is_penalized(&v1));
        assert!(!tracker.is_penalized(&v2));
        assert_eq!(tracker.tracked_count(), 2);
    }
}
