//! Verification orchestrator — connects endorsement, selection, voting, and outcomes
//! into a single end-to-end verification workflow.

use crate::attempts::AttemptContexts;
use crate::challenge::{AppealContext, Challenge, ChallengeEngine};
use crate::endorsement::EndorsementEngine;
use crate::error::VerificationError;
use crate::outcomes::{
    compute_challenge_outcome, compute_verification_outcomes, ChallengeOutcomeEvent,
    ChallengeResult, VerificationOutcomeEvent, VerificationResult,
};
use crate::selection_request::SelectionRequest;
use crate::state::{VerificationPhase, VerificationState, VerifierVote};
use crate::voting::{NeitherVoteTracker, VerificationVoting, Vote, VotingOutcome};
use burst_consensus::checkpoint::{Checkpoint, CheckpointAnchor, CheckpointCertificate};
use burst_types::{BlockHash, ProtocolParams, Timestamp, WalletAddress};
use burst_vrf::RandomOutput;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod appeal;
mod deadline;
pub mod records;
#[cfg(test)]
pub(crate) mod snapshot_tests;
pub(crate) mod snapshot_undo;
pub use deadline::CommitteeActivation;

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
    /// Wallet unverified (fraud confirmed). All TRST originating from the
    /// wallet is revoked and BRN accrual stops.
    WalletUnverified { wallet: WalletAddress },
    /// Wallet deactivated for benign, non-fraud reasons — death, prolonged
    /// inactivity, or other community-defined grounds (whitepaper
    /// §Unverification Without Revocation; grounds are defined via the
    /// Consti). BRN accrual stops and transaction rights are lost, but the
    /// TRST it originated is NOT revoked — the holder was a real person who
    /// legitimately earned it.
    WalletDeactivated { wallet: WalletAddress },
    /// Verifier penalized for excessive Neither voting.
    VerifierPenalized {
        verifier: WalletAddress,
        reason: String,
        cooldown_until: u64,
    },
}

/// The orchestrator ties together all verification subsystems.
#[derive(Clone)]
pub struct VerificationOrchestrator {
    pub endorsement: EndorsementEngine,
    pub voting: VerificationVoting,
    pub challenges: ChallengeEngine,
    pub neither_tracker: NeitherVoteTracker,
    states: HashMap<WalletAddress, VerificationState>,
    active_challenges: HashMap<WalletAddress, Challenge>,
    active_appeals: HashMap<WalletAddress, AppealContext>,
    /// Earlier rounds stay locked until the entire attempt reaches an outcome.
    prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
    selections: HashMap<WalletAddress, SelectionRound>,
    attempts: AttemptContexts,
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
            active_appeals: HashMap::new(),
            prior_votes: HashMap::new(),
            selections: HashMap::new(),
            attempts: AttemptContexts::default(),
            penalized_verifiers: HashMap::new(),
            pending_events: Vec::new(),
        }
    }
}

impl VerificationOrchestrator {
    pub fn apply_governed_rules(&mut self, params: &ProtocolParams) {
        self.neither_tracker.configure(
            params.neither_penalty_threshold_bps,
            params.neither_window_assignments,
        );
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Install the configured genesis identity once, without emitting an admission
    /// or replacing a later challenge outcome. The caller supplies its trust anchor.
    pub fn initialize_genesis(&mut self, wallet: &WalletAddress) {
        self.states
            .entry(wallet.clone())
            .or_insert_with(|| VerificationState {
                target: wallet.clone(),
                phase: VerificationPhase::Verified,
                endorsements: Vec::new(),
                endorser_set: Default::default(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: Default::default(),
                started_at: Timestamp::EPOCH,
            });
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
        self.process_endorsement_at(wallet, endorser, brn_burned, params, Timestamp::now())
    }

    pub fn process_endorsement_at(
        &mut self,
        wallet: &WalletAddress,
        endorser: &WalletAddress,
        brn_burned: u128,
        params: &ProtocolParams,
        now: Timestamp,
    ) -> Result<(), VerificationError> {
        if brn_burned < params.endorsement_burn_amount {
            return Err(VerificationError::InsufficientEndorsementBurn {
                needed: params.endorsement_burn_amount,
                provided: brn_burned,
            });
        }
        let restart = self
            .states
            .get(wallet)
            .is_some_and(|state| state.phase.can_reverify());
        if restart {
            if self.active_challenges.contains_key(wallet) {
                return Err(VerificationError::Other(
                    "unsettled challenge cannot restart verification".into(),
                ));
            }
            let state = &self.states[wallet];
            let latest = state
                .votes
                .iter()
                .map(|vote| vote.timestamp.as_secs())
                .chain([state.started_at.as_secs()])
                .max()
                .unwrap();
            if now.as_secs() < latest {
                return Err(VerificationError::Other(
                    "re-verification predates the prior outcome".into(),
                ));
            }
            self.states.remove(wallet);
            self.selections.remove(wallet);
            self.prior_votes.remove(wallet);
            self.attempts.clear(wallet);
        }
        if self.selections.contains_key(wallet) {
            return Err(VerificationError::Other(
                "endorsement phase has a frozen selection".into(),
            ));
        }
        let state = self
            .states
            .entry(wallet.clone())
            .or_insert_with(|| VerificationState {
                target: wallet.clone(),
                phase: VerificationPhase::Endorsing,
                endorsements: Vec::new(),
                endorser_set: std::collections::HashSet::new(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: std::collections::HashSet::new(),
                started_at: now,
            });

        self.endorsement
            .submit_endorsement(state, endorser.clone(), brn_burned, now)?;
        // Independent account chains may arrive in either order. The signed
        // earliest endorsement defines the start, not local arrival order.
        state.started_at = state.started_at.min(now);

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
        self.select_verifiers_at(
            wallet,
            eligible_verifiers,
            randomness,
            params,
            Timestamp::now(),
        )
    }

    pub fn select_verifiers_at(
        &mut self,
        wallet: &WalletAddress,
        eligible_verifiers: &[WalletAddress],
        randomness: &[u8; 32],
        params: &ProtocolParams,
        now: Timestamp,
    ) -> Result<Vec<WalletAddress>, VerificationError> {
        if self.selections.contains_key(wallet) {
            return Err(VerificationError::Other(
                "a frozen selection cannot be replaced by local randomness".into(),
            ));
        }
        let now_secs = now.as_secs();
        self.cleanup_expired_penalties(now_secs);

        let state = self.states.get_mut(wallet).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {wallet}"))
        })?;

        match state.phase {
            VerificationPhase::Endorsing => {
                if !self
                    .endorsement
                    .check_threshold(state, params.endorsement_threshold)
                {
                    return Err(VerificationError::ThresholdNotMet {
                        have: state.endorsements.len() as u32,
                        need: params.endorsement_threshold,
                    });
                }
            }
            VerificationPhase::Challenged => {}
            VerificationPhase::Voting if state.selected_verifiers.is_empty() => {}
            VerificationPhase::Voting => {
                return Err(VerificationError::AlreadyInProgress(wallet.to_string()))
            }
            _ => {
                return Err(VerificationError::Other(format!(
                    "wallet {wallet} is in phase {:?}, cannot select verifiers",
                    state.phase
                )));
            }
        }

        let penalized = &self.penalized_verifiers;
        let mut unique = std::collections::HashSet::new();
        let filtered: Vec<&WalletAddress> = eligible_verifiers
            .iter()
            .filter(|w| *w != wallet && !state.endorser_set.contains(*w))
            .filter(|w| !state.excluded_verifiers.contains(w))
            .filter(|w| penalized.get(w).is_none_or(|&until| until <= now_secs))
            .filter(|w| unique.insert((*w).clone()))
            .collect();

        let count = params.num_verifiers as usize;
        if count == 0 || filtered.len() < count {
            return Err(VerificationError::Other(format!(
                "insufficient independent verifiers: have {}, need {count}",
                filtered.len()
            )));
        }
        let mut scored: Vec<(usize, [u8; 32])> = filtered
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                let hash = burst_crypto::blake2b_256_multi(&[randomness, addr.as_str().as_bytes()]);
                (i, hash)
            })
            .collect();

        scored.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| filtered[a.0].as_str().cmp(filtered[b.0].as_str()))
        });
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

    /// Called with the complete eligible pool and trigger derived from the
    /// confirmed cut, before checkpoint signing. The caller must validate that
    /// cut; neither a peer-provided pool nor current provisional accounts suffice.
    pub fn prepare_selection(
        &mut self,
        wallet: &WalletAddress,
        trigger: BlockHash,
        cutoff: Timestamp,
        eligible: &[WalletAddress],
        params: &ProtocolParams,
    ) -> Result<SelectionRequest, VerificationError> {
        let state = self.selection_state(wallet, cutoff, params)?;
        let pool = eligible
            .iter()
            .filter(|address| *address != wallet && !state.endorser_set.contains(*address))
            .filter(|address| !state.excluded_verifiers.contains(*address))
            .filter(|address| !self.is_penalized(address, cutoff.as_secs()))
            .cloned()
            .collect();
        let request = SelectionRequest::new(
            wallet.clone(),
            trigger,
            state.revote_count,
            cutoff.as_secs(),
            params.params_hash(),
            pool,
            params.num_verifiers,
        )
        .map_err(|error| VerificationError::Other(error.to_string()))?;
        self.freeze_replayed_selection(&request, params)?;
        Ok(request)
    }

    /// Restore inputs from their already validated certified cut. Do not filter
    /// the old pool against newer penalties, balances or opt-in state. This
    /// grants no voting authority until `install_selection` checks the proof.
    pub fn freeze_replayed_selection(
        &mut self,
        request: &SelectionRequest,
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        request
            .commitment()
            .map_err(|error| VerificationError::Other(error.to_string()))?;
        let wallet = request.target();
        self.validate_selection_lifecycle(request)?;
        if let Some(existing) = self.selections.get(wallet) {
            if existing.request != *request {
                return Err(VerificationError::Other(
                    "selection inputs are already frozen".into(),
                ));
            }
            return Ok(());
        }
        if request.params_hash() != params.params_hash() {
            return Err(VerificationError::Other(
                "selection parameters differ from replay".into(),
            ));
        }
        let state = self.selection_state(wallet, Timestamp::new(request.cutoff()), params)?;
        if request.attempt() != state.revote_count {
            return Err(VerificationError::Other(
                "selection belongs to another attempt".into(),
            ));
        }
        self.selections.insert(
            wallet.clone(),
            SelectionRound {
                request: request.clone(),
                certified: None,
            },
        );
        Ok(())
    }

    fn selection_state(
        &self,
        wallet: &WalletAddress,
        cutoff: Timestamp,
        params: &ProtocolParams,
    ) -> Result<&VerificationState, VerificationError> {
        let state = self
            .states
            .get(wallet)
            .ok_or_else(|| VerificationError::Other("no verification state".into()))?;
        match state.phase {
            VerificationPhase::Endorsing
                if state.endorsements.len() >= params.endorsement_threshold as usize => {}
            VerificationPhase::Challenged => {}
            VerificationPhase::Voting if state.selected_verifiers.is_empty() => {}
            _ => {
                return Err(VerificationError::Other(
                    "verification is not awaiting selection".into(),
                ))
            }
        }
        let latest = state
            .endorsements
            .iter()
            .map(|e| e.timestamp)
            .chain(
                self.prior_votes
                    .get(wallet)
                    .into_iter()
                    .flatten()
                    .map(|v| v.timestamp),
            )
            .chain([state.started_at])
            .map(|t| t.as_secs())
            .max()
            .unwrap();
        if cutoff.as_secs() < latest {
            return Err(VerificationError::Other(
                "selection cut predates its lifecycle dependencies".into(),
            ));
        }
        Ok(state)
    }

    /// Installs the exact certified pool selection, never a fresh eligibility
    /// query. `parent` must be the locally trusted checkpoint-chain anchor.
    pub fn install_selection(
        &mut self,
        wallet: &WalletAddress,
        parent: &CheckpointAnchor,
        certificate: &CheckpointCertificate,
        beacon: &RandomOutput,
    ) -> Result<BlockHash, VerificationError> {
        let selection = self
            .selections
            .get(wallet)
            .ok_or_else(|| VerificationError::Other("no frozen selection request".into()))?;
        self.validate_selection_lifecycle(&selection.request)?;
        let selected = selection
            .request
            .select(parent, certificate, beacon)
            .map_err(|error| VerificationError::Other(error.to_string()))?;
        let context = selection
            .request
            .vote_context(&certificate.checkpoint)
            .map_err(|error| VerificationError::Other(error.to_string()))?;
        if let Some((existing, _)) = &selection.certified {
            if existing.hash() != certificate.checkpoint.hash() {
                return Err(VerificationError::Other(
                    "selection already has a different certificate".into(),
                ));
            }
            return Ok(context);
        }
        let state = self
            .states
            .get_mut(wallet)
            .ok_or_else(|| VerificationError::Other("selection state is missing".into()))?;
        if !state.votes.is_empty()
            || !state.selected_verifiers.is_empty()
            || !matches!(
                state.phase,
                VerificationPhase::Endorsing
                    | VerificationPhase::Challenged
                    | VerificationPhase::Voting
            )
        {
            return Err(VerificationError::Other(
                "verification is not awaiting its committee".into(),
            ));
        }
        state.selected_verifiers = selected.clone();
        state.phase = VerificationPhase::Voting;
        self.selections.get_mut(wallet).unwrap().certified =
            Some((certificate.checkpoint.clone(), beacon.clone()));
        self.pending_events
            .push(VerificationEvent::VerifiersSelected {
                wallet: wallet.clone(),
                verifiers: selected,
            });
        Ok(context)
    }

    pub fn selection_request(&self, wallet: &WalletAddress) -> Option<&SelectionRequest> {
        self.selections
            .get(wallet)
            .map(|selection| &selection.request)
    }

    pub fn selection_context(&self, wallet: &WalletAddress) -> Option<BlockHash> {
        let selection = self.selections.get(wallet)?;
        let (checkpoint, _) = selection.certified.as_ref()?;
        selection.request.vote_context(checkpoint).ok()
    }

    /// Only accepted signed votes bind these contexts. Synthetic timeout votes
    /// cannot prove completion of an earlier panel.
    pub fn completed_selection(
        &self,
        wallet: &WalletAddress,
        context: BlockHash,
        selected: &[WalletAddress],
    ) -> bool {
        !context.is_zero()
            && !selected.is_empty()
            && self.attempts.completed_panel(wallet, context, selected)
    }

    /// Compare a target's frozen economic participants with replay without
    /// comparing unrelated live requests, local arrival order or current pools.
    /// Challenges retain endorsements and reset the round counter, so their
    /// signed identity and economic terms must also distinguish the lifecycle.
    pub fn selection_history_commitment(
        &self,
        wallet: &WalletAddress,
    ) -> Result<BlockHash, VerificationError> {
        self.selection_history_with_defaults(wallet, &HashMap::new())
    }

    /// Defaults must come from earlier replayed deadline certificates. They are
    /// committed separately and never bind a signed vote or a stake receipt.
    pub fn selection_history_with_defaults(
        &self,
        wallet: &WalletAddress,
        defaults: &HashMap<WalletAddress, (BlockHash, u64)>,
    ) -> Result<BlockHash, VerificationError> {
        let state = self
            .states
            .get(wallet)
            .ok_or_else(|| VerificationError::Other("selection target is missing".into()))?;
        let mut endorsements: Vec<_> = state
            .endorsements
            .iter()
            .map(|entry| (&entry.endorser, entry.burn_amount, entry.timestamp))
            .collect();
        endorsements.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let mut votes = Vec::new();
        let mut default_votes = Vec::new();
        for vote in self.prior_votes.get(wallet).into_iter().flatten() {
            let context = self
                .attempts
                .vote_context(wallet, &vote.verifier)
                .filter(|context| !context.is_zero());
            if let Some((completion, deadline)) = defaults.get(&vote.verifier) {
                if context.is_some()
                    || completion.is_zero()
                    || vote.vote != Vote::Neither
                    || vote.stake_amount != 0
                    || vote.timestamp.as_secs() != *deadline
                {
                    return Err(VerificationError::Other(
                        "prior default differs from its certified completion".into(),
                    ));
                }
                default_votes.push((&vote.verifier, *completion, *deadline));
                continue;
            }
            let context = context.ok_or_else(|| {
                VerificationError::Other("prior vote has no certified context".into())
            })?;
            votes.push((
                &vote.verifier,
                vote.vote,
                vote.stake_amount,
                vote.timestamp,
                context,
            ));
        }
        if default_votes.len() != defaults.len() {
            return Err(VerificationError::Other(
                "certified defaults do not match prior votes".into(),
            ));
        }
        default_votes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        votes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let mut excluded: Vec<_> = state.excluded_verifiers.iter().collect();
        excluded.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let challenge = self.selection_challenge(wallet)?;
        let mut bytes =
            bincode::serialize(&(state.revote_count, endorsements, votes, excluded, challenge))
                .map_err(|error| VerificationError::Other(error.to_string()))?;
        if let Some(appeal) = self.active_appeals.get(wallet) {
            bytes.extend_from_slice(b"BURST/appeal-selection/v1\0");
            bytes.extend(
                bincode::serialize(appeal)
                    .map_err(|error| VerificationError::Other(error.to_string()))?,
            );
        }
        if !default_votes.is_empty() {
            let defaults = bincode::serialize(&default_votes)
                .map_err(|error| VerificationError::Other(error.to_string()))?;
            return Ok(BlockHash::new(burst_crypto::blake2b_256_multi(&[
                b"BURST/selection-history-with-defaults/v1\0",
                &bytes,
                &defaults,
            ])));
        }
        Ok(BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/selection-history/v2\0",
            &bytes,
        ])))
    }

    pub fn prior_panel_votes(&self, wallet: &WalletAddress) -> &[crate::state::VerifierVote] {
        self.prior_votes.get(wallet).map_or(&[], Vec::as_slice)
    }

    /// The signed admission for the active challenge, including during revotes.
    /// Legacy state without that binding cannot supply certified selection inputs.
    pub fn active_challenge_context(
        &self,
        wallet: &WalletAddress,
    ) -> Result<Option<BlockHash>, VerificationError> {
        Ok(self
            .selection_challenge(wallet)?
            .map(|(_, context)| context))
    }

    pub fn has_active_challenge(&self, wallet: &WalletAddress) -> bool {
        self.active_challenges.contains_key(wallet)
    }

    fn selection_challenge(
        &self,
        wallet: &WalletAddress,
    ) -> Result<Option<(&Challenge, BlockHash)>, VerificationError> {
        let Some(challenge) = self.active_challenges.get(wallet) else {
            return Ok(None);
        };
        let context = self
            .attempts
            .challenge_context(wallet)
            .filter(|context| !context.is_zero())
            .ok_or_else(|| {
                VerificationError::Other("selection challenge has no signed context".into())
            })?;
        Ok(Some((challenge, context)))
    }

    fn validate_selection_lifecycle(
        &self,
        request: &SelectionRequest,
    ) -> Result<(), VerificationError> {
        if let Some((_, context)) = self.selection_challenge(request.target())? {
            if request.attempt() == 0 && request.trigger() != context {
                return Err(VerificationError::Other(
                    "selection belongs to another challenge".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn validate_certified_vote(
        &self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
        context: BlockHash,
        params_hash: BlockHash,
        timestamp: Timestamp,
    ) -> Result<(), VerificationError> {
        self.validate_vote(wallet, voter)?;
        let selection = self.selections.get(wallet).ok_or_else(|| {
            VerificationError::Other("verification committee is not certified".into())
        })?;
        self.validate_selection_lifecycle(&selection.request)?;
        if self.selection_context(wallet) != Some(context) || context.is_zero() {
            return Err(VerificationError::Other(
                "verification vote has the wrong selection context".into(),
            ));
        }
        if selection.request.params_hash() != params_hash {
            return Err(VerificationError::Other(
                "verification vote parameters differ from its request".into(),
            ));
        }
        let release = selection
            .request
            .beacon_time()
            .map_err(|error| VerificationError::Other(error.to_string()))?;
        if timestamp.as_secs() < release {
            return Err(VerificationError::Other(
                "verification vote predates its selection beacon".into(),
            ));
        }
        Ok(())
    }

    /// Check eligibility without consuming a vote or changing stake state.
    pub fn validate_vote(
        &self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
    ) -> Result<(), VerificationError> {
        let state = self.states.get(wallet).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {wallet}"))
        })?;
        if state.phase != VerificationPhase::Voting {
            return Err(VerificationError::Other(format!(
                "wallet {wallet} is not accepting votes"
            )));
        }
        if !state.selected_verifiers.contains(voter) {
            return Err(VerificationError::NotSelected(voter.to_string()));
        }
        if state.votes.iter().any(|vote| &vote.verifier == voter) {
            return Err(VerificationError::AlreadyVoted(voter.to_string()));
        }
        Ok(())
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
        self.process_vote_at(wallet, voter, vote, params, Timestamp::now())
    }

    pub fn process_vote_at(
        &mut self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
        vote: Vote,
        params: &ProtocolParams,
        now: Timestamp,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        if self.selections.contains_key(wallet) {
            return Err(VerificationError::Other(
                "this committee requires a context-bound vote".into(),
            ));
        }
        self.cast_vote_at(wallet, voter, vote, params, now)
    }

    pub fn process_certified_vote_at(
        &mut self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
        vote: Vote,
        context: BlockHash,
        params: &ProtocolParams,
        now: Timestamp,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        // The node authenticates a panel's activation and enforces its deadline.
        // Challenge admission alone cannot start a certified voting window.
        let selection_params = self
            .selection_request(wallet)
            .ok_or_else(|| {
                VerificationError::Other("verification committee is not certified".into())
            })?
            .params_hash();
        self.validate_certified_vote(wallet, voter, context, selection_params, now)?;
        self.attempts.bind_vote(wallet, voter, context)?;
        self.cast_vote_at(wallet, voter, vote, params, now)
    }

    fn cast_vote_at(
        &mut self,
        wallet: &WalletAddress,
        voter: &WalletAddress,
        vote: Vote,
        params: &ProtocolParams,
        now: Timestamp,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        self.validate_vote(wallet, voter)?;
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
            .cast_vote(state, voter.clone(), vote, stake, now)?;
        self.neither_tracker.configure(
            params.neither_penalty_threshold_bps,
            params.neither_window_assignments,
        );
        self.neither_tracker.record_vote(voter, vote);

        if self.neither_tracker.is_penalized(voter) {
            let now_secs = now.as_secs();
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
            // The vote still counts as Illegitimate. Applying a penalty must
            // not skip tallying when this was the last selected verifier.
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
                let mut outcomes = build_verification_outcomes(
                    wallet,
                    &result,
                    state,
                    self.prior_votes.remove(wallet).unwrap_or_default(),
                )?;
                self.attempts
                    .annotate_votes(wallet, &mut outcomes.verifiers);
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
                let mut outcomes = build_verification_outcomes(
                    wallet,
                    &result,
                    state,
                    self.prior_votes.remove(wallet).unwrap_or_default(),
                )?;
                self.attempts
                    .annotate_votes(wallet, &mut outcomes.verifiers);
                let event = VerificationEvent::VerificationComplete {
                    wallet: wallet.clone(),
                    result,
                    outcomes,
                };
                self.pending_events.push(event.clone());
                Ok(Some(event))
            }
            VotingOutcome::Revote => {
                self.prior_votes
                    .entry(wallet.clone())
                    .or_default()
                    .extend(state.votes.clone());
                self.voting.initiate_revote(state, params.max_revotes)?;
                self.selections.remove(wallet);
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
        reason: crate::challenge::ChallengeReason,
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        self.initiate_challenge_at(
            target,
            challenger,
            challenger_is_verified,
            stake,
            (reason, Timestamp::now()),
            params,
        )
    }

    pub fn initiate_signed_challenge_at(
        &mut self,
        target: &WalletAddress,
        challenger: &WalletAddress,
        challenger_is_verified: bool,
        stake: u128,
        event: (crate::challenge::ChallengeReason, Timestamp, BlockHash),
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        let (reason, now, context) = event;
        if context.is_zero() {
            return Err(VerificationError::Other(
                "challenge attempt context is missing".into(),
            ));
        }
        self.initiate_challenge_at(
            target,
            challenger,
            challenger_is_verified,
            stake,
            (reason, now),
            params,
        )?;
        self.attempts.bind_challenge(target, context);
        Ok(())
    }

    pub fn initiate_challenge_at(
        &mut self,
        target: &WalletAddress,
        challenger: &WalletAddress,
        challenger_is_verified: bool,
        stake: u128,
        event: (crate::challenge::ChallengeReason, Timestamp),
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        let (reason, now) = event;
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
                .initiate(challenger.clone(), target.clone(), stake, reason, now);

        self.active_challenges.insert(target.clone(), challenge);
        self.active_appeals.remove(target);
        state.phase = VerificationPhase::Challenged;
        state.excluded_verifiers.clear();
        state.selected_verifiers.clear();
        state.votes.clear();
        state.revote_count = 0;
        state.started_at = now;
        self.prior_votes.remove(target);
        self.selections.remove(target);
        self.attempts.clear(target);

        Ok(())
    }

    /// Resolve a challenge after re-verification votes are in.
    ///
    /// If fraud is confirmed the wallet is set to Unverified and a
    /// `WalletUnverified` event is emitted (the node uses this to trigger
    /// TRST revocation via the merger graph).
    /// Deactivate a wallet for benign (non-fraud) reasons — distinct from
    /// fraud revocation (whitepaper §Unverification Without Revocation).
    ///
    /// Emits `WalletDeactivated`: the node stops BRN accrual and removes
    /// transaction rights, but does NOT revoke originated TRST. The wallet
    /// can be re-verified later, resuming accrual without losing its earned
    /// balance.
    pub fn deactivate_wallet(&mut self, wallet: &WalletAddress) {
        self.pending_events
            .push(VerificationEvent::WalletDeactivated {
                wallet: wallet.clone(),
            });
    }

    /// Resolve a challenge if (and only if) one is active and every selected
    /// verifier has voted. Called by the node after each processed vote so a
    /// challenge finalizes the moment its last vote lands.
    pub fn try_resolve_challenge(
        &mut self,
        target: &WalletAddress,
        params: &ProtocolParams,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        if !self.active_challenges.contains_key(target) {
            return Ok(None);
        }
        let ready = self
            .states
            .get(target)
            .map(|st| {
                !st.selected_verifiers.is_empty() && st.votes.len() >= st.selected_verifiers.len()
            })
            .unwrap_or(false);
        if !ready {
            return Ok(None);
        }
        let state = self
            .states
            .get_mut(target)
            .ok_or_else(|| VerificationError::Other("challenge state missing".into()))?;
        if !challenge_is_decisive(state, params) {
            self.prior_votes
                .entry(target.clone())
                .or_default()
                .extend(state.votes.clone());
            self.voting.initiate_revote(state, params.max_revotes)?;
            self.selections.remove(target);
            return Ok(None);
        }
        self.resolve_challenge(target, params).map(Some)
    }

    /// A complete panel determines a verdict, not its ordering against ledger
    /// activity. Callers must agree that boundary before applying an upheld verdict.
    pub fn pending_challenge_verdict(
        &self,
        target: &WalletAddress,
        params: &ProtocolParams,
    ) -> Option<ChallengeResult> {
        self.active_challenges.get(target)?;
        let state = self.states.get(target)?;
        if state.selected_verifiers.is_empty()
            || state.votes.len() != state.selected_verifiers.len()
            || !challenge_is_decisive(state, params)
        {
            return None;
        }
        Some(
            if matches!(
                self.voting
                    .tally(state, params.verification_threshold_bps, params.max_revotes),
                VotingOutcome::Verified
            ) {
                ChallengeResult::ChallengeRejected
            } else {
                ChallengeResult::FraudConfirmed
            },
        )
    }

    /// Ordinary rejected challenges preserve identity and provenance. All appeal
    /// verdicts and upheld ordinary challenges await an agreed settlement cut.
    pub fn collect_challenge_outcome(
        &mut self,
        target: &WalletAddress,
        params: &ProtocolParams,
    ) -> Result<Option<VerificationEvent>, VerificationError> {
        if self.active_appeals.contains_key(target)
            || self.pending_challenge_verdict(target, params)
                == Some(ChallengeResult::FraudConfirmed)
        {
            return Ok(None);
        }
        self.try_resolve_challenge(target, params)
    }

    pub fn resolve_challenge(
        &mut self,
        target: &WalletAddress,
        params: &ProtocolParams,
    ) -> Result<VerificationEvent, VerificationError> {
        if !self.active_challenges.contains_key(target) {
            return Err(VerificationError::NoChallengeActive(target.to_string()));
        }
        let state = self.states.get_mut(target).ok_or_else(|| {
            VerificationError::Other(format!("no verification state for {target}"))
        })?;
        if state.selected_verifiers.is_empty()
            || state.votes.len() != state.selected_verifiers.len()
            || !challenge_is_decisive(state, params)
        {
            return Err(VerificationError::Other(
                "challenge requires completed votes or a revote".into(),
            ));
        }
        let challenge = self
            .active_challenges
            .remove(target)
            .ok_or_else(|| VerificationError::NoChallengeActive(target.to_string()))?;

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
        let prior_votes = self.prior_votes.remove(target).unwrap_or_default();
        let verifiers: Vec<(WalletAddress, u128, bool)> = state
            .votes
            .iter()
            .chain(prior_votes.iter())
            .map(|v| {
                let voted_correctly = if outcome_was_legitimate {
                    v.vote == Vote::Legitimate
                } else {
                    v.vote != Vote::Legitimate
                };
                (v.verifier.clone(), v.stake_amount, voted_correctly)
            })
            .collect();

        let mut outcome_event = compute_challenge_outcome(
            target,
            &challenge.challenger,
            challenge_result.clone(),
            challenge.stake_amount,
            &verifiers,
        )?;
        self.attempts.annotate_challenge(target, &mut outcome_event);
        outcome_event.appeal = self.active_appeals.remove(target);

        // "Upheld" means the wallet failed to re-clear the verification bar. What
        // happens next depends on WHY it was challenged (same procedure, different
        // consequence): Fraud → unverify + revoke TRST; Inactivity → deactivate
        // (benign) with NO revocation. The challenger is rewarded in both upheld
        // cases (compute_challenge_outcome returns the stake + pays correct
        // verifiers); only Fraud additionally pays the revoked-TRST bounty.
        if fraud_confirmed {
            match challenge.reason {
                crate::challenge::ChallengeReason::Fraud => {
                    state.phase = VerificationPhase::Unverified;
                    if outcome_event.appeal.is_none() {
                        self.pending_events
                            .push(VerificationEvent::WalletUnverified {
                                wallet: target.clone(),
                            });
                    }
                }
                crate::challenge::ChallengeReason::Inactivity => {
                    state.phase = VerificationPhase::Deactivated;
                    if outcome_event.appeal.is_none() {
                        self.pending_events
                            .push(VerificationEvent::WalletDeactivated {
                                wallet: target.clone(),
                            });
                    }
                }
            }
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
                endorser_set: std::collections::HashSet::new(),
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
        self.genesis_verify_at(
            wallet,
            genesis_creator,
            verified_wallet_count,
            bootstrap_threshold,
            Timestamp::now(),
        )
    }

    pub fn genesis_verify_at(
        &mut self,
        wallet: &WalletAddress,
        genesis_creator: &WalletAddress,
        verified_wallet_count: u64,
        bootstrap_threshold: u64,
        now: Timestamp,
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
                endorser_set: std::collections::HashSet::new(),
                selected_verifiers: Vec::new(),
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: std::collections::HashSet::new(),
                started_at: now,
            });

        state.phase = VerificationPhase::Verified;

        let result = VerificationResult::Verified;
        let mut outcomes = build_verification_outcomes(
            wallet,
            &result,
            state,
            self.prior_votes.remove(wallet).unwrap_or_default(),
        )?;
        self.attempts
            .annotate_votes(wallet, &mut outcomes.verifiers);
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

    /// Legacy admission-window predicate for expiry regression fixtures.
    /// Certified panel deadlines are enforced using their activation metadata.
    pub fn has_expired_challenges(&self, now: Timestamp, duration_secs: u64) -> bool {
        self.active_challenges.iter().any(|(target, challenge)| {
            now.as_secs()
                .saturating_sub(challenge.initiated_at.as_secs())
                >= duration_secs
                && !self.active_appeals.contains_key(target)
                && !self.timely_complete_challenge(target, duration_secs)
        })
    }

    fn timely_complete_challenge(&self, target: &WalletAddress, duration_secs: u64) -> bool {
        let Some(challenge) = self.active_challenges.get(target) else {
            return false;
        };
        self.states.get(target).is_some_and(|state| {
            !state.selected_verifiers.is_empty()
                && state.selected_verifiers.len() == state.votes.len()
                && state.selected_verifiers.iter().all(|selected| {
                    state
                        .votes
                        .iter()
                        .filter(|vote| &vote.verifier == selected)
                        .count()
                        == 1
                })
                && state.votes.iter().all(|vote| {
                    vote.timestamp.as_secs() >= challenge.initiated_at.as_secs()
                        && vote
                            .timestamp
                            .as_secs()
                            .saturating_sub(challenge.initiated_at.as_secs())
                            < duration_secs
                })
        })
    }

    /// Legacy admission-window check, not used for certified panel admission.
    pub fn validate_challenge_vote_time(
        &self,
        wallet: &WalletAddress,
        timestamp: Timestamp,
        duration_secs: u64,
    ) -> Result<(), VerificationError> {
        if self.active_challenges.get(wallet).is_some_and(|challenge| {
            timestamp
                .as_secs()
                .saturating_sub(challenge.initiated_at.as_secs())
                >= duration_secs
        }) {
            return Err(VerificationError::Other(
                "challenge vote is at or after its deadline; agreed timeout settlement is required"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Legacy, gated expiry calculation on candidate state. This is not the
    /// agreed missing-verifier completion required by decision 15.16.
    ///
    /// An incomplete challenge past `duration_secs` resolves in favor of the
    /// target. The challenger forfeits half the stake; cast-vote stakes return
    /// without rewards or penalties. A complete timely panel is preserved.
    /// Calling this with a local clock does not authorize live settlement.
    ///
    /// Events are returned directly, not also queued in `pending_events`.
    pub fn cleanup_expired_challenges(
        &mut self,
        now: Timestamp,
        duration_secs: u64,
    ) -> Result<Vec<VerificationEvent>, VerificationError> {
        let now_secs = now.as_secs();
        let expired: Vec<WalletAddress> = self
            .active_challenges
            .iter()
            .filter(|(target, c)| {
                now_secs.saturating_sub(c.initiated_at.as_secs()) >= duration_secs
                    && !self.active_appeals.contains_key(*target)
                    && !self.timely_complete_challenge(target, duration_secs)
            })
            .map(|(target, _)| target.clone())
            .collect();

        let mut events = Vec::new();
        for target in expired {
            let challenge = self.active_challenges.remove(&target).unwrap();
            self.selections.remove(&target);

            // All cast votes count as "correct" so stakes unlock; with no
            // dissenters the forfeited pool is zero, so no TRST rewards.
            let prior_votes = self.prior_votes.remove(&target).unwrap_or_default();
            let voters: Vec<(WalletAddress, u128, bool)> = self
                .states
                .get(&target)
                .map(|st| {
                    st.votes
                        .iter()
                        .chain(prior_votes.iter())
                        .map(|v| (v.verifier.clone(), v.stake_amount, true))
                        .collect()
                })
                .unwrap_or_default();

            if let Some(state) = self.states.get_mut(&target) {
                state.phase = VerificationPhase::Verified;
            }

            let mut outcome_event = compute_challenge_outcome(
                &target,
                &challenge.challenger,
                ChallengeResult::Expired,
                challenge.stake_amount,
                &voters,
            )?;
            self.attempts
                .annotate_challenge(&target, &mut outcome_event);

            events.push(VerificationEvent::ChallengeResolved {
                wallet: target,
                outcome: outcome_event,
            });
        }
        Ok(events)
    }

    /// Get the verification state of a wallet.
    pub fn get_state(&self, wallet: &WalletAddress) -> Option<&VerificationState> {
        self.states.get(wallet)
    }

    /// Read current processes without cloning the persisted engine snapshot.
    pub fn states(&self) -> impl Iterator<Item = &VerificationState> {
        self.states.values()
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
            active_appeals: self.active_appeals.clone(),
            prior_votes: self.prior_votes.clone(),
            selections: self.selections.clone(),
            attempts: self.attempts.clone(),
            penalized_verifiers: self.penalized_verifiers.clone(),
            neither_tracker: self.neither_tracker.clone(),
        }
    }

    /// Restore in-flight verification state from a persisted snapshot.
    pub fn restore(snapshot: OrchestratorSnapshot) -> Self {
        Self {
            endorsement: EndorsementEngine,
            voting: VerificationVoting,
            challenges: ChallengeEngine,
            neither_tracker: snapshot.neither_tracker,
            states: snapshot.states,
            active_challenges: snapshot.active_challenges,
            active_appeals: snapshot.active_appeals,
            prior_votes: snapshot.prior_votes,
            selections: snapshot.selections,
            attempts: snapshot.attempts,
            penalized_verifiers: snapshot.penalized_verifiers,
            pending_events: Vec::new(),
        }
    }
}

/// Serializable snapshot of orchestrator state for persistence across restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorSnapshot {
    #[serde(serialize_with = "crate::state::ordered_wallet_map")]
    pub states: HashMap<WalletAddress, VerificationState>,
    #[serde(serialize_with = "crate::state::ordered_wallet_map")]
    pub active_challenges: HashMap<WalletAddress, Challenge>,
    #[serde(serialize_with = "crate::state::ordered_wallet_map")]
    pub penalized_verifiers: HashMap<WalletAddress, u64>,
    pub neither_tracker: NeitherVoteTracker,
    #[serde(serialize_with = "crate::state::ordered_vote_map")]
    pub prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
    #[serde(serialize_with = "crate::state::ordered_wallet_map")]
    selections: HashMap<WalletAddress, SelectionRound>,
    attempts: AttemptContexts,
    #[serde(serialize_with = "crate::state::ordered_wallet_map")]
    active_appeals: HashMap<WalletAddress, AppealContext>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SelectionRound {
    request: SelectionRequest,
    // Authorization is checked before installation. Signer subsets and agreement
    // rounds are not economic state and must not change its canonical hash.
    certified: Option<(Checkpoint, RandomOutput)>,
}

#[derive(Deserialize)]
struct LegacySelectionRound {
    request: SelectionRequest,
    certified: Option<(CheckpointCertificate, RandomOutput)>,
}

fn restore_legacy_selections(
    selections: HashMap<WalletAddress, LegacySelectionRound>,
) -> HashMap<WalletAddress, SelectionRound> {
    selections
        .into_iter()
        .map(|(wallet, selection)| {
            (
                wallet,
                SelectionRound {
                    request: selection.request,
                    certified: selection
                        .certified
                        .map(|(certificate, beacon)| (certificate.checkpoint, beacon)),
                },
            )
        })
        .collect()
}

impl OrchestratorSnapshot {
    const FORMAT: &'static [u8] = b"BURST-VERIFICATION\x08";

    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        let mut bytes = Self::FORMAT.to_vec();
        bytes.extend(bincode::serialize(self)?);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::Error> {
        use bincode::Options;
        let options = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes();
        if let Some(payload) = bytes.strip_prefix(Self::FORMAT) {
            let snapshot: Self = options.deserialize(payload)?;
            snapshot.validate_appeals()?;
            snapshot.neither_tracker.validate()?;
            return Ok(snapshot);
        }
        #[derive(Deserialize)]
        struct ProofSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: NeitherVoteTracker,
            prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
            selections: HashMap<WalletAddress, LegacySelectionRound>,
            attempts: AttemptContexts,
            active_appeals: HashMap<WalletAddress, AppealContext>,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x07") {
            let old: ProofSnapshot = options.deserialize(payload)?;
            let snapshot = Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker,
                prior_votes: old.prior_votes,
                selections: restore_legacy_selections(old.selections),
                attempts: old.attempts,
                active_appeals: old.active_appeals,
            };
            snapshot.validate_appeals()?;
            snapshot.neither_tracker.validate()?;
            return Ok(snapshot);
        }
        #[derive(Deserialize)]
        struct AppealSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: crate::voting::LegacyNeitherVoteTracker,
            prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
            selections: HashMap<WalletAddress, LegacySelectionRound>,
            attempts: AttemptContexts,
            active_appeals: HashMap<WalletAddress, AppealContext>,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x06") {
            let old: AppealSnapshot = options.deserialize(payload)?;
            let snapshot = Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker.into_rolling()?,
                prior_votes: old.prior_votes,
                selections: restore_legacy_selections(old.selections),
                attempts: old.attempts,
                active_appeals: old.active_appeals,
            };
            snapshot.validate_appeals()?;
            return Ok(snapshot);
        }
        #[derive(Deserialize)]
        struct ScopedSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: crate::voting::LegacyNeitherVoteTracker,
            prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
            selections: HashMap<WalletAddress, LegacySelectionRound>,
            attempts: AttemptContexts,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x05") {
            let old: ScopedSnapshot = options.deserialize(payload)?;
            return Ok(Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker.into_rolling()?,
                prior_votes: old.prior_votes,
                selections: restore_legacy_selections(old.selections),
                attempts: old.attempts,
                active_appeals: HashMap::new(),
            });
        }
        #[derive(Deserialize)]
        struct UnscopedSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: crate::voting::LegacyNeitherVoteTracker,
            prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
            selections: HashMap<WalletAddress, LegacySelectionRound>,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x04") {
            let old: UnscopedSnapshot = options.deserialize(payload)?;
            return Ok(Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker.into_rolling()?,
                prior_votes: old.prior_votes,
                selections: restore_legacy_selections(old.selections),
                attempts: AttemptContexts::default(),
                active_appeals: HashMap::new(),
            });
        }
        #[derive(Deserialize)]
        struct UnboundSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: crate::voting::LegacyNeitherVoteTracker,
            prior_votes: HashMap<WalletAddress, Vec<VerifierVote>>,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x03") {
            let old: UnboundSnapshot = options.deserialize(payload)?;
            return Ok(Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker.into_rolling()?,
                prior_votes: old.prior_votes,
                selections: HashMap::new(),
                attempts: AttemptContexts::default(),
                active_appeals: HashMap::new(),
            });
        }
        #[derive(Deserialize)]
        struct PreviousSnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
            neither_tracker: crate::voting::LegacyNeitherVoteTracker,
        }
        if let Some(payload) = bytes.strip_prefix(b"BURST-VERIFICATION\x02") {
            let old: PreviousSnapshot = options.deserialize(payload)?;
            if old
                .states
                .values()
                .any(|state| state.revote_count > 0 && state.phase == VerificationPhase::Voting)
            {
                return Err(Box::new(bincode::ErrorKind::Custom(
                    "old snapshot lost active revote stakes; replay is required".into(),
                )));
            }
            return Ok(Self {
                states: old.states,
                active_challenges: old.active_challenges,
                penalized_verifiers: old.penalized_verifiers,
                neither_tracker: old.neither_tracker.into_rolling()?,
                prior_votes: HashMap::new(),
                selections: HashMap::new(),
                attempts: AttemptContexts::default(),
                active_appeals: HashMap::new(),
            });
        }
        if bytes.starts_with(b"BURST-VERIFICATION") {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "unsupported verification snapshot version".into(),
            )));
        }
        // The legacy snapshot did not retain vote history. Preserve everything
        // it did store, without treating a damaged v2 snapshot as legacy data.
        #[derive(Deserialize)]
        struct LegacySnapshot {
            states: HashMap<WalletAddress, VerificationState>,
            active_challenges: HashMap<WalletAddress, Challenge>,
            penalized_verifiers: HashMap<WalletAddress, u64>,
        }
        let old: LegacySnapshot = options.deserialize(bytes)?;
        if old
            .states
            .values()
            .any(|state| state.revote_count > 0 && state.phase == VerificationPhase::Voting)
        {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "legacy snapshot lost active revote stakes; replay is required".into(),
            )));
        }
        Ok(Self {
            states: old.states,
            active_challenges: old.active_challenges,
            penalized_verifiers: old.penalized_verifiers,
            neither_tracker: NeitherVoteTracker::new(5000),
            prior_votes: HashMap::new(),
            selections: HashMap::new(),
            attempts: AttemptContexts::default(),
            active_appeals: HashMap::new(),
        })
    }
}

fn challenge_is_decisive(state: &VerificationState, params: &ProtocolParams) -> bool {
    if state.votes.is_empty() {
        return false;
    }
    let legitimate = state
        .votes
        .iter()
        .filter(|vote| vote.vote == Vote::Legitimate)
        .count() as u64;
    let illegitimate = state
        .votes
        .iter()
        .filter(|vote| vote.vote == Vote::Illegitimate)
        .count() as u64;
    let total = state.votes.len() as u64;
    legitimate * 10_000 >= total * params.verification_threshold_bps as u64
        || (illegitimate > 0
            && (total - legitimate) * 10_000 >= total * params.verification_threshold_bps as u64)
        || state.revote_count >= params.max_revotes
}

fn build_verification_outcomes(
    wallet: &WalletAddress,
    result: &VerificationResult,
    state: &VerificationState,
    prior_votes: Vec<VerifierVote>,
) -> Result<VerificationOutcomeEvent, VerificationError> {
    let endorsers: Vec<(WalletAddress, u128)> = state
        .endorsements
        .iter()
        .map(|e| (e.endorser.clone(), e.burn_amount))
        .collect();

    let outcome_was_legitimate = *result == VerificationResult::Verified;
    let verifiers: Vec<(WalletAddress, u128, bool)> = state
        .votes
        .iter()
        .chain(prior_votes.iter())
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
#[path = "orchestrator/challenge_selection_tests.rs"]
mod challenge_selection_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(s: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{}", &s.repeat(60 / s.len() + 1)[..60]))
    }

    fn test_params() -> ProtocolParams {
        ProtocolParams {
            endorsement_threshold: 3,
            endorsement_burn_amount: 1000,
            num_verifiers: 3,
            verification_threshold_bps: 6000, // 60% for easier testing
            verifier_stake_amount: 100,
            max_revotes: 3,
            challenge_stake_amount: 500,
            ..ProtocolParams::burst_defaults()
        }
    }

    #[test]
    fn endorsement_start_time_is_independent_of_arrival_order() {
        let params = test_params();
        let target = test_addr("target");
        let mut forward = VerificationOrchestrator::new();
        let mut reverse = VerificationOrchestrator::new();
        for index in 1..=3 {
            forward.process_endorsement_at(&target, &test_addr(&index.to_string()), 1000, &params, Timestamp::new(index * 10)).unwrap();
        }
        for index in (1..=3).rev() {
            reverse.process_endorsement_at(&target, &test_addr(&index.to_string()), 1000, &params, Timestamp::new(index * 10)).unwrap();
        }
        assert_eq!(forward.snapshot().encode().unwrap(), reverse.snapshot().encode().unwrap());
        assert_eq!(reverse.get_state(&target).unwrap().started_at, Timestamp::new(10));
        let before = reverse.snapshot().encode().unwrap();
        assert!(reverse.process_endorsement_at(&target, &test_addr("1"), 1000, &params, Timestamp::new(1)).is_err());
        assert_eq!(reverse.snapshot().encode().unwrap(), before, "rejected duplicate must not change the start time");
    }

    #[test]
    fn replay_freezes_the_certified_pool_without_granting_vote_authority() {
        let address = |seed| {
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
        };
        let params = test_params();
        let target = address(40);
        let pool: Vec<_> = (50..56).map(address).collect();
        let mut original = VerificationOrchestrator::new();
        for seed in 1..=3 {
            original
                .process_endorsement_at(
                    &target,
                    &address(seed),
                    params.endorsement_burn_amount,
                    &params,
                    Timestamp::new(1_595_434_000),
                )
                .unwrap();
        }
        let mut replayed = original.clone();
        let cutoff = Timestamp::new(1_595_434_050);
        let request = original
            .prepare_selection(&target, BlockHash::new([7; 32]), cutoff, &pool, &params)
            .unwrap();
        replayed
            .penalized_verifiers
            .insert(pool[0].clone(), cutoff.as_secs() + 1000);
        let wrong_attempt = SelectionRequest::new(
            target.clone(),
            BlockHash::new([7; 32]),
            1,
            cutoff.as_secs(),
            params.params_hash(),
            pool.clone(),
            params.num_verifiers,
        )
        .unwrap();
        assert!(replayed
            .freeze_replayed_selection(&wrong_attempt, &params)
            .is_err());
        assert!(replayed.selection_request(&target).is_none());
        replayed
            .freeze_replayed_selection(&request, &params)
            .unwrap();
        assert_eq!(replayed.selection_request(&target), Some(&request));
        assert!(replayed.selection_context(&target).is_none());
        assert!(replayed
            .get_state(&target)
            .unwrap()
            .selected_verifiers
            .is_empty());
        assert!(replayed
            .process_vote_at(&target, &pool[1], Vote::Legitimate, &params, cutoff)
            .is_err());
        let replacement = SelectionRequest::new(
            target.clone(),
            BlockHash::new([8; 32]),
            0,
            cutoff.as_secs(),
            params.params_hash(),
            pool,
            params.num_verifiers,
        )
        .unwrap();
        assert!(replayed
            .freeze_replayed_selection(&replacement, &params)
            .is_err());
        assert_eq!(replayed.selection_request(&target), Some(&request));
        let mut changed_params = params;
        changed_params.verifier_stake_amount += 1;
        replayed
            .freeze_replayed_selection(&request, &changed_params)
            .unwrap();
        assert_eq!(replayed.selection_request(&target), Some(&request));
        let mut not_yet_frozen = replayed.clone();
        not_yet_frozen.selections.remove(&target);
        assert!(not_yet_frozen
            .freeze_replayed_selection(&request, &changed_params)
            .is_err());
    }

    #[test]
    fn v4_snapshots_preserve_state_but_do_not_invent_attempt_bindings() {
        let params = test_params();
        let target = test_addr("target");
        let challenger = test_addr("challenger");
        let mut orch = VerificationOrchestrator::new();
        orch.genesis_verify_at(&target, &test_addr("creator"), 1, 50, Timestamp::new(0))
            .unwrap();
        orch.drain_events();
        let context = BlockHash::new([17; 32]);
        orch.initiate_signed_challenge_at(
            &target,
            &challenger,
            true,
            500,
            (crate::ChallengeReason::Fraud, Timestamp::new(100), context),
            &params,
        )
        .unwrap();
        let mut old = b"BURST-VERIFICATION\x04".to_vec();
        old.extend(
            bincode::serialize(&(
                &orch.states,
                &orch.active_challenges,
                &orch.penalized_verifiers,
                &crate::voting::LegacyNeitherVoteTracker::empty(),
                &orch.prior_votes,
                &orch.selections,
            ))
            .unwrap(),
        );
        for (bytes, expected) in [
            (old, None),
            (orch.snapshot().encode().unwrap(), Some(context)),
        ] {
            let mut restored =
                VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
            assert_eq!(
                restored.snapshot().active_challenges[&target].challenger,
                challenger
            );
            let events = restored
                .cleanup_expired_challenges(Timestamp::new(200), 100)
                .unwrap();
            let VerificationEvent::ChallengeResolved { outcome, .. } = &events[0] else {
                panic!("missing expiry")
            };
            assert_eq!(outcome.challenge_context, expected);
            assert_eq!(outcome.challenger_stake, 500);
            assert_eq!(outcome.outcome, ChallengeResult::Expired);
        }
    }

    #[test]
    fn normal_endorsements_require_the_governed_burn_before_creating_state() {
        let mut orch = VerificationOrchestrator::new();
        let mut params = ProtocolParams::default();
        let target = test_addr("target");
        let first = test_addr("first");
        let second = test_addr("second");
        for amount in [0, 1, params.endorsement_burn_amount - 1] {
            assert!(matches!(
                orch.process_endorsement_at(&target, &first, amount, &params, Timestamp::new(10)),
                Err(VerificationError::InsufficientEndorsementBurn { .. })
            ));
            assert!(orch.get_state(&target).is_none());
            assert!(orch.drain_events().is_empty());
        }
        orch.process_endorsement_at(
            &target,
            &first,
            params.endorsement_burn_amount,
            &params,
            Timestamp::new(10),
        )
        .unwrap();
        params.endorsement_burn_amount += 1;
        assert!(orch
            .process_endorsement_at(
                &target,
                &second,
                params.endorsement_burn_amount - 1,
                &params,
                Timestamp::new(11)
            )
            .is_err());
        assert_eq!(orch.get_state(&target).unwrap().endorsements.len(), 1);
        orch.process_endorsement_at(
            &target,
            &second,
            params.endorsement_burn_amount + 1,
            &params,
            Timestamp::new(11),
        )
        .unwrap();
        assert_eq!(
            orch.get_state(&target).unwrap().endorsements.len(),
            2,
            "extra burn does not buy extra endorsements"
        );
    }

    #[test]
    fn signed_time_drives_penalties_and_challenges_without_wall_clock_reads() {
        let params = test_params();
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        endorse_wallet(&mut orch, &target, &params);
        let pool = [test_addr("v1"), test_addr("v2"), test_addr("v3")];
        let selected = orch
            .select_verifiers_at(&target, &pool, &[1; 32], &params, Timestamp::new(100))
            .unwrap();
        orch.process_vote_at(
            &target,
            &selected[0],
            Vote::Neither,
            &params,
            Timestamp::new(123),
        )
        .unwrap();
        assert_eq!(
            orch.get_state(&target).unwrap().votes[0].timestamp,
            Timestamp::new(123)
        );
        assert_eq!(
            orch.snapshot().penalized_verifiers[&selected[0]],
            123 + params.neither_penalty_cooldown_secs
        );
        let challenged = test_addr("other");
        orch.genesis_verify_at(
            &challenged,
            &test_addr("creator"),
            1,
            50,
            Timestamp::new(10),
        )
        .unwrap();
        orch.initiate_challenge_at(
            &challenged,
            &test_addr("challenger"),
            true,
            500,
            (crate::ChallengeReason::Fraud, Timestamp::new(200)),
            &params,
        )
        .unwrap();
        assert_eq!(
            orch.snapshot().active_challenges[&challenged].initiated_at,
            Timestamp::new(200)
        );
        assert!(orch.resolve_challenge(&challenged, &params).is_err());
        assert!(orch.snapshot().active_challenges.contains_key(&challenged));
    }

    #[test]
    fn inconclusive_challenge_requires_a_new_panel_and_keeps_every_stake() {
        let mut params = test_params();
        params.verification_threshold_bps = 9000;
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        orch.genesis_verify_at(&target, &test_addr("creator"), 1, 50, Timestamp::new(0))
            .unwrap();
        orch.initiate_challenge_at(
            &target,
            &test_addr("challenger"),
            true,
            500,
            (crate::ChallengeReason::Fraud, Timestamp::new(100)),
            &params,
        )
        .unwrap();
        let pool = [test_addr("v1"), test_addr("v2"), test_addr("v3")];
        let selected = orch
            .select_verifiers_at(&target, &pool, &[1; 32], &params, Timestamp::new(101))
            .unwrap();
        for (i, voter) in selected.iter().enumerate() {
            let vote = if i == 0 {
                Vote::Legitimate
            } else {
                Vote::Illegitimate
            };
            orch.process_vote_at(&target, voter, vote, &params, Timestamp::new(102))
                .unwrap();
        }
        assert!(orch.resolve_challenge(&target, &params).is_err());
        assert!(orch
            .try_resolve_challenge(&target, &params)
            .unwrap()
            .is_none());
        let encoded = orch.snapshot().encode().unwrap();
        let mut orch =
            VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&encoded).unwrap());
        assert_eq!(orch.snapshot().prior_votes[&target].len(), 3);
        assert_eq!(orch.get_state(&target).unwrap().revote_count, 1);
        assert!(orch
            .select_verifiers_at(&target, &pool, &[2; 32], &params, Timestamp::new(103))
            .is_err());
        let pool = [test_addr("v4"), test_addr("v5"), test_addr("v6")];
        let selected = orch
            .select_verifiers_at(&target, &pool, &[2; 32], &params, Timestamp::new(103))
            .unwrap();
        for voter in &selected {
            orch.process_vote_at(
                &target,
                voter,
                Vote::Legitimate,
                &params,
                Timestamp::new(104),
            )
            .unwrap();
        }
        let Some(VerificationEvent::ChallengeResolved { outcome, .. }) =
            orch.try_resolve_challenge(&target, &params).unwrap()
        else {
            panic!("missing resolution")
        };
        assert_eq!(outcome.outcome, ChallengeResult::ChallengeRejected);
        assert_eq!(outcome.verifier_outcomes.len(), 6);
        assert_eq!(
            outcome
                .verifier_outcomes
                .iter()
                .map(|v| v.penalty)
                .sum::<u128>(),
            200
        );
        assert_eq!(
            outcome
                .verifier_outcomes
                .iter()
                .map(|v| v.trst_reward)
                .sum::<u128>(),
            200
        );
    }

    #[test]
    fn old_snapshots_cannot_silently_drop_unsettled_revote_stakes() {
        let params = test_params();
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        endorse_wallet(&mut orch, &target, &params);
        let pool = [test_addr("v1"), test_addr("v2"), test_addr("v3")];
        let selected = orch
            .select_verifiers(&target, &pool, &[1; 32], &params)
            .unwrap();
        for voter in &selected {
            orch.process_vote(&target, voter, Vote::Illegitimate, &params)
                .unwrap();
        }
        let mut old = b"BURST-VERIFICATION\x02".to_vec();
        old.extend(
            bincode::serialize(&(
                &orch.states,
                &orch.active_challenges,
                &orch.penalized_verifiers,
                &orch.neither_tracker,
            ))
            .unwrap(),
        );
        assert!(OrchestratorSnapshot::decode(&old).is_err());
        assert_eq!(
            OrchestratorSnapshot::decode(&orch.snapshot().encode().unwrap())
                .unwrap()
                .prior_votes[&target]
                .len(),
            3
        );
    }

    #[test]
    fn persistence_retains_neither_history_and_active_votes() {
        let params = test_params();
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        endorse_wallet(&mut orch, &target, &params);
        let pool: Vec<_> = (1..=3).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = orch
            .select_verifiers(&target, &pool, &[1; 32], &params)
            .unwrap();
        orch.neither_tracker
            .record_vote(&selected[0], Vote::Neither);
        orch.process_vote(&target, &selected[0], Vote::Legitimate, &params)
            .unwrap();
        let bytes = orch.snapshot().encode().unwrap();
        let restored =
            VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
        assert_eq!(restored.neither_tracker.total_assignments(&selected[0]), 2);
        assert_eq!(restored.neither_tracker.neither_count(&selected[0]), 1);
        assert_eq!(
            restored.get_state(&target).unwrap().selected_verifiers,
            selected
        );
        assert_eq!(restored.get_state(&target).unwrap().votes.len(), 1);
        assert!(matches!(
            restored.validate_vote(&target, &selected[0]),
            Err(VerificationError::AlreadyVoted(_))
        ));
        assert!(OrchestratorSnapshot::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(OrchestratorSnapshot::decode(&trailing).is_err());
        let mut unknown_version = bytes;
        unknown_version[OrchestratorSnapshot::FORMAT.len() - 1] = 255;
        assert!(OrchestratorSnapshot::decode(&unknown_version).is_err());
    }

    #[test]
    fn legacy_persistence_preserves_verification_and_cooldowns() {
        let params = test_params();
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        verify_wallet(&mut orch, &target, &params);
        orch.penalized_verifiers
            .insert(test_addr("penalized"), u64::MAX);
        // Bincode encodes a tuple and a struct with these three fields identically.
        let bytes = bincode::serialize(&(
            &orch.states,
            &orch.active_challenges,
            &orch.penalized_verifiers,
        ))
        .unwrap();
        let restored =
            VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
        assert_eq!(
            restored.get_state(&target).unwrap().phase,
            VerificationPhase::Verified
        );
        assert_eq!(restored.penalized_count(), 1);
        assert_eq!(restored.neither_tracker.tracked_count(), 0);
    }

    #[test]
    fn selection_requires_completed_endorsement_and_an_independent_full_pool() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let target = test_addr("target");
        let pool: Vec<_> = (1..=3).map(|i| test_addr(&format!("v{i}"))).collect();
        orch.process_endorsement(&target, &test_addr("e1"), 1000, &params)
            .unwrap();
        assert!(matches!(
            orch.select_verifiers(&target, &pool, &[0; 32], &params),
            Err(VerificationError::ThresholdNotMet { .. })
        ));
        for endorser in ["e2", "e3"] {
            orch.process_endorsement(&target, &test_addr(endorser), 1000, &params)
                .unwrap();
        }
        let mut contaminated = vec![
            target.clone(),
            test_addr("e1"),
            pool[0].clone(),
            pool[0].clone(),
        ];
        assert!(orch
            .select_verifiers(&target, &contaminated, &[0; 32], &params)
            .is_err());
        assert_eq!(
            orch.get_state(&target).unwrap().phase,
            VerificationPhase::Endorsing
        );
        contaminated.extend(pool.clone());
        let selected = orch
            .select_verifiers(&target, &contaminated, &[0; 32], &params)
            .unwrap();
        assert_eq!(selected.len(), 3);
        assert!(pool.iter().all(|verifier| selected.contains(verifier)));
    }

    #[test]
    fn reselection_cannot_discard_votes_and_pool_order_does_not_change_selection() {
        let params = test_params();
        let target = test_addr("target");
        let mut a = VerificationOrchestrator::new();
        let mut b = VerificationOrchestrator::new();
        endorse_wallet(&mut a, &target, &params);
        endorse_wallet(&mut b, &target, &params);
        let mut pool: Vec<_> = (1..=6).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = a
            .select_verifiers(&target, &pool, &[1; 32], &params)
            .unwrap();
        pool.reverse();
        assert_eq!(
            selected,
            b.select_verifiers(&target, &pool, &[1; 32], &params)
                .unwrap()
        );
        a.process_vote(&target, &selected[0], Vote::Legitimate, &params)
            .unwrap();
        assert!(a
            .select_verifiers(&target, &pool, &[2; 32], &params)
            .is_err());
        let state = a.get_state(&target).unwrap();
        assert_eq!(state.selected_verifiers, selected);
        assert_eq!(state.votes.len(), 1);
    }

    #[test]
    fn penalized_final_neither_vote_still_resolves_verification() {
        let params = test_params();
        let target = test_addr("target");
        let mut orch = VerificationOrchestrator::new();
        endorse_wallet(&mut orch, &target, &params);
        let pool: Vec<_> = (1..=3).map(|i| test_addr(&format!("v{i}"))).collect();
        let selected = orch
            .select_verifiers(&target, &pool, &[1; 32], &params)
            .unwrap();
        for voter in &selected[..2] {
            assert!(orch
                .process_vote(&target, voter, Vote::Legitimate, &params)
                .unwrap()
                .is_none());
        }
        let event = orch
            .process_vote(&target, &selected[2], Vote::Neither, &params)
            .unwrap();
        assert!(matches!(
            event,
            Some(VerificationEvent::VerificationComplete {
                result: VerificationResult::Verified,
                ..
            })
        ));
        assert_eq!(orch.penalized_count(), 1);
        assert_eq!(orch.get_state(&target).unwrap().votes.len(), 3);
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
        orch.initiate_challenge(
            &wallet,
            &challenger,
            true,
            500,
            crate::challenge::ChallengeReason::Fraud,
            &params,
        )
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
                assert_eq!(outcome.challenger_stake, 500);
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
        orch.initiate_challenge(
            &wallet,
            &challenger,
            true,
            500,
            crate::challenge::ChallengeReason::Fraud,
            &params,
        )
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
                assert_eq!(outcome.outcome, ChallengeResult::ChallengeRejected);
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

        let result = orch.initiate_challenge(
            &wallet,
            &test_addr("c"),
            true,
            500,
            crate::challenge::ChallengeReason::Fraud,
            &params,
        );
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
        let result = orch.initiate_challenge(
            &wallet,
            &unverified_challenger,
            false,
            500,
            crate::challenge::ChallengeReason::Fraud,
            &params,
        );
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
        assert!(result.is_ok(), "a penalized Neither vote must still count");

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
                assert_eq!(eo.brn_burned, 1000);
            }
        }
    }

    #[test]
    fn deactivate_wallet_emits_event_without_revocation() {
        let mut orch = VerificationOrchestrator::new();
        let wallet = WalletAddress::new("brst_deactivate_me");
        orch.deactivate_wallet(&wallet);
        let events = orch.drain_events();
        assert!(matches!(
            &events[..],
            [VerificationEvent::WalletDeactivated { wallet: w }] if *w == wallet
        ));
    }

    #[test]
    fn try_resolve_challenge_waits_for_all_votes() {
        let params = test_params();
        let mut orch = VerificationOrchestrator::new();
        let target = test_addr("challenged");
        let challenger = test_addr("challenger");

        // Set up a verified wallet, then challenge it.
        verify_wallet(&mut orch, &target, &params);
        orch.initiate_challenge(
            &target,
            &challenger,
            true,
            1000,
            crate::challenge::ChallengeReason::Fraud,
            &params,
        )
        .unwrap();
        let verifiers: Vec<WalletAddress> = (0..params.num_verifiers)
            .map(|i| test_addr(&format!("cv{i}")))
            .collect();
        let rand = [7u8; 32];
        orch.select_verifiers(&target, &verifiers, &rand, &params)
            .unwrap();
        orch.drain_events();

        let selected = orch.get_state(&target).unwrap().selected_verifiers.clone();

        // Not all votes in — must NOT resolve.
        orch.process_vote(&target, &selected[0], Vote::Illegitimate, &params)
            .unwrap();
        assert!(orch
            .try_resolve_challenge(&target, &params)
            .unwrap()
            .is_none());

        // Remaining votes land — resolves on the last one.
        for v in selected.iter().skip(1) {
            orch.process_vote(&target, v, Vote::Illegitimate, &params)
                .unwrap();
        }
        let resolved = orch.try_resolve_challenge(&target, &params).unwrap();
        assert!(resolved.is_some());
        // Unanimous Illegitimate → fraud confirmed → WalletUnverified queued.
        let events = orch.drain_events();
        assert!(events.iter().any(
            |e| matches!(e, VerificationEvent::WalletUnverified { wallet } if *wallet == target)
        ));
        // Second call is a no-op (challenge consumed).
        assert!(orch
            .try_resolve_challenge(&target, &params)
            .unwrap()
            .is_none());
    }

    #[test]
    fn inactivity_challenge_deactivates_without_revoking() {
        let mut orch = VerificationOrchestrator::new();
        let params = test_params();
        let target = test_addr("inactive");
        let challenger = test_addr("hunter");

        verify_wallet(&mut orch, &target, &params);
        // Same procedure, but the reason is benign Inactivity, not Fraud.
        orch.initiate_challenge(
            &target,
            &challenger,
            true,
            1000,
            crate::challenge::ChallengeReason::Inactivity,
            &params,
        )
        .unwrap();
        let verifiers: Vec<WalletAddress> = (0..params.num_verifiers)
            .map(|i| test_addr(&format!("iv{i}")))
            .collect();
        orch.select_verifiers(&target, &verifiers, &[9u8; 32], &params)
            .unwrap();
        orch.drain_events();
        let selected = orch.get_state(&target).unwrap().selected_verifiers.clone();

        // Verifiers agree the wallet is no longer an active human → challenge
        // upheld → benign DEACTIVATION (not fraud revocation).
        for v in &selected {
            orch.process_vote(&target, v, Vote::Illegitimate, &params)
                .unwrap();
        }
        assert!(orch
            .try_resolve_challenge(&target, &params)
            .unwrap()
            .is_some());

        let events = orch.drain_events();
        // Upheld inactivity challenge → WalletDeactivated, NOT WalletUnverified.
        assert!(events.iter().any(
            |e| matches!(e, VerificationEvent::WalletDeactivated { wallet } if *wallet == target)
        ));
        assert!(!events
            .iter()
            .any(|e| matches!(e, VerificationEvent::WalletUnverified { .. })));
        assert_eq!(
            orch.get_state(&target).unwrap().phase,
            VerificationPhase::Deactivated
        );
    }
}
