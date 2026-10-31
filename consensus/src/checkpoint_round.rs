//! Round recovery for checkpoint agreement, following Tendermint's lock/valid
//! rules (arXiv:1807.04938, Algorithm 1). Human weight comes from the parent.
//! State changes must be durably committed before signing or broadcasting.
//! Ledger replay and pre-beacon admission remain the application's predicates.

use crate::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, CheckpointError, CheckpointSignature,
};
use burst_crypto::{decode_address, verify_signature};
use burst_types::{BlockHash, NetworkId, PublicKey, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoundError {
    #[error(transparent)]
    Certificate(#[from] CheckpointError),
    #[error("checkpoint message has the wrong height, parent, round or phase")]
    WrongContext,
    #[error("checkpoint proposal or justification is invalid")]
    InvalidProposal,
    #[error("checkpoint transition conflicts with durable round state")]
    InvalidTransition,
    #[error("invalid checkpoint round state")]
    InvalidState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Prevote,
    Precommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ballot {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub parent: BlockHash,
    pub height: u64,
    pub round: u64,
    pub phase: Phase,
    pub value: Option<BlockHash>,
}

impl Ballot {
    pub fn precommit(checkpoint: &Checkpoint, round: u64) -> Self {
        Self {
            network: checkpoint.network,
            genesis: checkpoint.genesis,
            parent: checkpoint.parent,
            height: checkpoint.height,
            round,
            phase: Phase::Precommit,
            value: Some(checkpoint.hash()),
        }
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = b"BURST/checkpoint-ballot/v1\0".to_vec();
        bytes.push(match self.network {
            NetworkId::Live => 0,
            NetworkId::Test => 1,
            NetworkId::Dev => 2,
        });
        bytes.extend_from_slice(self.genesis.as_bytes());
        bytes.extend_from_slice(self.parent.as_bytes());
        bytes.extend_from_slice(&self.height.to_be_bytes());
        bytes.extend_from_slice(&self.round.to_be_bytes());
        bytes.push(match self.phase {
            Phase::Prevote => 0,
            Phase::Precommit => 1,
        });
        bytes.push(u8::from(self.value.is_some()));
        bytes.extend_from_slice(self.value.unwrap_or(BlockHash::ZERO).as_bytes());
        bytes
    }

    fn validate(&self, anchor: &CheckpointAnchor) -> Result<(), RoundError> {
        if self.network != anchor.network()
            || self.genesis != anchor.genesis_hash()
            || self.parent != anchor.hash()
            || Some(self.height) != anchor.height().checked_add(1)
            || self.value == Some(BlockHash::ZERO)
        {
            return Err(RoundError::WrongContext);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedBallot {
    pub ballot: Ballot,
    pub signer: CheckpointSignature,
}

impl SignedBallot {
    pub fn verify(&self, anchor: &CheckpointAnchor) -> Result<(), RoundError> {
        self.ballot.validate(anchor)?;
        verify_signer(anchor, &self.ballot.signing_payload(), &self.signer)
    }
}

fn verify_signer(
    anchor: &CheckpointAnchor,
    payload: &[u8],
    signer: &CheckpointSignature,
) -> Result<(), RoundError> {
    let key =
        decode_address(signer.representative.as_str()).ok_or(CheckpointError::InvalidSigner)?;
    if anchor.roster().weight(&signer.representative) == 0
        || !verify_signature(payload, &signer.signature, &PublicKey(key))
    {
        return Err(CheckpointError::InvalidSigner.into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quorum {
    pub ballot: Ballot,
    pub signatures: Vec<CheckpointSignature>,
}

impl Quorum {
    pub fn verify(&self, anchor: &CheckpointAnchor) -> Result<(), RoundError> {
        self.ballot.validate(anchor)?;
        anchor.verify_signatures(&self.ballot.signing_payload(), &self.signatures)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalBody {
    pub checkpoint: Checkpoint,
    pub round: u64,
    pub valid_round: Option<u64>,
}

impl ProposalBody {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = b"BURST/checkpoint-proposal/v1\0".to_vec();
        bytes.extend_from_slice(self.checkpoint.hash().as_bytes());
        bytes.extend_from_slice(&self.round.to_be_bytes());
        bytes.push(u8::from(self.valid_round.is_some()));
        bytes.extend_from_slice(&self.valid_round.unwrap_or(0).to_be_bytes());
        bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub body: ProposalBody,
    pub signer: CheckpointSignature,
    pub justification: Option<Quorum>,
}

impl Proposal {
    pub fn verify(&self, anchor: &CheckpointAnchor) -> Result<(), RoundError> {
        anchor.validate_successor(&self.body.checkpoint)?;
        if &self.signer.representative != anchor.proposer(self.body.round) {
            return Err(RoundError::InvalidProposal);
        }
        verify_signer(anchor, &self.body.signing_payload(), &self.signer)?;
        match (&self.justification, self.body.valid_round) {
            (None, None) => (),
            (Some(proof), Some(round))
                if round < self.body.round
                    && proof.ballot.round == round
                    && proof.ballot.phase == Phase::Prevote
                    && proof.ballot.value == Some(self.body.checkpoint.hash()) =>
            {
                proof.verify(anchor)?
            }
            _ => return Err(RoundError::InvalidProposal),
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Propose,
    Prevote,
    Precommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Prepared {
    checkpoint: Checkpoint,
    proof: Quorum,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundState {
    network: NetworkId,
    genesis: BlockHash,
    parent: BlockHash,
    height: u64,
    representative: WalletAddress,
    round: u64,
    step: Step,
    proposed: Option<ProposalBody>,
    proposed_proof: Option<Quorum>,
    prevote: Option<Ballot>,
    precommit: Option<Ballot>,
    locked: Option<Prepared>,
    valid: Option<Prepared>,
    decision: Option<CheckpointCertificate>,
}

impl RoundState {
    pub fn new(
        anchor: &CheckpointAnchor,
        representative: WalletAddress,
    ) -> Result<Self, RoundError> {
        if anchor.roster().weight(&representative) == 0 {
            return Err(CheckpointError::InvalidSigner.into());
        }
        Ok(Self {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or(RoundError::WrongContext)?,
            representative,
            round: 0,
            step: Step::Propose,
            proposed: None,
            proposed_proof: None,
            prevote: None,
            precommit: None,
            locked: None,
            valid: None,
            decision: None,
        })
    }

    pub fn round(&self) -> u64 {
        self.round
    }
    pub fn step(&self) -> Step {
        self.step
    }
    pub fn prevote(&self) -> Option<&Ballot> {
        self.prevote.as_ref()
    }
    pub fn precommit(&self) -> Option<&Ballot> {
        self.precommit.as_ref()
    }
    pub fn proposal(&self) -> Option<&ProposalBody> {
        self.proposed.as_ref()
    }
    pub fn proposal_proof(&self) -> Option<&Quorum> {
        self.proposed_proof.as_ref()
    }
    pub fn decision(&self) -> Option<&CheckpointCertificate> {
        self.decision.as_ref()
    }
    pub fn valid_proof(&self) -> Option<&Quorum> {
        self.valid.as_ref().map(|p| &p.proof)
    }

    /// Distinct checkpoint hashes whose payloads the caller must retain for
    /// recovery. A prevote can refer to a peer proposal absent from `proposed`.
    pub fn retained_values(&self) -> Vec<BlockHash> {
        let mut values = Vec::new();
        for value in [
            self.proposed.as_ref().map(|p| p.checkpoint.hash()),
            self.prevote.as_ref().and_then(|v| v.value),
            self.precommit.as_ref().and_then(|v| v.value),
            self.locked.as_ref().map(|p| p.checkpoint.hash()),
            self.valid.as_ref().map(|p| p.checkpoint.hash()),
            self.decision.as_ref().map(|c| c.checkpoint.hash()),
        ]
        .into_iter()
        .flatten()
        {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        values
    }

    /// Validate a disk snapshot against its configured signer and trusted anchor.
    /// Deserializing this type is not a validation operation. These checks do not
    /// establish that the snapshot is the latest durable state or replay its ledger.
    pub fn validate(
        &self,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), RoundError> {
        self.ballot(Phase::Prevote, None).validate(anchor)?;
        if &self.representative != representative || anchor.roster().weight(representative) == 0 {
            return Err(RoundError::InvalidState);
        }
        for (ballot, phase) in [
            (&self.prevote, Phase::Prevote),
            (&self.precommit, Phase::Precommit),
        ] {
            if let Some(ballot) = ballot {
                ballot.validate(anchor)?;
                if ballot.round != self.round || ballot.phase != phase {
                    return Err(RoundError::InvalidState);
                }
            }
        }
        if !matches!(
            (self.step, self.prevote.is_some(), self.precommit.is_some()),
            (Step::Propose, false, false)
                | (Step::Prevote, true, false)
                | (Step::Precommit, true, true)
        ) {
            return Err(RoundError::InvalidState);
        }
        for prepared in [&self.locked, &self.valid].into_iter().flatten() {
            anchor.validate_successor(&prepared.checkpoint)?;
            prepared.proof.verify(anchor)?;
            if prepared.proof.ballot.phase != Phase::Prevote
                || prepared.proof.ballot.round > self.round
                || prepared.proof.ballot.value != Some(prepared.checkpoint.hash())
            {
                return Err(RoundError::InvalidState);
            }
        }
        if let Some(locked) = &self.locked {
            if self
                .valid
                .as_ref()
                .is_none_or(|valid| valid.proof.ballot.round < locked.proof.ballot.round)
            {
                return Err(RoundError::InvalidState);
            }
        }
        if let Some(value) = self.precommit.as_ref().and_then(|v| v.value) {
            if self.locked.as_ref().is_none_or(|locked| {
                locked.proof.ballot.round != self.round || locked.checkpoint.hash() != value
            }) {
                return Err(RoundError::InvalidState);
            }
        }
        if let Some(proposed) = &self.proposed {
            anchor.validate_successor(&proposed.checkpoint)?;
            if proposed.round != self.round
                || anchor.proposer(self.round) != representative
                || proposed.valid_round.is_some_and(|r| r >= self.round)
            {
                return Err(RoundError::InvalidState);
            }
            match (&self.proposed_proof, proposed.valid_round) {
                (None, None) => (),
                (Some(proof), Some(round))
                    if proof.ballot.round == round
                        && proof.ballot.phase == Phase::Prevote
                        && proof.ballot.value == Some(proposed.checkpoint.hash()) =>
                {
                    proof.verify(anchor)?
                }
                _ => return Err(RoundError::InvalidState),
            }
        } else if self.proposed_proof.is_some() {
            return Err(RoundError::InvalidState);
        }
        if let Some(decision) = &self.decision {
            anchor.verify_certificate(decision)?;
        }
        Ok(())
    }

    /// Reuse this round's proposal or a prior prepared value before considering
    /// `fresh`. The caller validates the ledger contents and persists the updated
    /// state before signing the returned body.
    pub fn propose(
        &mut self,
        anchor: &CheckpointAnchor,
        fresh: Checkpoint,
    ) -> Result<ProposalBody, RoundError> {
        self.ensure_active()?;
        if anchor.proposer(self.round) != &self.representative || self.step != Step::Propose {
            return Err(RoundError::InvalidTransition);
        }
        if let Some(proposal) = &self.proposed {
            return Ok(proposal.clone());
        }
        let (checkpoint, valid_round) = match &self.valid {
            Some(valid) if valid.proof.ballot.round < self.round => {
                (valid.checkpoint.clone(), Some(valid.proof.ballot.round))
            }
            Some(_) => return Err(RoundError::InvalidTransition),
            None => (fresh, None),
        };
        anchor.validate_successor(&checkpoint)?;
        let proposal = ProposalBody {
            checkpoint,
            round: self.round,
            valid_round,
        };
        self.proposed = Some(proposal.clone());
        self.proposed_proof = self.valid.as_ref().map(|valid| valid.proof.clone());
        Ok(proposal)
    }

    /// Call only after replay and, for new selection inputs, pre-beacon checks.
    pub fn accept_proposal(
        &mut self,
        anchor: &CheckpointAnchor,
        proposal: &Proposal,
    ) -> Result<Ballot, RoundError> {
        self.ensure_active()?;
        proposal.verify(anchor)?;
        if proposal.body.round != self.round || self.step != Step::Propose {
            return Err(RoundError::InvalidTransition);
        }
        let hash = proposal.body.checkpoint.hash();
        let allowed = self.locked.as_ref().is_none_or(|locked| {
            locked.checkpoint.hash() == hash
                || proposal
                    .body
                    .valid_round
                    .is_some_and(|round| round >= locked.proof.ballot.round)
        });
        let ballot = self.ballot(Phase::Prevote, allowed.then_some(hash));
        self.prevote = Some(ballot.clone());
        self.step = Step::Prevote;
        Ok(ballot)
    }

    /// The proposal must already satisfy the application validity predicate.
    /// A late prepare proof updates recovery evidence, but never emits a second
    /// precommit in a round or clears a lock on a nil result.
    pub fn observe_prepared(
        &mut self,
        anchor: &CheckpointAnchor,
        proposal: &Proposal,
        proof: &Quorum,
    ) -> Result<Option<Ballot>, RoundError> {
        self.ensure_active()?;
        proposal.verify(anchor)?;
        proof.verify(anchor)?;
        if proof.ballot.phase != Phase::Prevote
            || proof.ballot.round != proposal.body.round
            || proof.ballot.round > self.round
            || proof.ballot.value != Some(proposal.body.checkpoint.hash())
        {
            return Err(RoundError::WrongContext);
        }
        let prepared = Prepared {
            checkpoint: proposal.body.checkpoint.clone(),
            proof: proof.clone(),
        };
        if self
            .valid
            .as_ref()
            .is_none_or(|valid| valid.proof.ballot.round < proof.ballot.round)
        {
            self.valid = Some(prepared.clone());
        }
        if self.step != Step::Prevote || proof.ballot.round != self.round {
            return Ok(None);
        }
        self.locked = Some(prepared);
        let ballot = self.ballot(Phase::Precommit, proof.ballot.value);
        self.precommit = Some(ballot.clone());
        self.step = Step::Precommit;
        Ok(Some(ballot))
    }

    /// Record a verified precommit certificate, including one from another round.
    /// This does not execute or install the checkpoint's ledger state. A certificate
    /// for a different checkpoint cannot replace an existing decision.
    pub fn decide(
        &mut self,
        anchor: &CheckpointAnchor,
        certificate: CheckpointCertificate,
    ) -> Result<(), RoundError> {
        anchor.verify_certificate(&certificate)?;
        if self
            .decision
            .as_ref()
            .is_some_and(|old| old.checkpoint != certificate.checkpoint)
        {
            return Err(RoundError::InvalidTransition);
        }
        self.decision = Some(certificate);
        Ok(())
    }

    /// Prevote/precommit timeouts require human quorum evidence that peers
    /// reached that phase. The caller decides when the timer has elapsed; this
    /// method reads no clock. Proposal timeout needs no participation evidence.
    pub fn timeout(
        &mut self,
        anchor: &CheckpointAnchor,
        round: u64,
        step: Step,
        evidence: &[SignedBallot],
    ) -> Result<Option<Ballot>, RoundError> {
        self.ensure_active()?;
        if round != self.round || step != self.step {
            return Ok(None);
        }
        match step {
            Step::Propose => {
                let ballot = self.ballot(Phase::Prevote, None);
                self.prevote = Some(ballot.clone());
                self.step = Step::Prevote;
                Ok(Some(ballot))
            }
            Step::Prevote => {
                verify_participation(
                    anchor,
                    round,
                    Some(Phase::Prevote),
                    evidence,
                    anchor.required_humans(),
                )?;
                let ballot = self.ballot(Phase::Precommit, None);
                self.precommit = Some(ballot.clone());
                self.step = Step::Precommit;
                Ok(Some(ballot))
            }
            Step::Precommit => {
                verify_participation(
                    anchor,
                    round,
                    Some(Phase::Precommit),
                    evidence,
                    anchor.required_humans(),
                )?;
                self.start_round(round.checked_add(1).ok_or(RoundError::InvalidTransition)?);
                Ok(None)
            }
        }
    }

    /// Advance to a later round supported by more than the tolerated fault weight.
    /// Mixed phases and values count for participation. Locks and valid preparation
    /// evidence survive the round change; this does not decide a checkpoint.
    pub fn catch_up_round(
        &mut self,
        anchor: &CheckpointAnchor,
        round: u64,
        evidence: &[SignedBallot],
    ) -> Result<(), RoundError> {
        self.ensure_active()?;
        if round <= self.round {
            return Err(RoundError::InvalidTransition);
        }
        // More than f human units, not f representative keys.
        verify_participation(
            anchor,
            round,
            None,
            evidence,
            (anchor.roster().total_humans() - 1) / 3 + 1,
        )?;
        self.start_round(round);
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), RoundError> {
        if self.decision.is_some() {
            return Err(RoundError::InvalidTransition);
        }
        Ok(())
    }
    fn start_round(&mut self, round: u64) {
        self.round = round;
        self.step = Step::Propose;
        self.proposed = None;
        self.proposed_proof = None;
        self.prevote = None;
        self.precommit = None;
    }
    fn ballot(&self, phase: Phase, value: Option<BlockHash>) -> Ballot {
        Ballot {
            network: self.network,
            genesis: self.genesis,
            parent: self.parent,
            height: self.height,
            round: self.round,
            phase,
            value,
        }
    }
}

fn verify_participation(
    anchor: &CheckpointAnchor,
    round: u64,
    phase: Option<Phase>,
    votes: &[SignedBallot],
    required: u64,
) -> Result<(), RoundError> {
    if votes.len() > anchor.roster().total_humans() as usize {
        return Err(CheckpointError::InvalidSigner.into());
    }
    let mut seen = HashSet::new();
    let mut weight = 0;
    for vote in votes {
        vote.verify(anchor)?;
        if vote.ballot.round != round || phase.is_some_and(|p| vote.ballot.phase != p) {
            return Err(RoundError::WrongContext);
        }
        if !seen.insert(&vote.signer.representative) {
            return Err(CheckpointError::InvalidSigner.into());
        }
        weight += anchor.roster().weight(&vote.signer.representative);
    }
    if weight < required {
        return Err(CheckpointError::InsufficientQuorum.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{HumanDelegation, HumanRoster};
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};

    fn address(seed: u8) -> WalletAddress {
        derive_address(&keypair_from_seed(&[seed; 32]).public)
    }
    fn anchor() -> CheckpointAnchor {
        CheckpointAnchor::genesis(
            NetworkId::Dev,
            BlockHash::new([9; 32]),
            100,
            HumanRoster::new(
                (1..=4)
                    .map(|n| HumanDelegation {
                        human: address(n),
                        representative: address(n),
                    })
                    .collect(),
            )
            .unwrap(),
            6700,
        )
        .unwrap()
    }
    fn checkpoint(anchor: &CheckpointAnchor, value: u8) -> Checkpoint {
        Checkpoint {
            network: NetworkId::Dev,
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: 1,
            cutoff: 101,
            ledger_root: BlockHash::new([value; 32]),
            params_hash: BlockHash::new([2; 32]),
            next_roster: anchor.roster().commitment(),
            selection_root: BlockHash::new([3; 32]),
            next_quorum_bps: 6700,
        }
    }
    fn sign(seed: u8, bytes: &[u8]) -> CheckpointSignature {
        CheckpointSignature {
            representative: address(seed),
            signature: sign_message(bytes, &keypair_from_seed(&[seed; 32]).private),
        }
    }
    fn votes(ballot: &Ballot, seeds: &[u8]) -> Vec<SignedBallot> {
        seeds
            .iter()
            .map(|&seed| SignedBallot {
                ballot: ballot.clone(),
                signer: sign(seed, &ballot.signing_payload()),
            })
            .collect()
    }
    fn quorum(ballot: Ballot, seeds: &[u8]) -> Quorum {
        Quorum {
            signatures: votes(&ballot, seeds)
                .into_iter()
                .map(|v| v.signer)
                .collect(),
            ballot,
        }
    }
    fn proposal(
        anchor: &CheckpointAnchor,
        cp: Checkpoint,
        round: u64,
        proof: Option<Quorum>,
    ) -> Proposal {
        let body = ProposalBody {
            checkpoint: cp,
            round,
            valid_round: proof.as_ref().map(|p| p.ballot.round),
        };
        let seed = (1..=4)
            .find(|&n| address(n) == *anchor.proposer(round))
            .unwrap();
        Proposal {
            signer: sign(seed, &body.signing_payload()),
            body,
            justification: proof,
        }
    }
    fn advance(state: &mut RoundState, anchor: &CheckpointAnchor) {
        let round = state.round();
        if state.step() == Step::Propose {
            state.timeout(anchor, round, Step::Propose, &[]).unwrap();
        }
        if state.step() == Step::Prevote {
            let evidence = votes(state.prevote().unwrap(), &[1, 2, 3]);
            state
                .timeout(anchor, round, Step::Prevote, &evidence)
                .unwrap();
        }
        let evidence = votes(state.precommit().unwrap(), &[1, 2, 3]);
        state
            .timeout(anchor, round, Step::Precommit, &evidence)
            .unwrap();
    }

    #[test]
    fn three_independent_humans_prepare_and_commit_but_two_cannot() {
        let anchor = anchor();
        let cp = checkpoint(&anchor, 10);
        let proposal = proposal(&anchor, cp.clone(), 0, None);
        let mut nodes: Vec<_> = (1..=4)
            .map(|n| RoundState::new(&anchor, address(n)).unwrap())
            .collect();
        let prevotes: Vec<_> = nodes
            .iter_mut()
            .map(|n| n.accept_proposal(&anchor, &proposal).unwrap())
            .collect();
        let prepare = quorum(prevotes[0].clone(), &[1, 2, 3]);
        assert!(quorum(prevotes[0].clone(), &[1, 2])
            .verify(&anchor)
            .is_err());
        for node in &mut nodes {
            assert_eq!(
                node.observe_prepared(&anchor, &proposal, &prepare)
                    .unwrap()
                    .unwrap()
                    .value,
                Some(cp.hash())
            );
            assert!(node
                .observe_prepared(&anchor, &proposal, &prepare)
                .unwrap()
                .is_none());
        }
        let signatures = votes(nodes[0].precommit().unwrap(), &[1, 2, 3])
            .into_iter()
            .map(|v| v.signer)
            .collect();
        let cert = CheckpointCertificate {
            checkpoint: cp,
            round: 0,
            signatures,
        };
        for (i, node) in nodes.iter_mut().enumerate() {
            node.decide(&anchor, cert.clone()).unwrap();
            node.validate(&anchor, &address(i as u8 + 1)).unwrap();
            assert!(node.timeout(&anchor, 0, Step::Precommit, &[]).is_err());
        }
        let mut changed_round = cert;
        changed_round.round = 1;
        assert!(anchor.verify_certificate(&changed_round).is_err());
    }

    #[test]
    fn prevotes_and_mixed_rounds_cannot_be_reused_as_a_decision() {
        let anchor = anchor();
        let cp = checkpoint(&anchor, 10);
        let commit = Ballot::precommit(&cp, 0);
        let mut prepare = commit.clone();
        prepare.phase = Phase::Prevote;
        let mut cert = CheckpointCertificate {
            checkpoint: cp,
            round: 0,
            signatures: quorum(prepare, &[1, 2, 3]).signatures,
        };
        assert!(anchor.verify_certificate(&cert).is_err());
        cert.signatures = quorum(commit.clone(), &[1, 2, 3]).signatures;
        let mut later = commit;
        later.round = 1;
        cert.signatures[2] = sign(3, &later.signing_payload());
        assert!(anchor.verify_certificate(&cert).is_err());
    }

    #[test]
    fn split_proposals_recover_without_a_permanent_first_choice() {
        let anchor = anchor();
        let a = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
        let b = proposal(&anchor, checkpoint(&anchor, 11), 0, None);
        let mut nodes: Vec<_> = (1..=4)
            .map(|n| RoundState::new(&anchor, address(n)).unwrap())
            .collect();
        let mut split = Vec::new();
        for (i, node) in nodes.iter_mut().enumerate() {
            let ballot = node
                .accept_proposal(&anchor, if i < 2 { &a } else { &b })
                .unwrap();
            split.push(SignedBallot {
                signer: sign(i as u8 + 1, &ballot.signing_payload()),
                ballot,
            });
        }
        let mut nil_commits = Vec::new();
        for (i, node) in nodes.iter_mut().enumerate() {
            let ballot = node
                .timeout(&anchor, 0, Step::Prevote, &split)
                .unwrap()
                .unwrap();
            assert_eq!(ballot.value, None);
            nil_commits.push(SignedBallot {
                signer: sign(i as u8 + 1, &ballot.signing_payload()),
                ballot,
            });
        }
        for node in &mut nodes {
            node.timeout(&anchor, 0, Step::Precommit, &nil_commits)
                .unwrap();
        }
        let retry = proposal(&anchor, b.body.checkpoint.clone(), 1, None);
        let mut prepared = Vec::new();
        for node in &mut nodes {
            prepared.push(node.accept_proposal(&anchor, &retry).unwrap());
        }
        let proof = quorum(prepared[0].clone(), &[1, 2, 3]);
        for node in &mut nodes {
            let commit = node
                .observe_prepared(&anchor, &retry, &proof)
                .unwrap()
                .unwrap();
            assert_eq!(commit.value, Some(b.body.checkpoint.hash()));
        }
    }

    #[test]
    fn timeout_and_restart_do_not_unlock_a_prepared_value() {
        let anchor = anchor();
        let a = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
        let mut node = RoundState::new(&anchor, address(1)).unwrap();
        let prevote = node.accept_proposal(&anchor, &a).unwrap();
        let proof = quorum(prevote, &[1, 2, 3]);
        node.observe_prepared(&anchor, &a, &proof).unwrap();
        advance(&mut node, &anchor);
        let bytes = bincode::serialize(&node).unwrap();
        let mut restarted: RoundState = bincode::deserialize(&bytes).unwrap();
        restarted.validate(&anchor, &address(1)).unwrap();
        let b = proposal(&anchor, checkpoint(&anchor, 11), 1, None);
        assert_eq!(restarted.accept_proposal(&anchor, &b).unwrap().value, None);
        advance(&mut restarted, &anchor);
        let reproposal = proposal(&anchor, a.body.checkpoint.clone(), 2, Some(proof));
        assert_eq!(
            restarted
                .accept_proposal(&anchor, &reproposal)
                .unwrap()
                .value,
            Some(a.body.checkpoint.hash())
        );
    }

    #[test]
    fn a_newer_prepare_proof_allows_safe_recovery_from_a_minority_lock() {
        let anchor = anchor();
        let a = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
        let mut node = RoundState::new(&anchor, address(1)).unwrap();
        let ballot = node.accept_proposal(&anchor, &a).unwrap();
        node.observe_prepared(&anchor, &a, &quorum(ballot, &[1, 2, 3]))
            .unwrap();
        advance(&mut node, &anchor);
        // Only this node saw A's prepare proof. The other three can prepare B
        // in round 1; there was no quorum of commits for A.
        let b = checkpoint(&anchor, 11);
        let mut ballot = Ballot::precommit(&b, 1);
        ballot.phase = Phase::Prevote;
        let proof = quorum(ballot, &[2, 3, 4]);
        advance(&mut node, &anchor);
        let justified = proposal(&anchor, b.clone(), 2, Some(proof));
        assert_eq!(
            node.accept_proposal(&anchor, &justified).unwrap().value,
            Some(b.hash())
        );
    }

    #[test]
    fn late_preparation_updates_reproposal_but_never_double_precommits() {
        let anchor = anchor();
        let p = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
        let mut node = RoundState::new(&anchor, address(1)).unwrap();
        let ballot = node.accept_proposal(&anchor, &p).unwrap();
        let evidence = votes(&ballot, &[1, 2, 3]);
        node.timeout(&anchor, 0, Step::Prevote, &evidence).unwrap();
        let proof = quorum(ballot, &[1, 2, 3]);
        assert!(node
            .observe_prepared(&anchor, &p, &proof)
            .unwrap()
            .is_none());
        assert_eq!(node.precommit().unwrap().value, None);
        assert_eq!(node.valid_proof(), Some(&proof));
        node.validate(&anchor, &address(1)).unwrap();
    }

    #[test]
    fn forged_context_proofs_and_single_byzantine_round_jumps_fail() {
        let anchor = anchor();
        let cp = checkpoint(&anchor, 10);
        let mut node = RoundState::new(&anchor, address(1)).unwrap();
        let ballot = Ballot::precommit(&cp, 99);
        assert!(node
            .catch_up_round(&anchor, 99, &votes(&ballot, &[4]))
            .is_err());
        assert_eq!(node.round(), 0);
        assert!(node
            .catch_up_round(&anchor, 99, &votes(&ballot, &[4, 4]))
            .is_err());
        let mut wrong_parent = ballot.clone();
        wrong_parent.parent = BlockHash::new([8; 32]);
        assert!(node
            .catch_up_round(&anchor, 99, &votes(&wrong_parent, &[1, 2]))
            .is_err());
        node.catch_up_round(&anchor, 99, &votes(&ballot, &[1, 2]))
            .unwrap();
        assert_eq!(node.round(), 99);
        let mut wrong_proposer = proposal(&anchor, cp.clone(), 99, None);
        wrong_proposer.signer = sign(99, &wrong_proposer.body.signing_payload());
        assert!(node.accept_proposal(&anchor, &wrong_proposer).is_err());
        let mut prevote = Ballot::precommit(&cp, 98);
        prevote.phase = Phase::Prevote;
        let insufficient = proposal(&anchor, cp, 99, Some(quorum(prevote, &[1, 2])));
        assert!(node.accept_proposal(&anchor, &insufficient).is_err());
        assert_eq!(node.step(), Step::Propose);
    }

    #[test]
    fn restored_state_cannot_discard_lock_evidence_or_change_signer() {
        let anchor = anchor();
        let p = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
        let mut node = RoundState::new(&anchor, address(1)).unwrap();
        let ballot = node.accept_proposal(&anchor, &p).unwrap();
        node.observe_prepared(&anchor, &p, &quorum(ballot, &[1, 2, 3]))
            .unwrap();
        assert!(node.validate(&anchor, &address(2)).is_err());
        let mut corrupt = node.clone();
        corrupt.locked = None;
        assert!(corrupt.validate(&anchor, &address(1)).is_err());
        let mut corrupt = node.clone();
        corrupt.valid = None;
        assert!(corrupt.validate(&anchor, &address(1)).is_err());
        let mut corrupt = node;
        corrupt.locked.as_mut().unwrap().proof.signatures.pop();
        assert!(corrupt.validate(&anchor, &address(1)).is_err());
    }

    #[test]
    fn delayed_prepare_delivery_cannot_create_two_decisions_with_one_equivocator() {
        // Representative four signs both sides. Every honest node signs only
        // what its state machine permits. Enumerate which honest nodes see A's
        // prepare QC before the first timeout, including a minority-only lock.
        for delivery_mask in 0..8u8 {
            let anchor = anchor();
            let a = proposal(&anchor, checkpoint(&anchor, 10), 0, None);
            let mut honest: Vec<_> = (1..=3)
                .map(|n| RoundState::new(&anchor, address(n)).unwrap())
                .collect();
            let mut prepare_votes = Vec::new();
            for (i, node) in honest.iter_mut().enumerate() {
                let ballot = node.accept_proposal(&anchor, &a).unwrap();
                prepare_votes.push(SignedBallot {
                    signer: sign(i as u8 + 1, &ballot.signing_payload()),
                    ballot,
                });
            }
            let prepare_a = Quorum {
                ballot: prepare_votes[0].ballot.clone(),
                signatures: prepare_votes.iter().map(|v| v.signer.clone()).collect(),
            };
            let mut first_commits = Vec::new();
            for (i, node) in honest.iter_mut().enumerate() {
                let ballot = if delivery_mask & (1 << i) != 0 {
                    node.observe_prepared(&anchor, &a, &prepare_a)
                        .unwrap()
                        .unwrap()
                } else {
                    node.timeout(&anchor, 0, Step::Prevote, &prepare_votes)
                        .unwrap()
                        .unwrap()
                };
                first_commits.push(SignedBallot {
                    signer: sign(i as u8 + 1, &ballot.signing_payload()),
                    ballot,
                });
            }
            let mut a_signatures: Vec<_> = first_commits
                .iter()
                .filter(|v| v.ballot.value.is_some())
                .map(|v| v.signer.clone())
                .collect();
            a_signatures.push(sign(
                4,
                &Ballot::precommit(&a.body.checkpoint, 0).signing_payload(),
            ));
            let cert_a = CheckpointCertificate {
                checkpoint: a.body.checkpoint.clone(),
                round: 0,
                signatures: a_signatures,
            };
            let a_decided = anchor.verify_certificate(&cert_a).is_ok();
            for node in &mut honest {
                node.timeout(&anchor, 0, Step::Precommit, &first_commits)
                    .unwrap();
            }

            let b = proposal(&anchor, checkpoint(&anchor, 11), 1, None);
            let mut b_votes = Vec::new();
            for (i, node) in honest.iter_mut().enumerate() {
                let restored: RoundState =
                    bincode::deserialize(&bincode::serialize(node).unwrap()).unwrap();
                *node = restored;
                node.validate(&anchor, &address(i as u8 + 1)).unwrap();
                let ballot = node.accept_proposal(&anchor, &b).unwrap();
                if ballot.value.is_some() {
                    b_votes.push(sign(i as u8 + 1, &ballot.signing_payload()));
                }
            }
            let mut b_ballot = Ballot::precommit(&b.body.checkpoint, 1);
            b_ballot.phase = Phase::Prevote;
            b_votes.push(sign(4, &b_ballot.signing_payload()));
            let prepare_b = Quorum {
                ballot: b_ballot,
                signatures: b_votes,
            };
            if prepare_b.verify(&anchor).is_ok() {
                let mut b_commits = Vec::new();
                for (i, node) in honest.iter_mut().enumerate() {
                    let ballot = node
                        .observe_prepared(&anchor, &b, &prepare_b)
                        .unwrap()
                        .unwrap();
                    b_commits.push(sign(i as u8 + 1, &ballot.signing_payload()));
                }
                let cert_b = CheckpointCertificate {
                    checkpoint: b.body.checkpoint.clone(),
                    round: 1,
                    signatures: b_commits,
                };
                assert!(anchor.verify_certificate(&cert_b).is_ok());
                assert!(
                    !a_decided,
                    "two decisions under delivery mask {delivery_mask}"
                );
            } else {
                assert!(
                    a_decided,
                    "a minority lock must not stall the unlocked honest quorum"
                );
            }
        }
    }
}
