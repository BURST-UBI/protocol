//! Verification orchestrator — connects endorsement, selection, voting, and outcomes
//! into a single end-to-end verification workflow.

use crate::challenge::{Challenge, ChallengeEngine, CHALLENGE_TIMEOUT_SECS};
use crate::endorsement::EndorsementEngine;
use crate::error::VerificationError;
use crate::outcomes::{
    compute_challenge_outcome, compute_verification_outcomes, ChallengeOutcomeEvent,
    ChallengeResult, VerificationOutcomeEvent, VerificationResult,
};
use crate::state::{VerificationPhase, VerificationState};
use crate::voting::{NeitherVoteTracker, VerificationVoting, Vote, VotingOutcome};
use burst_types::{ProtocolParams, Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Events emitted by the orchestrator for the node to process.
#[derive(Clone, Debug)]
pub enum VerificationEvent {
    /// Endorsement threshold reached — verification can begin.
    EndorsementComplete { wallet: WalletAddress },
    /// Verifiers have been selected.
    VerifiersSelected {
        wallet: WalletAddress,
        verifiers: Vec<WalletAddress>,
    },
    /// Verification vote completed — wallet is now verified or rejected.
    VerificationComplete {
        wallet: WalletAddress,
        result: VerificationResult,
        outcomes: VerificationOutcomeEvent,
    },
    /// Challenge resolved.
    ChallengeResolved {
        wallet: WalletAddress,
        outcome: ChallengeOutcomeEvent,
    },
    /// Wallet unverified (fraud confirmed).
    WalletUnverified { wallet: WalletAddress },
    /// Verifier penalized for excessive Neither voting.
    VerifierPenalized {
        verifier: WalletAddress,
        reason: String,
        cooldown_until: u64,
    },
}

/// The orchestrator ties together all verification subsystems.
pub struct VerificationOrchestrator {
    pub endorsement: EndorsementEngine,
    pub voting: VerificationVoting,
    pub challenges: ChallengeEngine,
    pub neither_tracker: NeitherVoteTracker,
    states: HashMap<WalletAddress, VerificationState>,
    active_challenges: HashMap<WalletAddress, Challenge>,
    /// Verifiers under penalty cooldown: address -> cooldown_until timestamp (secs).
    penalized_verifiers: HashMap<WalletAddress, u64>,
    /// Pending events for the node to process.
    pending_events: Vec<VerificationEvent>,
}

impl Default for VerificationOrchestrator {
    fn default() -> Self {
        Self {
            endorsement: EndorsementEngine,
            voting: VerificationVoting,
            challenges: ChallengeEngine,
            neither_tracker: NeitherVoteTracker::new(5000),
            states: HashMap::new(),
            active_challenges: HashMap::new(),
            penalized_verifiers: HashMap::new(),
            pending_events: Vec::new(),
        }
    }
}

impl VerificationOrchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process an endorsement for a wallet.
    /// If the threshold is met, emits EndorsementComplete.
    pub fn process_endorsement(
        &mut self,
        wallet: &WalletAddress,
        endorser: &WalletAddress,
        brn_burned: u128,
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        let state = self
            .states
            .entry(wallet.clone())
            .or_insert_with(|| VerificationState {
                target: wallet.clone(),
                phase: VerificationPhase::Endorsing,
                endorsements: Vec::new(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: std::collections::HashSet::new(),
                started_at: Timestamp::now(),
            });

        self.endorsement.submit_endorsement(
            state,
            endorser.clone(),
            brn_burned,
            Timestamp::now(),
        )?;

        if self
            .endorsement
            .check_threshold(state, params.endorsement_threshold)
        {
            self.pending_events
                .push(VerificationEvent::EndorsementComplete {
                    wallet: wallet.clone(),
                });
        }

        Ok(())
    }

    /// Select verifiers for a wallet that has completed endorsement (or is under challenge).
    pub fn select_verifiers(
        &mut self,
        wallet: &WalletAddress,
        eligible_verifiers: &[WalletAddress],
        randomness: &[u8; 32],
        params: &ProtocolParams,
    ) -> Result<Vec<WalletAddress>, VerificationError> {
        let now_secs = Timestamp::now().as_secs();
        self.cleanup_expired_penalties(now_secs);

        let state = self.states.get_mut(wallet).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {wallet}"))
        })?;

        match state.phase {
            VerificationPhase::Endorsing
            | VerificationPhase::Challenged
            | VerificationPhase::Voting => {}
            _ => {
                return Err(VerificationError::Other(format!(
                    "wallet {wallet} is in phase {:?}, cannot select verifiers",
                    state.phase
                )));
            }
        }

        let penalized = &self.penalized_verifiers;
        let filtered: Vec<&WalletAddress> = eligible_verifiers
            .iter()
            .filter(|w| !state.excluded_verifiers.contains(w))
            .filter(|w| !penalized.get(w).is_some_and(|&until| now_secs < until))
            .collect();

        let count = params.num_verifiers as usize;
        let mut scored: Vec<(usize, [u8; 32])> = filtered
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                let hash = burst_crypto::blake2b_256_multi(&[randomness, addr.as_str().as_bytes()]);
                (i, hash)
            })
            .collect();

        scored.sort_by_key(|a| a.1);
        scored.truncate(count);

        let selected: Vec<WalletAddress> =
            scored.iter().map(|(i, _)| filtered[*i].clone()).collect();

        state.selected_verifiers = selected.clone();
        state.votes.clear();
        state.phase = VerificationPhase::Voting;

        self.pending_events
            .push(VerificationEvent::VerifiersSelected {
                wallet: wallet.clone(),
                verifiers: selected.clone(),
            });

        Ok(selected)
    }

    /// Process a verification vote.
    ///
    /// For regular verification: auto-tallies when all verifiers have voted and
    /// returns the completion event. For challenge re-votes: just records the vote
    /// (call `resolve_challenge` to finalize).
    pub fn process_vote(
        &mut self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
        vote: Vote,
        params: &ProtocolParams,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        let is_challenge = self.active_challenges.contains_key(wallet);

        let state = self.states.get_mut(wallet).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {wallet}"))
        })?;

        if state.phase != VerificationPhase::Voting {
            return Err(VerificationError::Other(format!(
                "wallet {wallet} is in phase {:?}, not accepting votes",
                state.phase
            )));
        }

        let stake = match vote {
            Vote::Neither => 0,
            _ => params.verifier_stake_amount,
        };

        self.voting
            .cast_vote(state, voter.clone(), vote, stake, Timestamp::now())?;
        self.neither_tracker.record_vote(voter, vote);

        if self.neither_tracker.is_penalized(voter) {
            let now_secs = Timestamp::now().as_secs();
            let penalty = self.neither_tracker.apply_neither_penalty(
                voter,
                now_secs,
                params.neither_penalty_cooldown_secs,
            );
            self.penalized_verifiers
                .insert(voter.clone(), penalty.cooldown_until);
            self.pending_events
                .push(VerificationEvent::VerifierPenalized {
                    verifier: voter.clone(),
                    reason: "excessive_neither_votes".into(),
                    cooldown_until: penalty.cooldown_until,
                });
            return Err(VerificationError::NeitherPenalty(voter.to_string()));
        }

        if state.votes.len() < state.selected_verifiers.len() {
            return Ok(None);
        }

        // All votes are in — for challenges, let resolve_challenge handle the outcome.
        if is_challenge {
            return Ok(None);
        }

        let tally = self
            .voting
            .tally(state, params.verification_threshold_bps, params.max_revotes);

        match tally {
            VotingOutcome::Verified => {
                state.phase = VerificationPhase::Verified;
                let result = VerificationResult::Verified;
                let outcomes = build_verification_outcomes(wallet, &result, state);
                let event = VerificationEvent::VerificationComplete {
                    wallet: wallet.clone(),
                    result,
                    outcomes,
                };
                self.pending_events.push(event.clone());
                Ok(Some(event))
            }
            VotingOutcome::Failed => {
                state.phase = VerificationPhase::Failed;
                let result = VerificationResult::Failed;
                let outcomes = build_verification_outcomes(wallet, &result, state);
                let event = VerificationEvent::VerificationComplete {
                    wallet: wallet.clone(),
                    result,
                    outcomes,
                };
                self.pending_events.push(event.clone());
                Ok(Some(event))
            }
            VotingOutcome::Revote => {
                self.voting.initiate_revote(state, params.max_revotes)?;
                Ok(None)
            }
        }
    }

    /// Initiate a challenge against a verified wallet.
    pub fn initiate_challenge(
        &mut self,
        target: &WalletAddress,
        challenger: &WalletAddress,
        challenger_is_verified: bool,
        stake: u128,
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        if !challenger_is_verified {
            return Err(VerificationError::ChallengerNotVerified(
                challenger.to_string(),
            ));
        }

        if stake < params.challenge_stake_amount {
            return Err(VerificationError::InsufficientStake {
                needed: params.challenge_stake_amount,
                provided: stake,
            });
        }

        let state = self.states.get_mut(target).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {target}"))
        })?;

        if state.phase != VerificationPhase::Verified {
            return Err(VerificationError::Other(format!(
                "wallet {target} is not verified, cannot challenge"
            )));
        }

        let challenge =
            self.challenges
                .initiate(challenger.clone(), target.clone(), stake, Timestamp::now());

        self.active_challenges.insert(target.clone(), challenge);
        state.phase = VerificationPhase::Challenged;
        state.excluded_verifiers.clear();
        state.selected_verifiers.clear();
        state.votes.clear();

        Ok(())
    }

    /// Resolve a challenge after re-verification votes are in.
    ///
    /// If fraud is confirmed the wallet is set to Unverified and a
    /// `WalletUnverified` event is emitted (the node uses this to trigger
    /// TRST revocation via the merger graph).
    pub fn resolve_challenge(
        &mut self,
        target: &WalletAddress,
        params: &ProtocolParams,
    ) -> Result<VerificationEvent, VerificationError> {
        let challenge = self
            .active_challenges
            .remove(target)
            .ok_or_else(|| VerificationError::NoChallengeActive(target.to_string()))?;

        let state = self.states.get_mut(target).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {target}"))
        })?;

        let tally = self
            .voting
            .tally(state, params.verification_threshold_bps, params.max_revotes);
        let fraud_confirmed = !matches!(tally, VotingOutcome::Verified);

        let challenge_result = if fraud_confirmed {
            ChallengeResult::FraudConfirmed
        } else {
            ChallengeResult::ChallengeRejected
        };

        let outcome_was_legitimate = !fraud_confirmed;
        let verifiers: Vec<(WalletAddress, u128, bool)> = state
            .votes
            .iter()
            .map(|v| {
                let voted_correctly = if outcome_was_legitimate {
                    v.vote == Vote::Legitimate
                } else {
                    v.vote != Vote::Legitimate
                };
                (v.verifier.clone(), v.stake_amount, voted_correctly)
            })
            .collect();

        let outcome_event = compute_challenge_outcome(
            target,
            &challenge.challenger,
            challenge_result.clone(),
            challenge.stake_amount,
            &verifiers,
        );

        if fraud_confirmed {
            state.phase = VerificationPhase::Unverified;
            self.pending_events
                .push(VerificationEvent::WalletUnverified {
                    wallet: target.clone(),
                });
        } else {
            state.phase = VerificationPhase::Verified;
        }

        let event = VerificationEvent::ChallengeResolved {
            wallet: target.clone(),
            outcome: outcome_event,
        };
        self.pending_events.push(event.clone());

        Ok(event)
    }

    /// During the genesis/bootstrap phase (verified_wallets < bootstrap_exit_threshold),
    /// the genesis creator can directly endorse wallets without requiring the
    /// endorser to be verified. This solves the chicken-and-egg problem.
    pub fn genesis_endorse(
        &mut self,
        endorser: &WalletAddress,
        target: &WalletAddress,
        genesis_creator: &WalletAddress,
        verified_wallet_count: u64,
        bootstrap_threshold: u64,
    ) -> Result<(), VerificationError> {
        if verified_wallet_count >= bootstrap_threshold {
            return Err(VerificationError::BootstrapPhaseEnded);
        }

        if endorser != genesis_creator {
            return Err(VerificationError::NotGenesisCreator);
        }

        let state = self
            .states
            .entry(target.clone())
            .or_insert_with(|| VerificationState {
                target: target.clone(),
                phase: VerificationPhase::Endorsing,
                endorsements: Vec::new(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: std::collections::HashSet::new(),
                started_at: Timestamp::now(),
            });

        self.endorsement
            .submit_endorsement(state, endorser.clone(), 0, Timestamp::now())?;

        self.pending_events
            .push(VerificationEvent::EndorsementComplete {
                wallet: target.clone(),
            });

        Ok(())
    }

    /// During bootstrap phase, the genesis creator can directly verify wallets
    /// (skipping the normal endorsement threshold + verifier vote process).
    pub fn genesis_verify(
        &mut self,
        wallet: &WalletAddress,
        genesis_creator: &WalletAddress,
        verified_wallet_count: u64,
        bootstrap_threshold: u64,
    ) -> Result<(), VerificationError> {
        if verified_wallet_count >= bootstrap_threshold {
            return Err(VerificationError::BootstrapPhaseEnded);
        }

        if wallet == genesis_creator {
            return Err(VerificationError::SelfVerification);
        }

        let state = self
            .states
            .entry(wallet.clone())
            .or_insert_with(|| VerificationState {
                target: wallet.clone(),
                phase: VerificationPhase::Endorsing,
                endorsements: Vec::new(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: std::collections::HashSet::new(),
                started_at: Timestamp::now(),
            });

        state.phase = VerificationPhase::Verified;

        let result = VerificationResult::Verified;
        let outcomes = build_verification_outcomes(wallet, &result, state);
        let event = VerificationEvent::VerificationComplete {
            wallet: wallet.clone(),
            result,
            outcomes,
        };
        self.pending_events.push(event);

        Ok(())
    }

    /// Check if a verifier is currently under Neither-vote penalty cooldown.
    pub fn is_penalized(&self, verifier: &WalletAddress, current_time_secs: u64) -> bool {
        self.penalized_verifiers
            .get(verifier)
            .is_some_and(|&until| current_time_secs < until)
    }

    /// Remove expired penalties from the map.
    pub fn cleanup_expired_penalties(&mut self, current_time_secs: u64) {
        self.penalized_verifiers
            .retain(|_, until| current_time_secs < *until);
    }

    /// Clean up challenges that have exceeded the deadline.
    ///
    /// Expired challenges resolve in favor of the challenged wallet.
    /// The challenger's stake is returned minus a penalty. Should be called
    /// periodically from the node's tick loop.
    pub fn cleanup_expired_challenges(&mut self, now: Timestamp) -> Vec<VerificationEvent> {
        let now_secs = now.as_secs();
        let expired: Vec<WalletAddress> = self
            .active_challenges
            .iter()
            .filter(|(_, c)| {
                now_secs.saturating_sub(c.initiated_at.as_secs()) > CHALLENGE_TIMEOUT_SECS
            })
            .map(|(target, _)| target.clone())
            .collect();

        let mut events = Vec::new();
        for target in expired {
            let challenge = self.active_challenges.remove(&target).unwrap();

            if let Some(state) = self.states.get_mut(&target) {
                state.phase = VerificationPhase::Verified;
            }

            let outcome_event = compute_challenge_outcome(
                &target,
                &challenge.challenger,
                ChallengeResult::Expired,
                challenge.stake_amount,
                &[],
            );

            let event = VerificationEvent::ChallengeResolved {
                wallet: target,
                outcome: outcome_event,
            };
            events.push(event.clone());
            self.pending_events.push(event);
        }
        events
    }

    /// Get the verification state of a wallet.
    pub fn get_state(&self, wallet: &WalletAddress) -> Option<&VerificationState> {
        self.states.get(wallet)
    }

    /// Drain pending events for the node to process.
    pub fn drain_events(&mut self) -> Vec<VerificationEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Number of verifiers currently under penalty cooldown.
    pub fn penalized_count(&self) -> usize {
        self.penalized_verifiers.len()
    }

    /// Serialize in-flight verification and challenge state for persistence.
    pub fn snapshot(&self) -> OrchestratorSnapshot {
        OrchestratorSnapshot {
            states: self.states.clone(),
            active_challenges: self.active_challenges.clone(),
            penalized_verifiers: self.penalized_verifiers.clone(),
        }
    }

    /// Restore in-flight verification state from a persisted snapshot.
    pub fn restore(snapshot: OrchestratorSnapshot) -> Self {
        Self {
            endorsement: EndorsementEngine,
            voting: VerificationVoting,
            challenges: ChallengeEngine,
            neither_tracker: NeitherVoteTracker::new(5000),
            states: snapshot.states,
            active_challenges: snapshot.active_challenges,
            penalized_verifiers: snapshot.penalized_verifiers,
            pending_events: Vec::new(),
        }
    }
}

/// Serializable snapshot of orchestrator state for persistence across restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorSnapshot {
    pub states: HashMap<WalletAddress, VerificationState>,
    pub active_challenges: HashMap<WalletAddress, Challenge>,
    pub penalized_verifiers: HashMap<WalletAddress, u64>,
}

fn build_verification_outcomes(
    wallet: &WalletAddress,
    result: &VerificationResult,
    state: &VerificationState,
) -> VerificationOutcomeEvent {
    let endorsers: Vec<(WalletAddress, u128)> = state
        .endorsements
        .iter()
        .map(|e| (e.endorser.clone(), e.burn_amount))
        .collect();

    let outcome_was_legitimate = *result == VerificationResult::Verified;
    let verifiers: Vec<(WalletAddress, u128, bool)> = state
        .votes
        .iter()
        .map(|v| {
            let voted_correctly = if outcome_was_legitimate {
                v.vote == Vote::Legitimate
            } else {
                v.vote != Vote::Legitimate
            };
            (v.verifier.clone(), v.stake_amount, voted_correctly)
        })
        .collect();

    compute_verification_outcomes(wallet, result.clone(), &endorsers, &verifiers)
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

    fn test_params() -> ProtocolParams {
        ProtocolParams {
            endorsement_threshold: 3,
            num_verifiers: 3,
            verification_threshold_bps: 6000, // 60% for easier testing
            verifier_stake_amount: 100,
            max_revotes: 3,
            challenge_stake_amount: 500,
            ..ProtocolParams::burst_defaults()
        }
    }

    /// Helper: run full endorsement phase, returning the orchestrator ready for verifier selection.
    fn endorse_wallet(
        orch: &mut VerificationOrchestrator,
        wallet: &WalletAddress,
        params: &ProtocolParams,
    ) {
        let endorsers: Vec<WalletAddress> = (1..=3).map(|i| test_addr(&format!("e{i}"))).collect();
        for e in &endorsers {
            orch.process_endorsement(wallet, e, 1000, params).unwrap();
        }
    }

    /// Helper: verify a wallet end-to-end so it can be challenged.
    fn verify_wallet(
        orch: &mut VerificationOrchestrator,
        wallet: &WalletAddress,
        params: &ProtocolParams,
    ) {
        endorse_wallet(orch, wallet, params);

        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        let randomness = [42u8; 32];
        let selected = orch
            .select_verifiers(wallet, &verifiers, &randomness, params)
            .unwrap();

        for v in &selected {
            orch.process_vote(wallet, v, Vote::Legitimate, params)
                .unwrap();
        }
    }

    // ── Full verification flow ──────────────────────────────────────────

    #[test]
    fn full_verification_flow_verified() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        // Phase 1: Endorsement
        endorse_wallet(&mut orch, &wallet, &params);

        let events = orch.drain_events();
        assert!(events.iter().any(|e| matches!(
            e,
            VerificationEvent::EndorsementComplete { wallet: w } if *w == wallet
        )));

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.endorsements.len(), 3);

        // Phase 2: Verifier selection
        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        let randomness = [1u8; 32];
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &randomness, &params)
            .unwrap();
        assert_eq!(selected.len(), 3);

        let events = orch.drain_events();
        assert!(events.iter().any(|e| matches!(
            e,
            VerificationEvent::VerifiersSelected { wallet: w, .. } if *w == wallet
        )));

        // Phase 3: Voting — all vote Legitimate
        for (i, v) in selected.iter().enumerate() {
            let result = orch
                .process_vote(&wallet, v, Vote::Legitimate, &params)
                .unwrap();
            if i < selected.len() - 1 {
                assert!(result.is_none());
            } else {
                let event = result.expect("last vote should trigger completion");
                match &event {
                    VerificationEvent::VerificationComplete {
                        wallet: w,
                        result,
                        outcomes,
                    } => {
                        assert_eq!(w, &wallet);
                        assert_eq!(*result, VerificationResult::Verified);
                        assert_eq!(outcomes.endorsers.len(), 3);
                        assert_eq!(outcomes.verifiers.len(), 3);
                        for vo in &outcomes.verifiers {
                            assert!(vo.voted_correctly);
                        }
                    }
                    _ => panic!("expected VerificationComplete"),
                }
            }
        }

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.phase, VerificationPhase::Verified);
    }

    #[test]
    fn full_verification_flow_failed() {
        let mut orch = VerificationOrchestrator::new();
        let mut params = test_params();
        params.max_revotes = 0;
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &[2u8; 32], &params)
            .unwrap();

        // All vote Illegitimate with no revotes remaining → fails
        for v in &selected {
            let _ = orch
                .process_vote(&wallet, v, Vote::Illegitimate, &params)
                .unwrap();
        }

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.phase, VerificationPhase::Failed);
    }

    #[test]
    fn endorsement_below_threshold_no_event() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        orch.process_endorsement(&wallet, &test_addr("e1"), 1000, &params)
            .unwrap();
        orch.process_endorsement(&wallet, &test_addr("e2"), 1000, &params)
            .unwrap();

        let events = orch.drain_events();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, VerificationEvent::EndorsementComplete { .. })),
            "should not emit EndorsementComplete before threshold"
        );
    }

    #[test]
    fn select_verifiers_wrong_phase_errors() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        verify_wallet(&mut orch, &wallet, &params);

        let result = orch.select_verifiers(&wallet, &[], &[0u8; 32], &params);
        assert!(result.is_err());
    }

    #[test]
    fn vote_from_non_selected_verifier_errors() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        orch.select_verifiers(&wallet, &verifiers, &[0u8; 32], &params)
            .unwrap();

        let outsider = test_addr("outsider");
        let result = orch.process_vote(&wallet, &outsider, Vote::Legitimate, &params);
        assert!(result.is_err());
    }

    // ── Challenge flow ──────────────────────────────────────────────────

    #[test]
    fn challenge_fraud_confirmed() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        verify_wallet(&mut orch, &wallet, &params);
        orch.drain_events();

        // Initiate challenge
        let challenger = test_addr("challenger");
        orch.initiate_challenge(&wallet, &challenger, true, 500, &params)
            .unwrap();

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.phase, VerificationPhase::Challenged);

        // Select verifiers for re-verification
        let verifiers: Vec<WalletAddress> =
            (10..=15).map(|i| test_addr(&format!("rv{i}"))).collect();
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &[99u8; 32], &params)
            .unwrap();

        // All vote Illegitimate → fraud confirmed
        for v in &selected {
            let result = orch
                .process_vote(&wallet, v, Vote::Illegitimate, &params)
                .unwrap();
            assert!(result.is_none(), "challenge votes should not auto-resolve");
        }

        // Resolve the challenge
        let event = orch.resolve_challenge(&wallet, &params).unwrap();
        match &event {
            VerificationEvent::ChallengeResolved { wallet: w, outcome } => {
                assert_eq!(w, &wallet);
                assert_eq!(outcome.outcome, ChallengeResult::FraudConfirmed);
                assert_eq!(outcome.challenger_reward, 1000); // 2x stake
                assert_eq!(outcome.challenger, challenger);
            }
            _ => panic!("expected ChallengeResolved"),
        }

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.phase, VerificationPhase::Unverified);

        // Should have WalletUnverified in pending events
        let events = orch.drain_events();
        assert!(events.iter().any(
            |e| matches!(e, VerificationEvent::WalletUnverified { wallet: w } if *w == wallet)
        ));
    }

    #[test]
    fn challenge_rejected() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        verify_wallet(&mut orch, &wallet, &params);
        orch.drain_events();

        let challenger = test_addr("challenger");
        orch.initiate_challenge(&wallet, &challenger, true, 500, &params)
            .unwrap();

        let verifiers: Vec<WalletAddress> =
            (10..=15).map(|i| test_addr(&format!("rv{i}"))).collect();
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &[88u8; 32], &params)
            .unwrap();

        // All vote Legitimate → challenge rejected
        for v in &selected {
            orch.process_vote(&wallet, v, Vote::Legitimate, &params)
                .unwrap();
        }

        let event = orch.resolve_challenge(&wallet, &params).unwrap();
        match &event {
            VerificationEvent::ChallengeResolved { wallet: w, outcome } => {
                assert_eq!(w, &wallet);
                assert_eq!(outcome.outcome, ChallengeResult::ChallengeRejected);
                assert_eq!(outcome.challenger_reward, 0);
            }
            _ => panic!("expected ChallengeResolved"),
        }

        // Wallet should be back to Verified
        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.phase, VerificationPhase::Verified);

        // No WalletUnverified event
        let events = orch.drain_events();
        assert!(!events
            .iter()
            .any(|e| matches!(e, VerificationEvent::WalletUnverified { .. })));
    }

    #[test]
    fn challenge_non_verified_wallet_errors() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let result = orch.initiate_challenge(&wallet, &test_addr("c"), true, 500, &params);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_without_challenge_errors() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        let result = orch.resolve_challenge(&wallet, &params);
        assert!(result.is_err());
    }

    #[test]
    fn neither_votes_tracked_across_verifications() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &[0u8; 32], &params)
            .unwrap();

        // Give verifier some Legitimate history so one Neither doesn't trigger penalty
        orch.neither_tracker
            .record_vote(&selected[0], Vote::Legitimate);
        orch.neither_tracker
            .record_vote(&selected[0], Vote::Legitimate);

        // Third vote is Neither — 1/3 = 3333 bps < 5000 threshold, no penalty
        orch.process_vote(&wallet, &selected[0], Vote::Neither, &params)
            .unwrap();

        assert_eq!(orch.neither_tracker.neither_count(&selected[0]), 1);
        assert_eq!(orch.neither_tracker.total_assignments(&selected[0]), 3);
    }

    #[test]
    fn drain_events_clears_buffer() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let events = orch.drain_events();
        assert!(!events.is_empty());

        let events = orch.drain_events();
        assert!(events.is_empty());
    }

    // ── Genesis bootstrapping ─────────────────────────────────────────

    #[test]
    fn genesis_creator_can_endorse_during_bootstrap() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");
        let target = test_addr("target");

        let result = orch.genesis_endorse(&genesis, &target, &genesis, 0, 10);
        assert!(result.is_ok());

        let events = orch.drain_events();
        assert!(events.iter().any(|e| matches!(
            e,
            VerificationEvent::EndorsementComplete { wallet: w } if *w == target
        )));
    }

    #[test]
    fn non_genesis_cannot_genesis_endorse() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");
        let impostor = test_addr("impostor");
        let target = test_addr("target");

        let result = orch.genesis_endorse(&impostor, &target, &genesis, 0, 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::NotGenesisCreator
        ));
    }

    #[test]
    fn genesis_endorse_fails_after_bootstrap_threshold() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");
        let target = test_addr("target");

        let result = orch.genesis_endorse(&genesis, &target, &genesis, 10, 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::BootstrapPhaseEnded
        ));
    }

    #[test]
    fn genesis_verify_sets_wallet_verified() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");
        let target = test_addr("target");

        let result = orch.genesis_verify(&target, &genesis, 0, 10);
        assert!(result.is_ok());

        let state = orch.get_state(&target).unwrap();
        assert_eq!(state.phase, VerificationPhase::Verified);

        let events = orch.drain_events();
        assert!(events.iter().any(|e| matches!(
            e,
            VerificationEvent::VerificationComplete {
                wallet: w,
                result: VerificationResult::Verified,
                ..
            } if *w == target
        )));
    }

    #[test]
    fn genesis_verify_rejects_self_verification() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");

        let result = orch.genesis_verify(&genesis, &genesis, 0, 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::SelfVerification
        ));
    }

    #[test]
    fn genesis_verify_fails_after_bootstrap_threshold() {
        let mut orch = VerificationOrchestrator::new();
        let genesis = test_addr("genesis");
        let target = test_addr("target");

        let result = orch.genesis_verify(&target, &genesis, 50, 50);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::BootstrapPhaseEnded
        ));
    }

    // ── Revote exclusion tests ────────────────────────────────────────

    #[test]
    fn revote_verifiers_do_not_overlap_with_previous_round() {
        let mut orch = VerificationOrchestrator::new();
        let mut params = test_params();
        params.num_verifiers = 2;
        params.max_revotes = 2;
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let pool: Vec<WalletAddress> = (1..=6).map(|i| test_addr(&format!("v{i}"))).collect();
        let randomness = [7u8; 32];

        let round1 = orch
            .select_verifiers(&wallet, &pool, &randomness, &params)
            .unwrap();
        assert_eq!(round1.len(), 2);

        // Force a revote
        for v in &round1 {
            orch.process_vote(&wallet, v, Vote::Illegitimate, &params)
                .unwrap();
        }

        // After tally triggers revote internally, select new verifiers
        let round2 = orch
            .select_verifiers(&wallet, &pool, &[8u8; 32], &params)
            .unwrap();
        assert_eq!(round2.len(), 2);

        for v in &round2 {
            assert!(
                !round1.contains(v),
                "round 2 verifier {v} was also in round 1"
            );
        }
    }

    #[test]
    fn excluded_verifiers_accumulate_across_multiple_revotes() {
        let mut orch = VerificationOrchestrator::new();
        let mut params = test_params();
        params.num_verifiers = 2;
        params.max_revotes = 3;
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let pool: Vec<WalletAddress> = (1..=8).map(|i| test_addr(&format!("v{i}"))).collect();

        // Round 1
        let round1 = orch
            .select_verifiers(&wallet, &pool, &[10u8; 32], &params)
            .unwrap();

        for v in &round1 {
            orch.process_vote(&wallet, v, Vote::Illegitimate, &params)
                .unwrap();
        }

        // Round 2
        let round2 = orch
            .select_verifiers(&wallet, &pool, &[20u8; 32], &params)
            .unwrap();

        for v in &round2 {
            orch.process_vote(&wallet, v, Vote::Illegitimate, &params)
                .unwrap();
        }

        // Round 3 — excluded list should contain round1 + round2
        let round3 = orch
            .select_verifiers(&wallet, &pool, &[30u8; 32], &params)
            .unwrap();

        let all_previous: Vec<&WalletAddress> = round1.iter().chain(round2.iter()).collect();

        for v in &round3 {
            assert!(
                !all_previous.contains(&v),
                "round 3 verifier {v} appeared in a previous round"
            );
        }

        let state = orch.get_state(&wallet).unwrap();
        assert_eq!(state.excluded_verifiers.len(), 4); // 2 from each of 2 rounds
    }

    // ── Challenger verification check ─────────────────────────────────

    #[test]
    fn challenge_fails_if_challenger_not_verified() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        verify_wallet(&mut orch, &wallet, &params);

        let unverified_challenger = test_addr("unverified");
        let result = orch.initiate_challenge(&wallet, &unverified_challenger, false, 500, &params);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::ChallengerNotVerified(_)
        ));
    }

    // ── Neither penalty enforcement ──────────────────────────────────

    #[test]
    fn penalized_verifier_excluded_from_selection() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        let v1 = test_addr("v1");

        // Manually penalize v1
        let future = Timestamp::now().as_secs() + 999_999;
        orch.penalized_verifiers.insert(v1.clone(), future);

        endorse_wallet(&mut orch, &wallet, &params);

        let pool: Vec<WalletAddress> = vec![
            v1.clone(),
            test_addr("v2"),
            test_addr("v3"),
            test_addr("v4"),
            test_addr("v5"),
        ];

        let selected = orch
            .select_verifiers(&wallet, &pool, &[42u8; 32], &params)
            .unwrap();

        assert!(
            !selected.contains(&v1),
            "penalized verifier should be excluded from selection"
        );
    }

    #[test]
    fn penalty_expires_verifier_eligible_again() {
        let mut orch = VerificationOrchestrator::new();
        let v1 = test_addr("v1");

        // Penalty already expired
        orch.penalized_verifiers.insert(v1.clone(), 0);

        assert!(
            !orch.is_penalized(&v1, 1),
            "verifier with expired cooldown should not be penalized"
        );

        // Cleanup should remove it
        orch.cleanup_expired_penalties(1);
        assert_eq!(orch.penalized_count(), 0);
    }

    #[test]
    fn excessive_neither_voting_triggers_penalty_event() {
        let mut orch = VerificationOrchestrator::new();
        let mut params = test_params();
        params.neither_penalty_cooldown_secs = 604800;
        let wallet = test_addr("target");

        endorse_wallet(&mut orch, &wallet, &params);

        let verifiers: Vec<WalletAddress> = (1..=5).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = orch
            .select_verifiers(&wallet, &verifiers, &[0u8; 32], &params)
            .unwrap();

        // Pre-load one verifier with 100% Neither history so next Neither triggers penalty
        orch.neither_tracker
            .record_vote(&selected[0], Vote::Neither);

        // This Neither vote should push ratio over 50% and trigger penalty
        let result = orch.process_vote(&wallet, &selected[0], Vote::Neither, &params);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VerificationError::NeitherPenalty(_)
        ));

        // A VerifierPenalized event should be in pending events
        let events = orch.drain_events();
        let penalty_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, VerificationEvent::VerifierPenalized { .. }))
            .collect();
        assert_eq!(penalty_events.len(), 1);

        match &penalty_events[0] {
            VerificationEvent::VerifierPenalized {
                verifier, reason, ..
            } => {
                assert_eq!(verifier, &selected[0]);
                assert_eq!(reason, "excessive_neither_votes");
            }
            _ => unreachable!(),
        }

        // Verifier should be in penalized map
        assert!(orch.penalized_count() > 0);
    }

    // ── Endorser reward outcome tests ────────────────────────────────

    #[test]
    fn verification_complete_includes_endorser_rewards() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let wallet = test_addr("target");

        verify_wallet(&mut orch, &wallet, &params);

        let events = orch.drain_events();
        let complete_event = events.iter().find(|e| {
            matches!(
                e,
                VerificationEvent::VerificationComplete {
                    result: VerificationResult::Verified,
                    ..
                }
            )
        });

        assert!(
            complete_event.is_some(),
            "should have VerificationComplete event"
        );

        if let Some(VerificationEvent::VerificationComplete { outcomes, .. }) = complete_event {
            assert_eq!(outcomes.endorsers.len(), 3);
            for eo in &outcomes.endorsers {
                assert_eq!(eo.brn_burned, 1000);
                // 10% of 1000 = 100
                assert_eq!(eo.trst_reward, 100);
            }
        }
    }
}
