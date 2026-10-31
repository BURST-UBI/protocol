//! Replay-authorized checkpoint agreement for one height. Networking supplies
//! bounded messages; this driver never signs from an advertised roster or pool.

use std::sync::Arc;

use burst_consensus::checkpoint::{CheckpointAnchor, CheckpointCertificate};
use burst_consensus::checkpoint_evidence::{FutureEvidence, RoundEvidence};
use burst_consensus::checkpoint_round::{Phase, Proposal, Quorum, RoundState, Step};
use burst_crypto::{derive_address, public_from_private};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{PrivateKey, ProtocolParams, WalletAddress};
use serde::{Deserialize, Serialize};

use crate::checkpoint_agreement::CompactReplayCandidate;
use crate::checkpoint_agreement::{self, Command, Outgoing, ReplayManifest, ReplayedCandidate};
use crate::checkpoint_replay::{ObservedSigningClock, ReplayedCheckpoint, SigningClock};
use crate::checkpoint_store::CertifiedCheckpoint;
use crate::confirmed_replay::snapshot::CompactSelectionCoverage;

mod compact;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    Proposal {
        proposal: Box<Proposal>,
        manifest: ReplayManifest,
    },
    Vote(burst_consensus::checkpoint_round::SignedBallot),
    Prepared {
        proposal: Box<Proposal>,
        proof: Quorum,
        manifest: ReplayManifest,
    },
    Decision {
        certificate: CheckpointCertificate,
        manifest: ReplayManifest,
    },
    CatchUp {
        round: u64,
        evidence: Vec<burst_consensus::checkpoint_round::SignedBallot>,
    },
}

pub struct CheckpointDriver {
    store: Arc<LmdbStore>,
    params: ProtocolParams,
    key: Arc<PrivateKey>,
    representative: WalletAddress,
    anchor: CheckpointAnchor,
    initial: ReplayManifest,
    replays: Vec<ReplayedCandidate>,
    compact: Option<Vec<CompactReplayCandidate>>,
    round: u64,
    evidence: RoundEvidence,
    future: FutureEvidence,
    proposals: Vec<(Proposal, ReplayManifest)>,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl CheckpointDriver {
    pub fn new(
        store: Arc<LmdbStore>,
        params: ProtocolParams,
        key: impl Into<Arc<PrivateKey>>,
        initial: ReplayedCheckpoint,
    ) -> Result<Self, StoreError> {
        Self::new_candidate(
            store,
            params,
            key,
            ReplayedCandidate::Selection(Box::new(initial)),
        )
    }

    /// Outcome and roster decisions remain in the signer journal until the
    /// height runtime persists them. Neither write installs economic state.
    pub fn new_candidate(
        store: Arc<LmdbStore>,
        params: ProtocolParams,
        key: impl Into<Arc<PrivateKey>>,
        initial: ReplayedCandidate,
    ) -> Result<Self, StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(initial.checkpoint().network)
            && !matches!(initial, ReplayedCandidate::Selection(_))
        {
            return Err(invalid(
                "mixed checkpoint agreement is behind the release gate",
            ));
        }
        let key = key.into();
        let network = initial.checkpoint().network;
        let chain = crate::checkpoint_store::chain::load(&store, network, &params)?;
        let anchor = crate::checkpoint_store::chain::next_anchor(&chain, network, &params)?;
        initial.input().validate_parent(&anchor)?;
        let representative = derive_address(&public_from_private(&key));
        let round = checkpoint_agreement::read_state(&store, &anchor, &representative)?
            .map_or(0, |state| state.round());
        // Recovering signed choices is mandatory even if the caller supplied a
        // different valid candidate for this height.
        let mut replays =
            checkpoint_agreement::read_candidates(&store, &anchor, &representative, &params)?;
        let manifest = initial.manifest();
        if !replays.iter().any(|replay| replay.manifest() == manifest) {
            replays.push(initial);
        }
        let evidence = RoundEvidence::new(anchor.clone(), round);
        let future = FutureEvidence::new(anchor.clone());
        Ok(Self {
            store,
            params,
            key,
            representative,
            anchor,
            initial: manifest,
            replays,
            compact: None,
            round,
            evidence,
            future,
            proposals: Vec::new(),
        })
    }

    pub fn round(&self) -> u64 {
        self.round
    }

    pub(crate) fn anchor(&self) -> &CheckpointAnchor {
        &self.anchor
    }

    pub(crate) fn is_proposer(&self) -> bool {
        self.anchor.proposer(self.round) == &self.representative
    }

    /// Check evidence even for messages the state machine would ignore as old.
    /// An ignored message is not automatically safe to relay to other signers.
    pub(crate) fn authenticate(&self, message: &Message) -> Result<(), StoreError> {
        Self::authenticate_for(&self.anchor, message)
    }

    pub(crate) fn authenticate_for(
        anchor: &CheckpointAnchor,
        message: &Message,
    ) -> Result<(), StoreError> {
        match message {
            Message::Proposal { proposal, manifest }
            | Message::Prepared {
                proposal, manifest, ..
            } => {
                proposal.verify(anchor).map_err(invalid)?;
                if &proposal.body.checkpoint != manifest.checkpoint() {
                    return Err(invalid("proposal manifest mismatch"));
                }
                if let Message::Prepared { proof, .. } = message {
                    proof.verify(anchor).map_err(invalid)?;
                    if proof.ballot.phase != Phase::Prevote
                        || proof.ballot.round != proposal.body.round
                        || proof.ballot.value != Some(proposal.body.checkpoint.hash())
                    {
                        return Err(invalid("prepare proof does not match proposal"));
                    }
                }
                Ok(())
            }
            Message::Vote(vote) => vote.verify(anchor).map_err(invalid),
            Message::Decision {
                certificate,
                manifest,
            } => {
                anchor.verify_certificate(certificate).map_err(invalid)?;
                if &certificate.checkpoint != manifest.checkpoint() {
                    return Err(invalid("decision manifest mismatch"));
                }
                Ok(())
            }
            Message::CatchUp { round, evidence } => {
                let mut state =
                    RoundState::new(anchor, anchor.proposer(0).clone()).map_err(invalid)?;
                state
                    .catch_up_round(anchor, *round, evidence)
                    .map_err(invalid)
            }
        }
    }

    pub fn state(&self) -> Result<Option<RoundState>, StoreError> {
        checkpoint_agreement::read_state(&self.store, &self.anchor, &self.representative)
    }

    /// Runtime recovery may find a journaled decision before its chain record.
    /// Persisting it still does not install economic state or clear vote fences.
    pub(crate) fn persist_decision(&self) -> Result<bool, StoreError> {
        let Some(state) = self.state()? else {
            return Ok(false);
        };
        let Some(certificate) = state.decision() else {
            return Ok(false);
        };
        if let Some(candidates) = &self.compact {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.input().checkpoint() == &certificate.checkpoint)
                .ok_or_else(|| invalid("compact decision has no replay coverage"))?;
            crate::checkpoint_store::compact::accept_candidate(
                &self.store,
                &self.anchor,
                candidate.input(),
                certificate,
            )?;
            return Ok(true);
        }
        let manifest = self.manifest(&certificate.checkpoint)?;
        crate::checkpoint_store::chain::accept_manifest(
            &self.store,
            self.anchor.network(),
            &self.params,
            certificate.clone(),
            &manifest,
        )?;
        Ok(true)
    }

    pub fn propose(&mut self, clock: Option<SigningClock>) -> Result<Vec<Message>, StoreError> {
        let clock = clock.map(ObservedSigningClock::new);
        let candidate = self.initial.clone();
        let output = self.apply(
            Command::Propose(candidate.checkpoint().clone()),
            &candidate,
            clock,
        )?;
        let mut messages = self.emit(output)?;
        if let Some(proposal) = messages
            .iter()
            .find(|message| matches!(message, Message::Proposal { .. }))
            .cloned()
        {
            messages.extend(self.receive_observed(proposal, clock)?);
        }
        Ok(messages)
    }

    /// An expired, unprepared proposal is not a certified selection. A later
    /// consensus round may propose a newly replayed cut before its beacon.
    /// This changes only the proposal cache, never durable votes or locks.
    pub(crate) fn refresh_candidate(
        &mut self,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        if self.compact.is_some() {
            // Compact replacement requires async proof retrieval. Retain the
            // existing candidate and its deadline until that replay is ready.
            return Ok(());
        }
        self.refresh_activation(clock)?;
        self.refresh_unsigned_lifecycle(clock)?;
        let Some(clock) = clock else { return Ok(()) };
        if self.round == 0
            || self.state()?.is_some_and(|state| {
                state.step() != Step::Propose
                    || state.proposal().is_some()
                    || state.valid_proof().is_some()
                    || state.decision().is_some()
            })
        {
            return Ok(());
        }
        let Some(ReplayedCandidate::Selection(previous)) = self
            .replays
            .iter()
            .find(|candidate| candidate.manifest() == self.initial)
        else {
            return Ok(());
        };
        let release = previous.request().beacon_time().map_err(invalid)?;
        let Some(earliest) = clock.now.checked_sub(clock.error_bound) else {
            return Ok(());
        };
        // Do not discard a still-usable window, including when uncertainty
        // straddles its end. The clock must establish expiry, not just lateness.
        if earliest < release {
            return Ok(());
        }
        let Some(cutoff) = earliest.checked_sub(1) else {
            return Ok(());
        };
        let Some(candidate) = CertifiedCheckpoint::prepare_next(
            &self.store,
            self.anchor.network(),
            &self.params,
            cutoff,
        )?
        else {
            return Ok(());
        };
        candidate.validate_parent(&self.anchor)?;
        candidate
            .request()
            .validate_before_signing(candidate.checkpoint(), clock.now, clock.error_bound)
            .map_err(invalid)?;
        self.initial = ReplayManifest::Selection(candidate.manifest());
        if !self
            .replays
            .iter()
            .any(|saved| saved.manifest() == self.initial)
        {
            self.replays
                .push(ReplayedCandidate::Selection(Box::new(candidate)));
        }
        self.prune_replays()
    }

    /// Roster, outcome, deadline and governance candidates can become stale
    /// while payment final votes advance. Refresh only before any proposal or
    /// prepared value exists; all durable vote coverage checks still apply.
    fn refresh_unsigned_lifecycle(
        &mut self,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        let Some(clock) = clock else { return Ok(()) };
        if matches!(
            self.initial,
            ReplayManifest::Selection(_) | ReplayManifest::Activation(_)
        ) || self.state()?.is_some_and(|state| {
            state.step() != Step::Propose
                || state.proposal().is_some()
                || state.valid_proof().is_some()
                || state.decision().is_some()
        }) {
            return Ok(());
        }
        let Some(candidate) = crate::checkpoint_service::discover_candidate(
            &self.store,
            self.anchor.network(),
            &self.params,
            clock,
        )?
        else {
            return Ok(());
        };
        candidate.input().validate_parent(&self.anchor)?;
        self.initial = candidate.manifest();
        if !self
            .replays
            .iter()
            .any(|saved| saved.manifest() == self.initial)
        {
            self.replays.push(candidate);
        }
        self.prune_replays()
    }

    /// An unsigned start can include new confirmed history in a later safe cut.
    /// Signed proposals and prepared values stay under the durable round rules.
    pub(crate) fn refresh_activation(
        &mut self,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        let Some(clock) = clock else { return Ok(()) };
        if self.state()?.is_some_and(|state| {
            state.step() != Step::Propose
                || state.proposal().is_some()
                || state.valid_proof().is_some()
                || state.decision().is_some()
        }) {
            return Ok(());
        }
        let Some(ReplayedCandidate::Activation(candidate)) = self
            .replays
            .iter()
            .find(|candidate| candidate.manifest() == self.initial)
        else {
            return Ok(());
        };
        let Some(cutoff) = clock
            .now
            .checked_sub(clock.error_bound)
            .and_then(|time| time.checked_sub(1))
        else {
            return Ok(());
        };
        if cutoff <= candidate.manifest().checkpoint.cutoff {
            return Ok(());
        }
        // Early votes may have been finalized since discovery. Waiting for the
        // old start to expire leaves their signer fences blocking this proposal.
        let candidate =
            if let Some(candidate) = candidate.prepare_later(&self.store, &self.params, cutoff)? {
                candidate.validate_fresh_time(clock)?;
                ReplayedCandidate::Activation(Box::new(candidate))
            } else if let Some(candidate) =
                crate::checkpoint_outcome::PreparedChallengeOutcome::prepare_next(
                    &self.store,
                    self.anchor.network(),
                    &self.params,
                    cutoff,
                )?
            {
                ReplayedCandidate::ChallengeOutcome(Box::new(candidate))
            } else if let Some(candidate) = CertifiedCheckpoint::prepare_next(
                &self.store,
                self.anchor.network(),
                &self.params,
                clock.now,
            )? {
                ReplayedCandidate::Selection(Box::new(candidate))
            } else if let Some(candidate) = crate::checkpoint_roster::PreparedRoster::prepare_next(
                &self.store,
                self.anchor.network(),
                &self.params,
                cutoff,
            )? {
                ReplayedCandidate::Roster(Box::new(candidate))
            } else {
                return Ok(());
            };
        self.initial = candidate.manifest();
        if !self
            .replays
            .iter()
            .any(|saved| saved.manifest() == self.initial)
        {
            self.replays.push(candidate);
        }
        self.prune_replays()
    }

    pub fn resume(&mut self) -> Result<Vec<Message>, StoreError> {
        let candidate = self.initial.clone();
        let output = self.apply(Command::Resume, &candidate, None)?;
        self.emit(output)
    }

    pub fn receive(
        &mut self,
        message: Message,
        clock: Option<SigningClock>,
    ) -> Result<Vec<Message>, StoreError> {
        self.receive_observed(message, clock.map(ObservedSigningClock::new))
    }

    fn receive_observed(
        &mut self,
        message: Message,
        clock: Option<ObservedSigningClock>,
    ) -> Result<Vec<Message>, StoreError> {
        if self
            .state()?
            .is_some_and(|state| state.decision().is_some())
        {
            return Ok(Vec::new());
        }
        let mut output = match message {
            Message::Proposal { proposal, manifest } => {
                self.remember(&proposal, &manifest)?;
                let state = self.state()?;
                if state
                    .as_ref()
                    .is_none_or(|state| state.step() == Step::Propose && state.decision().is_none())
                {
                    let output = self.apply(Command::Proposal(&proposal), &manifest, clock)?;
                    self.emit(output)?
                } else {
                    Vec::new()
                }
            }
            Message::Vote(vote) => {
                if vote.ballot.round > self.round {
                    let Some((round, evidence)) =
                        self.future.insert(vote, self.round).map_err(invalid)?
                    else {
                        return Ok(Vec::new());
                    };
                    return self.receive_observed(Message::CatchUp { round, evidence }, clock);
                }
                if vote.ballot.round < self.round {
                    vote.verify(&self.anchor).map_err(invalid)?;
                    return Ok(Vec::new());
                }
                if !self.evidence.insert(vote).map_err(invalid)? {
                    return Ok(Vec::new());
                }
                Vec::new()
            }
            Message::Prepared {
                proposal,
                proof,
                manifest,
            } => {
                Self::authenticate_for(
                    &self.anchor,
                    &Message::Prepared {
                        proposal: proposal.clone(),
                        proof: proof.clone(),
                        manifest: manifest.clone(),
                    },
                )?;
                if proposal.body.round == self.round {
                    self.remember(&proposal, &manifest)?;
                }
                self.prepare(&proposal, &proof, &manifest)?
            }
            Message::Decision {
                certificate,
                manifest,
            } => {
                if &certificate.checkpoint != manifest.checkpoint() {
                    return Err(invalid("decision manifest mismatch"));
                }
                let output = self.apply(Command::Decide(certificate), &manifest, None)?;
                return self.emit(output);
            }
            Message::CatchUp { round, evidence } => {
                if round <= self.round {
                    return Ok(Vec::new());
                }
                let candidate = self.initial.clone();
                self.apply(
                    Command::CatchUp {
                        round,
                        evidence: &evidence,
                    },
                    &candidate,
                    None,
                )?;
                self.round = round;
                self.future.discard_through(round);
                self.evidence = RoundEvidence::new(self.anchor.clone(), round);
                self.proposals.clear();
                self.prune_replays()?;
                for vote in &evidence {
                    self.evidence.insert(vote.clone()).map_err(invalid)?;
                }
                vec![Message::CatchUp { round, evidence }]
            }
        };
        output.extend(self.advance()?);
        Ok(output)
    }

    /// A proposal timeout emits a nil prevote. Prevote and precommit timeouts
    /// require participation evidence before advancing the step or round.
    pub fn timeout(&mut self, step: Step) -> Result<Vec<Message>, StoreError> {
        let evidence = match step {
            Step::Propose => Vec::new(),
            Step::Prevote => self.evidence.participation(Phase::Prevote),
            Step::Precommit => self.evidence.participation(Phase::Precommit),
        };
        let candidate = self.initial.clone();
        let output = self.apply(
            Command::Timeout {
                round: self.round,
                step,
                evidence: &evidence,
            },
            &candidate,
            None,
        )?;
        let round = self
            .state()?
            .ok_or_else(|| invalid("missing durable round after timeout"))?
            .round();
        if round != self.round {
            self.round = round;
            self.future.discard_through(round);
            self.evidence = RoundEvidence::new(self.anchor.clone(), round);
            self.proposals.clear();
            self.prune_replays()?;
        }
        self.emit(output)
    }

    fn remember(
        &mut self,
        proposal: &Proposal,
        manifest: &ReplayManifest,
    ) -> Result<(), StoreError> {
        proposal.verify(&self.anchor).map_err(invalid)?;
        if proposal.body.round != self.round || &proposal.body.checkpoint != manifest.checkpoint() {
            return Err(invalid("proposal manifest or round mismatch"));
        }
        if let Some((_, saved_manifest)) =
            self.proposals.iter().find(|(saved, _)| saved == proposal)
        {
            return if saved_manifest == manifest {
                Ok(())
            } else {
                Err(invalid("repeated proposal has a different replay manifest"))
            };
        }
        if self.proposals.len() >= 7 {
            return Err(invalid("too many checkpoint proposals in one round"));
        }
        self.cache(manifest)?;
        self.proposals.push((proposal.clone(), manifest.clone()));
        Ok(())
    }

    fn apply(
        &mut self,
        command: Command<'_>,
        manifest: &ReplayManifest,
        clock: Option<ObservedSigningClock>,
    ) -> Result<Vec<Outgoing>, StoreError> {
        self.cache(manifest)?;
        let retained = self
            .state()?
            .map_or_else(Vec::new, |state| state.retained_values());
        if let Some(candidates) = &self.compact {
            let refs = candidates
                .iter()
                .filter(|candidate| {
                    retained.contains(&candidate.input().checkpoint().hash())
                        || candidate.input().manifest() == *manifest
                })
                .map(CompactReplayCandidate::input)
                .collect::<Vec<_>>();
            let output = checkpoint_agreement::apply_compact_candidates_observed(
                &self.store,
                &self.anchor,
                &self.key,
                command,
                &refs,
                clock,
            )?;
            self.prune_replays()?;
            return Ok(output);
        }
        let refs = self
            .replays
            .iter()
            .filter(|replay| {
                retained.contains(&replay.checkpoint().hash()) || replay.manifest() == *manifest
            })
            .map(ReplayedCandidate::input)
            .collect::<Vec<_>>();
        let output = checkpoint_agreement::apply_candidates_observed(
            &self.store,
            &self.anchor,
            &self.key,
            command,
            &refs,
            clock,
        )?;
        self.prune_replays()?;
        Ok(output)
    }

    fn manifest(
        &self,
        checkpoint: &burst_consensus::checkpoint::Checkpoint,
    ) -> Result<ReplayManifest, StoreError> {
        if let Some(candidates) = &self.compact {
            return candidates
                .iter()
                .find(|candidate| candidate.input().checkpoint() == checkpoint)
                .map(|candidate| candidate.input().manifest())
                .ok_or_else(|| invalid("outgoing checkpoint has no compact replay manifest"));
        }
        self.replays
            .iter()
            .find(|replay| replay.checkpoint() == checkpoint)
            .map(|replay| replay.manifest())
            .ok_or_else(|| invalid("outgoing checkpoint has no durable replay manifest"))
    }

    fn cache(&mut self, manifest: &ReplayManifest) -> Result<(), StoreError> {
        if let Some(candidates) = &self.compact {
            return if candidates
                .iter()
                .any(|candidate| candidate.input().manifest() == *manifest)
            {
                Ok(())
            } else {
                Err(invalid(
                    "checkpoint manifest needs authenticated compact replay",
                ))
            };
        }
        if self
            .replays
            .iter()
            .any(|replay| replay.manifest() == *manifest)
        {
            return Ok(());
        }
        // Six durable choices, seven current-round proposals and the caller's
        // candidate. Confirmed blocks are immutable; restart replays every saved
        // choice again, and certificate admission independently rereads history.
        if self.replays.len() >= 14 {
            return Err(invalid("checkpoint replay cache is full"));
        }
        let replay = manifest.replay(&self.store, &self.params)?;
        replay.input().validate_parent(&self.anchor)?;
        self.replays.push(replay);
        Ok(())
    }

    fn prune_replays(&mut self) -> Result<(), StoreError> {
        let mut retained = self
            .state()?
            .map_or_else(Vec::new, |state| state.retained_values());
        retained.push(self.initial.checkpoint().hash());
        retained.extend(
            self.proposals
                .iter()
                .map(|(_, manifest)| manifest.checkpoint().hash()),
        );
        self.replays
            .retain(|replay| retained.contains(&replay.checkpoint().hash()));
        if let Some(candidates) = &mut self.compact {
            candidates
                .retain(|candidate| retained.contains(&candidate.input().checkpoint().hash()));
        }
        Ok(())
    }

    fn emit(&mut self, output: Vec<Outgoing>) -> Result<Vec<Message>, StoreError> {
        let mut messages = Vec::new();
        for outgoing in output {
            let message = match outgoing {
                Outgoing::Proposal(proposal) => {
                    let manifest = self.manifest(&proposal.body.checkpoint)?;
                    self.remember(&proposal, &manifest)?;
                    Message::Proposal { proposal, manifest }
                }
                Outgoing::Vote(vote) => {
                    self.evidence.insert(vote.clone()).map_err(invalid)?;
                    Message::Vote(vote)
                }
                Outgoing::Decision(certificate) => {
                    let manifest = self.manifest(&certificate.checkpoint)?;
                    if self.compact.is_some() {
                        // The durable agreement journal already holds this
                        // verified decision. Chain persistence and economic
                        // installation are separate authenticated operations.
                        self.anchor
                            .verify_certificate(&certificate)
                            .map_err(invalid)?;
                        messages.push(Message::Decision {
                            certificate,
                            manifest,
                        });
                        continue;
                    }
                    match &manifest {
                        ReplayManifest::Selection(selection) => {
                            CertifiedCheckpoint::accept(
                                &self.store,
                                self.anchor.network(),
                                &self.params,
                                certificate.clone(),
                                selection.heads.clone(),
                                selection.target.clone(),
                            )?;
                        }
                        ReplayManifest::ChallengeOutcome(_) => {
                            let ReplayedCandidate::ChallengeOutcome(outcome) =
                                manifest.replay(&self.store, &self.params)?
                            else {
                                unreachable!()
                            };
                            // The journal retains the decision. Live installation and
                            // height advancement require the payment-epoch handoff.
                            outcome.certify(certificate.clone())?;
                        }
                        ReplayManifest::Roster(_) => {
                            let ReplayedCandidate::Roster(roster) =
                                manifest.replay(&self.store, &self.params)?
                            else {
                                unreachable!()
                            };
                            roster.certify(certificate.clone())?;
                        }
                        ReplayManifest::Activation(_) => {
                            let ReplayedCandidate::Activation(activation) =
                                manifest.replay(&self.store, &self.params)?
                            else {
                                unreachable!()
                            };
                            activation.certify(certificate.clone())?;
                        }
                        ReplayManifest::Deadline(_) => {
                            let ReplayedCandidate::Deadline(deadline) =
                                manifest.replay(&self.store, &self.params)?
                            else {
                                return Err(invalid(
                                    "deadline decision has a different replay kind",
                                ));
                            };
                            deadline.certify(certificate.clone())?;
                        }
                        ReplayManifest::Governance(_) => {
                            let ReplayedCandidate::Governance(governance) =
                                manifest.replay(&self.store, &self.params)?
                            else {
                                return Err(invalid(
                                    "governance decision has a different replay kind",
                                ));
                            };
                            governance.certify(certificate.clone())?;
                        }
                    }
                    Message::Decision {
                        certificate,
                        manifest,
                    }
                }
            };
            messages.push(message);
        }
        Ok(messages)
    }

    fn advance(&mut self) -> Result<Vec<Message>, StoreError> {
        if self
            .state()?
            .is_some_and(|state| state.decision().is_some())
        {
            return Ok(Vec::new());
        }
        let mut messages = Vec::new();
        for (proposal, manifest) in self.proposals.clone() {
            if let Some(certificate) = self
                .evidence
                .certificate(&proposal.body.checkpoint)
                .map_err(invalid)?
            {
                let output = self.apply(Command::Decide(certificate), &manifest, None)?;
                messages.extend(self.emit(output)?);
                break;
            }
            if let Some(proof) = self.evidence.prepared(&proposal).map_err(invalid)? {
                let state = self.state()?;
                let already_prepared = state.as_ref().is_some_and(|state| {
                    state
                        .valid_proof()
                        .is_some_and(|saved| saved.ballot == proof.ballot)
                });
                if !already_prepared
                    || state
                        .as_ref()
                        .is_some_and(|state| state.step() != Step::Precommit)
                {
                    messages.extend(self.prepare(&proposal, &proof, &manifest)?);
                }
                if !already_prepared {
                    messages.push(Message::Prepared {
                        proposal: Box::new(proposal.clone()),
                        proof,
                        manifest: manifest.clone(),
                    });
                }
                if let Some(certificate) = self
                    .evidence
                    .certificate(&proposal.body.checkpoint)
                    .map_err(invalid)?
                {
                    let output = self.apply(Command::Decide(certificate), &manifest, None)?;
                    messages.extend(self.emit(output)?);
                    break;
                }
            }
        }
        Ok(messages)
    }

    fn prepare(
        &mut self,
        proposal: &Proposal,
        proof: &Quorum,
        manifest: &ReplayManifest,
    ) -> Result<Vec<Message>, StoreError> {
        proof.verify(&self.anchor).map_err(invalid)?;
        if proof.ballot.round > self.round
            || proof.ballot.round != proposal.body.round
            || proof.ballot.phase != Phase::Prevote
            || proof.ballot.value != Some(proposal.body.checkpoint.hash())
        {
            return Err(invalid("prepare proof does not match the active proposal"));
        }
        let mut messages = Vec::new();
        if proof.ballot.round == self.round
            && self
                .state()?
                .as_ref()
                .is_none_or(|state| state.step() == Step::Propose)
        {
            // A verified prepare quorum permits progress even when the leader's
            // original proposal was missed. Record nil first; never invent a
            // fresh value vote after its beacon became known.
            let output = self.apply(
                Command::Timeout {
                    round: self.round,
                    step: Step::Propose,
                    evidence: &[],
                },
                manifest,
                None,
            )?;
            messages.extend(self.emit(output)?);
        }
        let output = self.apply(Command::Prepared(proposal, proof), manifest, None)?;
        messages.extend(self.emit(output)?);
        Ok(messages)
    }
}
