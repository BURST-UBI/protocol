//! Bounded evidence for one checkpoint height and round. Only signatures from
//! the trusted parent electorate are retained; peer attendance changes no weight.

use std::collections::BTreeMap;

use burst_types::BlockHash;

use crate::checkpoint::{Checkpoint, CheckpointAnchor, CheckpointCertificate};
use crate::checkpoint_round::{Phase, Proposal, Quorum, RoundError, SignedBallot};

pub struct RoundEvidence {
    anchor: CheckpointAnchor,
    round: u64,
    prevotes: BTreeMap<String, SignedBallot>,
    precommits: BTreeMap<String, SignedBallot>,
}

/// One highest-round observation per authorized representative. A single signer
/// cannot allocate a queue for every round or evict another signer's evidence.
pub struct FutureEvidence {
    anchor: CheckpointAnchor,
    votes: BTreeMap<String, SignedBallot>,
}

impl FutureEvidence {
    pub fn new(anchor: CheckpointAnchor) -> Self {
        Self {
            anchor,
            votes: BTreeMap::new(),
        }
    }

    pub fn discard_through(&mut self, round: u64) {
        self.votes.retain(|_, vote| vote.ballot.round > round);
    }

    /// Mixed phases and values can establish round participation, never a value
    /// quorum. Every retained signer must authenticate this exact parent/height.
    /// Returns evidence when the incoming round has floor((humans - 1) / 3) + 1
    /// delegated human units. Retaining only each signer's highest round can drop
    /// its contribution to an older round.
    pub fn insert(
        &mut self,
        vote: SignedBallot,
        current_round: u64,
    ) -> Result<Option<(u64, Vec<SignedBallot>)>, RoundError> {
        vote.verify(&self.anchor)?;
        self.discard_through(current_round);
        let round = vote.ballot.round;
        if round <= current_round {
            return Ok(None);
        }
        let key = vote.signer.representative.to_string();
        if self
            .votes
            .get(&key)
            .is_some_and(|old| old.ballot.round >= round)
        {
            return Ok(None);
        }
        self.votes.insert(key, vote);
        let matching: Vec<_> = self
            .votes
            .values()
            .filter(|vote| vote.ballot.round == round)
            .cloned()
            .collect();
        let weight: u64 = matching
            .iter()
            .map(|vote| self.anchor.roster().weight(&vote.signer.representative))
            .sum();
        let required = (self.anchor.roster().total_humans() - 1) / 3 + 1;
        Ok((weight >= required).then_some((round, matching)))
    }
}

impl RoundEvidence {
    pub fn new(anchor: CheckpointAnchor, round: u64) -> Self {
        Self {
            anchor,
            round,
            prevotes: BTreeMap::new(),
            precommits: BTreeMap::new(),
        }
    }

    /// At most one ballot per representative and phase. A conflicting second
    /// signature is an error, not another vote or a replacement of the first.
    pub fn insert(&mut self, vote: SignedBallot) -> Result<bool, RoundError> {
        vote.verify(&self.anchor)?;
        if vote.ballot.round != self.round {
            return Err(RoundError::WrongContext);
        }
        let votes = match vote.ballot.phase {
            Phase::Prevote => &mut self.prevotes,
            Phase::Precommit => &mut self.precommits,
        };
        match votes.entry(vote.signer.representative.to_string()) {
            std::collections::btree_map::Entry::Occupied(existing) => {
                if existing.get().ballot != vote.ballot {
                    return Err(RoundError::InvalidTransition);
                }
                Ok(false)
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(vote);
                Ok(true)
            }
        }
    }

    pub fn quorum(&self, phase: Phase, value: Option<BlockHash>) -> Option<Quorum> {
        let votes = self.votes(phase);
        let mut weight = 0;
        let mut matching = votes.values().filter(|vote| vote.ballot.value == value);
        let first = matching.next()?;
        let mut signatures = Vec::new();
        for vote in std::iter::once(first).chain(matching) {
            weight += self.anchor.roster().weight(&vote.signer.representative);
            signatures.push(vote.signer.clone());
        }
        (weight >= self.anchor.required_humans()).then(|| Quorum {
            ballot: first.ballot.clone(),
            signatures,
        })
    }

    pub fn prepared(&self, proposal: &Proposal) -> Result<Option<Quorum>, RoundError> {
        proposal.verify(&self.anchor)?;
        if proposal.body.round != self.round {
            return Err(RoundError::WrongContext);
        }
        Ok(self.quorum(Phase::Prevote, Some(proposal.body.checkpoint.hash())))
    }

    pub fn certificate(
        &self,
        checkpoint: &Checkpoint,
    ) -> Result<Option<CheckpointCertificate>, RoundError> {
        self.anchor.validate_successor(checkpoint)?;
        Ok(self
            .quorum(Phase::Precommit, Some(checkpoint.hash()))
            .map(|quorum| CheckpointCertificate {
                checkpoint: checkpoint.clone(),
                round: self.round,
                signatures: quorum.signatures,
            }))
    }

    /// Different choices count towards phase participation, never towards the
    /// same value's quorum. RoundState checks this evidence for prevote and
    /// precommit timeouts; a proposal timeout requires no participation proof.
    pub fn participation(&self, phase: Phase) -> Vec<SignedBallot> {
        self.votes(phase).values().cloned().collect()
    }

    /// Catch-up counts a representative once even if both phases were received.
    pub fn round_participation(&self) -> Vec<SignedBallot> {
        let mut votes = self.prevotes.clone();
        votes.extend(self.precommits.clone());
        votes.into_values().collect()
    }

    fn votes(&self, phase: Phase) -> &BTreeMap<String, SignedBallot> {
        match phase {
            Phase::Prevote => &self.prevotes,
            Phase::Precommit => &self.precommits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{CheckpointSignature, HumanDelegation, HumanRoster};
    use crate::checkpoint_round::{Ballot, ProposalBody, RoundState, Step};
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};
    use burst_types::{NetworkId, Signature, WalletAddress};

    fn address(seed: u8) -> WalletAddress {
        derive_address(&keypair_from_seed(&[seed; 32]).public)
    }

    fn anchor() -> CheckpointAnchor {
        let roster = HumanRoster::new(
            (1..=4)
                .map(|seed| HumanDelegation {
                    human: address(seed),
                    representative: address(if seed == 4 { 1 } else { seed }),
                })
                .collect(),
        )
        .unwrap();
        CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([1; 32]), 100, roster, 6700)
            .unwrap()
    }

    fn vote(seed: u8, phase: Phase, value: Option<BlockHash>) -> SignedBallot {
        let anchor = anchor();
        let ballot = Ballot {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: 1,
            round: 0,
            phase,
            value,
        };
        let signer = CheckpointSignature {
            representative: address(seed),
            signature: sign_message(
                &ballot.signing_payload(),
                &keypair_from_seed(&[seed; 32]).private,
            ),
        };
        SignedBallot { ballot, signer }
    }

    fn future_vote(seed: u8, round: u64, phase: Phase) -> SignedBallot {
        let mut vote = vote(seed, phase, None);
        vote.ballot.round = round;
        vote.signer.signature = sign_message(
            &vote.ballot.signing_payload(),
            &keypair_from_seed(&[seed; 32]).private,
        );
        vote
    }

    #[test]
    fn future_rounds_need_more_than_fault_weight_not_duplicate_keys_or_mixed_rounds() {
        let mut evidence = FutureEvidence::new(anchor());
        assert!(evidence
            .insert(future_vote(2, 7, Phase::Prevote), 0)
            .unwrap()
            .is_none());
        assert!(evidence
            .insert(future_vote(2, 7, Phase::Precommit), 0)
            .unwrap()
            .is_none());
        assert!(evidence
            .insert(future_vote(3, 6, Phase::Prevote), 0)
            .unwrap()
            .is_none());
        let (round, proof) = evidence
            .insert(future_vote(3, 7, Phase::Precommit), 0)
            .unwrap()
            .unwrap();
        assert_eq!(round, 7);
        assert_eq!(proof.len(), 2);
        let mut state = RoundState::new(&anchor(), address(1)).unwrap();
        state.catch_up_round(&anchor(), round, &proof).unwrap();
        assert_eq!(state.round(), 7);
        evidence.discard_through(7);
        assert!(evidence.votes.is_empty());
        assert!(evidence
            .insert(future_vote(2, 7, Phase::Prevote), 7)
            .unwrap()
            .is_none());
        assert!(evidence.votes.is_empty());
        // This representative carries two of four humans, enough for > f.
        let (_, proof) = evidence
            .insert(future_vote(1, 8, Phase::Prevote), 7)
            .unwrap()
            .unwrap();
        assert_eq!(proof.len(), 1);
        state.catch_up_round(&anchor(), 8, &proof).unwrap();
    }

    #[test]
    fn future_round_spam_is_bounded_by_authority_and_invalid_votes_cannot_evict() {
        let mut evidence = FutureEvidence::new(anchor());
        for round in 1..=100 {
            assert!(evidence
                .insert(future_vote(2, round, Phase::Prevote), 0)
                .unwrap()
                .is_none());
        }
        assert_eq!(evidence.votes.len(), 1);
        let before = evidence.votes.clone();
        let mut forged = future_vote(2, 101, Phase::Prevote);
        forged.signer.signature = Signature([0; 64]);
        assert!(evidence.insert(forged, 0).is_err());
        let mut wrong_parent = future_vote(2, 101, Phase::Prevote);
        wrong_parent.ballot.parent = BlockHash::new([99; 32]);
        wrong_parent.signer.signature = sign_message(
            &wrong_parent.ballot.signing_payload(),
            &keypair_from_seed(&[2; 32]).private,
        );
        assert!(evidence.insert(wrong_parent, 0).is_err());
        for seed in [4, 99] {
            assert!(evidence
                .insert(future_vote(seed, 101, Phase::Prevote), 0)
                .is_err());
        }
        assert_eq!(evidence.votes, before);
        assert!(evidence
            .insert(future_vote(2, 99, Phase::Prevote), 0)
            .unwrap()
            .is_none());
        assert_eq!(evidence.votes, before);
        let (_, proof) = evidence
            .insert(future_vote(3, 100, Phase::Precommit), 0)
            .unwrap()
            .unwrap();
        assert_eq!(proof.len(), 2);
    }

    #[test]
    fn quorum_counts_delegated_humans_and_never_online_attendance() {
        let mut evidence = RoundEvidence::new(anchor(), 0);
        let value = Some(BlockHash::new([8; 32]));
        let first = vote(1, Phase::Prevote, value);
        assert!(evidence.insert(first.clone()).unwrap());
        for _ in 0..100 {
            assert!(!evidence.insert(first.clone()).unwrap());
        }
        assert!(evidence.quorum(Phase::Prevote, value).is_none());
        assert_eq!(evidence.participation(Phase::Prevote).len(), 1);
        evidence.insert(vote(2, Phase::Prevote, value)).unwrap();
        let quorum = evidence.quorum(Phase::Prevote, value).unwrap();
        assert_eq!(quorum.signatures.len(), 2);
        quorum.verify(&anchor()).unwrap();
        assert!(evidence.quorum(Phase::Precommit, value).is_none());
    }

    #[test]
    fn preparation_and_decision_require_separate_quorums_for_the_exact_value() {
        let anchor = anchor();
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: 1,
            cutoff: 101,
            ledger_root: BlockHash::new([3; 32]),
            params_hash: BlockHash::new([4; 32]),
            next_roster: anchor.roster().commitment(),
            selection_root: BlockHash::new([5; 32]),
            next_quorum_bps: 6700,
        };
        let proposer = (1..=3)
            .find(|seed| &address(*seed) == anchor.proposer(0))
            .unwrap();
        let body = ProposalBody {
            checkpoint: checkpoint.clone(),
            round: 0,
            valid_round: None,
        };
        let proposal = Proposal {
            signer: CheckpointSignature {
                representative: address(proposer),
                signature: sign_message(
                    &body.signing_payload(),
                    &keypair_from_seed(&[proposer; 32]).private,
                ),
            },
            body,
            justification: None,
        };
        let mut evidence = RoundEvidence::new(anchor.clone(), 0);
        assert!(evidence.prepared(&proposal).unwrap().is_none());
        for seed in [1, 2] {
            evidence
                .insert(vote(seed, Phase::Prevote, Some(checkpoint.hash())))
                .unwrap();
        }
        evidence
            .prepared(&proposal)
            .unwrap()
            .unwrap()
            .verify(&anchor)
            .unwrap();
        assert!(evidence.certificate(&checkpoint).unwrap().is_none());
        for seed in [1, 2] {
            evidence
                .insert(vote(seed, Phase::Precommit, Some(checkpoint.hash())))
                .unwrap();
        }
        anchor
            .verify_certificate(&evidence.certificate(&checkpoint).unwrap().unwrap())
            .unwrap();
        let mut other = checkpoint.clone();
        other.ledger_root = BlockHash::new([9; 32]);
        assert!(evidence.certificate(&other).unwrap().is_none());
        other.height = 2;
        assert!(evidence.certificate(&other).is_err());
        let mut forged = proposal;
        forged.signer.signature = Signature([0; 64]);
        assert!(evidence.prepared(&forged).is_err());
    }

    #[test]
    fn conflicting_or_unauthorized_ballots_do_not_mutate_evidence() {
        let mut evidence = RoundEvidence::new(anchor(), 0);
        evidence.insert(vote(1, Phase::Prevote, None)).unwrap();
        let before = evidence.participation(Phase::Prevote);
        assert!(evidence
            .insert(vote(1, Phase::Prevote, Some(BlockHash::new([8; 32]))))
            .is_err());
        for seed in [4, 99] {
            assert!(evidence.insert(vote(seed, Phase::Prevote, None)).is_err());
        }
        for field in 0..6 {
            let mut wrong = vote(2, Phase::Prevote, None);
            match field {
                0 => wrong.ballot.network = NetworkId::Live,
                1 => wrong.ballot.genesis = BlockHash::new([4; 32]),
                2 => wrong.ballot.parent = BlockHash::new([5; 32]),
                3 => wrong.ballot.height += 1,
                4 => wrong.ballot.round += 1,
                _ => wrong.ballot.value = Some(BlockHash::ZERO),
            }
            wrong.signer.signature = sign_message(
                &wrong.ballot.signing_payload(),
                &keypair_from_seed(&[2; 32]).private,
            );
            assert!(evidence.insert(wrong).is_err());
        }
        let mut forged = vote(2, Phase::Prevote, None);
        forged.signer.signature = Signature([0; 64]);
        assert!(evidence.insert(forged).is_err());
        assert_eq!(evidence.participation(Phase::Prevote), before);
    }

    #[test]
    fn split_votes_supply_timeout_evidence_but_no_value_quorum() {
        let mut evidence = RoundEvidence::new(anchor(), 0);
        let value = Some(BlockHash::new([8; 32]));
        evidence.insert(vote(1, Phase::Prevote, value)).unwrap();
        evidence.insert(vote(2, Phase::Prevote, None)).unwrap();
        assert!(evidence.quorum(Phase::Prevote, value).is_none());
        assert!(evidence.quorum(Phase::Prevote, None).is_none());
        let mut state = RoundState::new(&anchor(), address(3)).unwrap();
        state.timeout(&anchor(), 0, Step::Propose, &[]).unwrap();
        assert_eq!(
            state
                .timeout(
                    &anchor(),
                    0,
                    Step::Prevote,
                    &evidence.participation(Phase::Prevote)
                )
                .unwrap()
                .unwrap()
                .value,
            None
        );
        evidence.insert(vote(1, Phase::Precommit, None)).unwrap();
        assert_eq!(evidence.round_participation().len(), 2);
    }
}
