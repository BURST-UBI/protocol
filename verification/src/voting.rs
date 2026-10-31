//! Panel vote counting and rolling Neither-assignment history.

use crate::error::VerificationError;
use crate::state::{VerificationPhase, VerificationState, VerifierVote};
use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// A verifier's judgment under the current enrollment or challenge question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    /// Supports the wallet's eligibility under the panel question; requires BRN stake.
    Legitimate,
    /// Opposes the wallet's eligibility under the panel question; requires BRN stake.
    Illegitimate,
    /// No substantive judgment and no stake. Included in the tally denominator
    /// without supporting acceptance; excessive use can trigger a penalty.
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
#[derive(Clone)]
pub struct VerificationVoting;

impl VerificationVoting {
    /// Record one vote per selected verifier. The caller checks phase, deadline,
    /// signature and stake requirements before calling this helper.
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
        if state.votes.contains_verifier(&verifier) {
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
    /// Once `max_revotes` is exhausted, returns `Failed`. An empty tally always
    /// returns `Revote`. The caller ensures panel completion; this helper counts
    /// only recorded votes and does not check selected membership.
    pub fn tally(
        &self,
        state: &VerificationState,
        threshold_bps: u32,
        max_revotes: u32,
    ) -> VotingOutcome {
        let total = state.votes.len() as u128;
        if total == 0 {
            return VotingOutcome::Revote;
        }
        let legitimate = state
            .votes
            .iter()
            .filter(|v| v.vote == Vote::Legitimate)
            .count() as u128;
        let percentage_bps = (legitimate * 10_000) / total;

        if percentage_bps >= u128::from(threshold_bps) {
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

    /// Add zero-stake Neither votes for selected verifiers who have not voted.
    /// The caller must establish that the agreed deadline has passed; this
    /// helper uses now only as the synthetic vote timestamp.
    /// Returns the number of added votes.
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

    /// Exclude the current panel, clear its votes and increment the attempt.
    /// The caller retains votes needed for settlement and selects the next panel.
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
    /// Reported forfeiture flag. The orchestrator does not consume this field;
    /// setting it here does not cancel pending rewards.
    pub forfeited_rewards: bool,
}

/// Tracks per-verifier Neither vote history for penalty enforcement.
///
/// The governed threshold applies to the retained rolling assignment window.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NeitherVoteTracker {
    pub(crate) history: crate::tracked_map::TrackedMap<String, VoteWindow>,
    pub(crate) penalty_threshold_bps: u32,
    pub(crate) window_assignments: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct VoteWindow {
    votes: std::collections::VecDeque<bool>,
    neither: u32,
}

impl VoteWindow {
    fn trim(&mut self, limit: usize) {
        while self.votes.len() > limit {
            self.neither -= u32::from(self.votes.pop_front().unwrap());
        }
    }
}

/// Old snapshots retained counts but not order. Nonempty history needs replay.
#[derive(Serialize, Deserialize)]
pub(crate) struct LegacyNeitherVoteTracker {
    history: std::collections::HashMap<String, (u32, u32)>,
    penalty_threshold_bps: u32,
}

impl LegacyNeitherVoteTracker {
    pub(crate) fn into_rolling(self) -> Result<NeitherVoteTracker, bincode::Error> {
        if self.history.values().any(|counts| *counts != (0, 0)) {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "old Neither history has no vote order; replay is required".into(),
            )));
        }
        Ok(NeitherVoteTracker::new(self.penalty_threshold_bps))
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            history: Default::default(),
            penalty_threshold_bps: 5000,
        }
    }
}

impl NeitherVoteTracker {
    pub(crate) fn same_rules(&self, other: &Self) -> bool {
        self.penalty_threshold_bps == other.penalty_threshold_bps
            && self.window_assignments == other.window_assignments
    }

    pub(crate) fn same_history(
        &self,
        other: &Self,
        verifier: &WalletAddress,
    ) -> Result<bool, String> {
        crate::orchestrator::snapshot_undo::equivalent(
            self.history.get(verifier.as_str()),
            other.history.get(verifier.as_str()),
        )
    }

    pub(crate) fn rewind(&mut self, before: &Self, after: &Self) -> Result<(), String> {
        use crate::orchestrator::snapshot_undo::{equivalent, rewind_value};
        rewind_value(
            &before.penalty_threshold_bps,
            &after.penalty_threshold_bps,
            &mut self.penalty_threshold_bps,
        )?;
        rewind_value(
            &before.window_assignments,
            &after.window_assignments,
            &mut self.window_assignments,
        )?;
        for key in before
            .history
            .keys()
            .chain(after.history.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            if equivalent(before.history.get(key), after.history.get(key))? {
                continue;
            }
            if !equivalent(self.history.get(key), after.history.get(key))? {
                return Err("verification undo conflicts with a changed Neither window".into());
            }
            match before.history.get(key) {
                Some(value) => {
                    self.history.insert(key.clone(), value.clone());
                }
                None => {
                    self.history.remove(key);
                }
            }
        }
        self.validate().map_err(|e| e.to_string())
    }

    pub fn new(penalty_threshold_bps: u32) -> Self {
        Self {
            history: Default::default(),
            penalty_threshold_bps,
            window_assignments: burst_types::ProtocolParams::burst_defaults()
                .neither_window_assignments,
        }
    }

    /// Changes apply to future observations. A larger window fills prospectively;
    /// discarded assignments are not fabricated and past penalties are not undone.
    pub fn configure(&mut self, penalty_threshold_bps: u32, window_assignments: u32) {
        self.penalty_threshold_bps = penalty_threshold_bps;
        if window_assignments == self.window_assignments {
            return;
        }
        self.window_assignments = window_assignments;
        for key in self.history.keys().cloned().collect::<Vec<_>>() {
            let history = self.history.get_mut(&key).expect("existing history");
            history.trim(window_assignments as usize);
            if history.votes.is_empty() {
                self.history.remove(&key);
            }
        }
    }

    pub(crate) fn validate(&self) -> Result<(), bincode::Error> {
        if self.history.values().any(|history| {
            history.votes.is_empty()
                || history.votes.len() > self.window_assignments as usize
                || history.neither as usize != history.votes.iter().filter(|vote| **vote).count()
        }) {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "invalid rolling Neither history".into(),
            )));
        }
        Ok(())
    }

    /// Record one assignment. The caller authenticates the vote or timeout;
    /// this tracker stores only whether it was Neither.
    pub fn record_vote(&mut self, verifier: &WalletAddress, vote: Vote) {
        if self.window_assignments == 0 {
            return;
        }
        let history = self.history.entry(verifier.to_string()).or_default();
        history.trim(self.window_assignments as usize - 1);
        let neither = vote == Vote::Neither;
        history.votes.push_back(neither);
        history.neither += u32::from(neither);
    }

    pub fn exceeds_penalty_threshold(&self, verifier: &WalletAddress) -> bool {
        self.history.get(verifier.as_str()).is_some_and(|history| {
            u64::from(history.neither) * 10_000
                > history.votes.len() as u64 * u64::from(self.penalty_threshold_bps)
        })
    }

    pub fn neither_ratio_bps(&self, verifier: &WalletAddress) -> u32 {
        self.history.get(verifier.as_str()).map_or(0, |history| {
            if history.votes.is_empty() {
                0
            } else {
                (u64::from(history.neither) * 10_000 / history.votes.len() as u64) as u32
            }
        })
    }

    /// Retained assignments, not a lifetime total.
    pub fn retained_assignments(&self, verifier: &WalletAddress) -> u32 {
        self.history
            .get(verifier.as_str())
            .map_or(0, |history| history.votes.len() as u32)
    }

    pub fn neither_count(&self, verifier: &WalletAddress) -> u32 {
        self.history
            .get(verifier.as_str())
            .map_or(0, |history| history.neither)
    }

    /// Clear the history and return a proposed cooldown end. The caller checks
    /// the threshold and stores/enforces the cooldown; this method does neither.
    pub fn apply_neither_penalty(
        &mut self,
        verifier: &WalletAddress,
        current_time_secs: u64,
        cooldown_secs: u64,
    ) -> NeitherPenaltyAction {
        self.reset(verifier);
        NeitherPenaltyAction {
            verifier: verifier.clone(),
            cooldown_until: current_time_secs.saturating_add(cooldown_secs),
            forfeited_rewards: true,
        }
    }

    pub fn reset(&mut self, verifier: &WalletAddress) {
        self.history.remove(verifier.as_str());
    }

    pub fn tracked_count(&self) -> usize {
        self.history.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(s: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{}", &s.repeat(60 / s.len() + 1)[..60]))
    }

    #[test]
    fn large_panel_tally_preserves_rounding_and_revote_rules_without_overflow() {
        for count in [429_496, 429_497, 1_000_000] {
            let mut state = VerificationState {
                target: test_addr("target"),
                phase: VerificationPhase::Voting,
                endorsements: Vec::new().into(),
                endorser_set: Default::default(),
                selected_verifiers: Vec::new().into(),
                revote_count: 0,
                excluded_verifiers: Default::default(),
                started_at: Timestamp::new(1),
                votes: (0..count)
                    .map(|n| VerifierVote {
                        verifier: WalletAddress::new(format!("brst_large_panel_{n}")),
                        vote: Vote::Legitimate,
                        stake_amount: 1,
                        timestamp: Timestamp::new(2),
                    })
                    .collect(),
            };
            let voting = VerificationVoting;
            assert!(matches!(
                voting.tally(&state, 10_000, 0),
                VotingOutcome::Verified
            ));
            assert!(matches!(
                voting.tally(&state, 10_001, 0),
                VotingOutcome::Failed
            ));
            state.votes.last_mut().unwrap().vote = Vote::Neither;
            assert!(matches!(
                voting.tally(&state, 9_999, 0),
                VotingOutcome::Verified
            ));
            assert!(matches!(
                voting.tally(&state, 10_000, 0),
                VotingOutcome::Failed
            ));
            assert!(matches!(
                voting.tally(&state, 10_000, 1),
                VotingOutcome::Revote
            ));
            state.revote_count = 1;
            assert!(matches!(
                voting.tally(&state, 10_000, 1),
                VotingOutcome::Failed
            ));
            state.votes.truncate(3);
            state.votes[2].vote = Vote::Illegitimate;
            assert!(matches!(
                voting.tally(&state, 6_666, 1),
                VotingOutcome::Verified
            ));
            assert!(matches!(
                voting.tally(&state, 6_667, 1),
                VotingOutcome::Failed
            ));
            state.votes.clear();
            assert!(matches!(voting.tally(&state, 0, 0), VotingOutcome::Revote));
        }
    }

    #[test]
    fn rolling_history_cannot_bank_old_participation_to_hide_recent_neither_votes() {
        let verifier = test_addr("v1");
        let mut tracker = NeitherVoteTracker::new(5000);
        tracker.configure(5000, 4);
        for _ in 0..1000 {
            tracker.record_vote(&verifier, Vote::Legitimate);
        }
        for _ in 0..2 {
            tracker.record_vote(&verifier, Vote::Neither);
        }
        assert_eq!(tracker.retained_assignments(&verifier), 4);
        assert!(!tracker.exceeds_penalty_threshold(&verifier));
        tracker.record_vote(&verifier, Vote::Neither);
        assert!(tracker.exceeds_penalty_threshold(&verifier));
        assert_eq!(tracker.neither_count(&verifier), 3);
        let bytes = bincode::serialize(&tracker).unwrap();
        let mut restored: NeitherVoteTracker = bincode::deserialize(&bytes).unwrap();
        restored.validate().unwrap();
        for _ in 0..4 {
            restored.record_vote(&verifier, Vote::Illegitimate);
        }
        assert_eq!(restored.retained_assignments(&verifier), 4);
        assert_eq!(restored.neither_count(&verifier), 0);
        assert!(!restored.exceeds_penalty_threshold(&verifier));
    }

    #[test]
    fn governed_window_shrink_growth_disable_and_exact_threshold() {
        let verifier = test_addr("v1");
        let mut tracker = NeitherVoteTracker::new(6666);
        tracker.configure(6666, 3);
        for vote in [Vote::Legitimate, Vote::Neither, Vote::Neither] {
            tracker.record_vote(&verifier, vote);
        }
        assert!(
            tracker.exceeds_penalty_threshold(&verifier),
            "2/3 is strictly above 6666 basis points"
        );
        tracker.configure(6667, 3);
        assert!(!tracker.exceeds_penalty_threshold(&verifier));
        tracker.configure(5000, 1);
        assert_eq!(tracker.retained_assignments(&verifier), 1);
        assert!(tracker.exceeds_penalty_threshold(&verifier));
        tracker.configure(5000, 100);
        assert_eq!(
            tracker.retained_assignments(&verifier),
            1,
            "growth cannot invent old assignments"
        );
        tracker.configure(10000, 100);
        assert!(!tracker.exceeds_penalty_threshold(&verifier));
        tracker.configure(0, 0);
        tracker.record_vote(&verifier, Vote::Neither);
        assert_eq!(tracker.tracked_count(), 0);
        assert!(!tracker.exceeds_penalty_threshold(&verifier));
        tracker.configure(0, u32::MAX);
        tracker.record_vote(&verifier, Vote::Neither);
        assert!(tracker.exceeds_penalty_threshold(&verifier));
        let penalty = tracker.apply_neither_penalty(&verifier, 10, u64::MAX);
        assert_eq!(penalty.cooldown_until, u64::MAX);
        assert_eq!(tracker.tracked_count(), 0);
    }

    #[test]
    fn malformed_history_and_unordered_legacy_counts_require_replay() {
        let verifier = test_addr("v1");
        let mut tracker = NeitherVoteTracker::new(5000);
        tracker.record_vote(&verifier, Vote::Neither);
        tracker.history.get_mut(verifier.as_str()).unwrap().neither = 0;
        assert!(tracker.validate().is_err());
        let mut legacy = LegacyNeitherVoteTracker::empty();
        legacy.history.insert(verifier.to_string(), (3, 1));
        assert!(legacy.into_rolling().is_err());
        assert_eq!(
            LegacyNeitherVoteTracker::empty()
                .into_rolling()
                .unwrap()
                .tracked_count(),
            0
        );
    }

    #[test]
    fn neither_tracker_no_votes_not_penalized() {
        let tracker = NeitherVoteTracker::new(5000);
        assert!(!tracker.exceeds_penalty_threshold(&test_addr("v1")));
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
        assert!(!tracker.exceeds_penalty_threshold(&v));
    }

    #[test]
    fn neither_tracker_above_threshold() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Legitimate);
        // 2/3 = 6666 bps > 5000
        assert!(tracker.exceeds_penalty_threshold(&v));
    }

    #[test]
    fn neither_tracker_exact_threshold_not_penalized() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Legitimate);
        // 1/2 = 5000 bps = 5000 (not exceeded, equal)
        assert!(!tracker.exceeds_penalty_threshold(&v));
    }

    #[test]
    fn neither_tracker_reset_clears_history() {
        let mut tracker = NeitherVoteTracker::new(5000);
        let v = test_addr("v1");
        tracker.record_vote(&v, Vote::Neither);
        tracker.record_vote(&v, Vote::Neither);
        assert!(tracker.exceeds_penalty_threshold(&v));
        tracker.reset(&v);
        assert!(!tracker.exceeds_penalty_threshold(&v));
        assert_eq!(tracker.retained_assignments(&v), 0);
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
        assert!(tracker.exceeds_penalty_threshold(&v1));
        assert!(!tracker.exceeds_penalty_threshold(&v2));
        assert_eq!(tracker.tracked_count(), 2);
    }
}
