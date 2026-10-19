//! Frozen selection inputs for the checkpoint protocol. The node must derive
//! these inputs by replaying the checkpoint's ledger cut BEFORE signing it.
//! Certificate validation cannot prove pool completeness or signing time alone.

use burst_consensus::checkpoint::{Checkpoint, CheckpointAnchor, CheckpointCertificate};
use burst_crypto::{blake2b_256, blake2b_256_multi, decode_address};
use burst_types::{BlockHash, WalletAddress};
use burst_vrf::{drand::DRAND_MAINNET_CHAIN, DrandPolicy, RandomOutput};
#[cfg(test)]
use burst_vrf::DrandClient;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SelectionError {
    #[error("invalid or non-canonical selection request")]
    InvalidRequest,
    #[error("selection request does not match the checkpoint")]
    WrongCheckpoint,
    #[error("checkpoint has no valid quorum certificate")]
    InvalidCertificate,
    #[error("selection commitment must be signed before its beacon can be known")]
    CommitmentTooLate,
    #[error("selection beacon has the wrong round or an invalid proof")]
    InvalidBeacon,
}

/// One request per checkpoint selection root in v1. Pool eligibility, exclusions,
/// count and cutoff must be validated from the same agreed ledger cut, not from
/// a requester's list or the receiver's wall clock. Fields are private; decoded
/// requests are revalidated before use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRequest {
    target: WalletAddress,
    trigger: BlockHash,
    attempt: u32,
    cutoff: u64,
    params_hash: BlockHash,
    pool: Vec<WalletAddress>,
    count: u32,
    round: u64,
}

impl SelectionRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: WalletAddress,
        trigger: BlockHash,
        attempt: u32,
        cutoff: u64,
        params_hash: BlockHash,
        mut pool: Vec<WalletAddress>,
        count: u32,
    ) -> Result<Self, SelectionError> {
        pool.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let round = DrandPolicy::mainnet()
            .map_err(|_| SelectionError::InvalidRequest)?
            .chain_info()
            .round_after(cutoff)
            .map_err(|_| SelectionError::InvalidRequest)?;
        let request = Self {
            target,
            trigger,
            attempt,
            cutoff,
            params_hash,
            pool,
            count,
            round,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn round(&self) -> u64 {
        self.round
    }
    pub fn target(&self) -> &WalletAddress {
        &self.target
    }

    pub fn trigger(&self) -> BlockHash {
        self.trigger
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn cutoff(&self) -> u64 {
        self.cutoff
    }

    pub fn params_hash(&self) -> BlockHash {
        self.params_hash
    }

    pub fn beacon_time(&self) -> Result<u64, SelectionError> {
        DrandPolicy::mainnet()
            .map_err(|_| SelectionError::InvalidRequest)?
            .chain_info()
            .time_of_round(self.round)
            .map_err(|_| SelectionError::InvalidRequest)
    }

    /// The checkpoint hash also binds the network, genesis and parent roster
    /// history. A request digest alone would allow cross-network vote replay.
    pub fn vote_context(&self, checkpoint: &Checkpoint) -> Result<BlockHash, SelectionError> {
        self.validate_checkpoint(checkpoint)?;
        Ok(BlockHash::new(blake2b_256_multi(&[
            b"BURST/verification-vote-context/v1\0",
            checkpoint.hash().as_bytes(),
            self.commitment()?.as_bytes(),
        ])))
    }

    pub fn commitment(&self) -> Result<BlockHash, SelectionError> {
        self.validate()?;
        let mut bytes = b"BURST/selection-request/v1\0".to_vec();
        bytes.extend_from_slice(DRAND_MAINNET_CHAIN.as_bytes());
        bytes.extend_from_slice(
            &decode_address(self.target.as_str()).ok_or(SelectionError::InvalidRequest)?,
        );
        bytes.extend_from_slice(self.trigger.as_bytes());
        bytes.extend_from_slice(&self.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.cutoff.to_be_bytes());
        bytes.extend_from_slice(self.params_hash.as_bytes());
        bytes.extend_from_slice(&self.count.to_be_bytes());
        bytes.extend_from_slice(&self.round.to_be_bytes());
        bytes.extend_from_slice(&(self.pool.len() as u64).to_be_bytes());
        for wallet in &self.pool {
            bytes.extend_from_slice(
                &decode_address(wallet.as_str()).ok_or(SelectionError::InvalidRequest)?,
            );
        }
        Ok(BlockHash::new(blake2b_256(&bytes)))
    }

    fn validate(&self) -> Result<(), SelectionError> {
        if decode_address(self.target.as_str()).is_none()
            || self.trigger == BlockHash::ZERO
            || self.params_hash == BlockHash::ZERO
            || self.count == 0
            || self.pool.len() < self.count as usize
            || self.pool.len() > 1_000_000
            || self
                .pool
                .iter()
                .any(|w| w == &self.target || decode_address(w.as_str()).is_none())
            || self.pool.windows(2).any(|w| w[0].as_str() >= w[1].as_str())
        {
            return Err(SelectionError::InvalidRequest);
        }
        let expected = DrandPolicy::mainnet()
            .map_err(|_| SelectionError::InvalidRequest)?
            .chain_info()
            .round_after(self.cutoff)
            .map_err(|_| SelectionError::InvalidRequest)?;
        if self.round != expected {
            return Err(SelectionError::InvalidRequest);
        }
        Ok(())
    }

    fn validate_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), SelectionError> {
        if checkpoint.selection_root != self.commitment()?
            || checkpoint.cutoff != self.cutoff
            || checkpoint.params_hash != self.params_hash
        {
            return Err(SelectionError::WrongCheckpoint);
        }
        Ok(())
    }

    /// Call before durable reservation/signing, after ledger replay validation.
    /// `clock_error_bound` is a conservative bound, not the measured offset.
    /// Stop signing if the clock is untrusted. This check relies on drand's
    /// unpredictability assumption and the signer's clock bound.
    pub fn validate_before_signing(
        &self,
        checkpoint: &Checkpoint,
        now: u64,
        clock_error_bound: u64,
    ) -> Result<(), SelectionError> {
        self.validate_checkpoint(checkpoint)?;
        let release = DrandPolicy::mainnet()
            .map_err(|_| SelectionError::InvalidRequest)?
            .chain_info()
            .time_of_round(self.round)
            .map_err(|_| SelectionError::InvalidRequest)?;
        let latest_possible_now = now
            .checked_add(clock_error_bound)
            .ok_or(SelectionError::CommitmentTooLate)?;
        if now < self.cutoff || latest_possible_now >= release {
            return Err(SelectionError::CommitmentTooLate);
        }
        Ok(())
    }

    /// Select using precisely the certified inputs. Do not re-filter at current
    /// time: doing so would change eligibility after the randomness is known.
    /// A valid certificate assumes honest signers checked completeness and the
    /// pre-beacon deadline; the signature itself is not a timestamp proof.
    pub fn select(
        &self,
        parent: &CheckpointAnchor,
        certificate: &CheckpointCertificate,
        output: &RandomOutput,
    ) -> Result<Vec<WalletAddress>, SelectionError> {
        self.validate_checkpoint(&certificate.checkpoint)?;
        parent
            .verify_certificate(certificate)
            .map_err(|_| SelectionError::InvalidCertificate)?;
        let policy = DrandPolicy::mainnet().map_err(|_| SelectionError::InvalidBeacon)?;
        if output.round != self.round || !matches!(policy.verify(output), Ok(true)) {
            return Err(SelectionError::InvalidBeacon);
        }
        let mut scored: Vec<_> = self
            .pool
            .iter()
            .map(|wallet| {
                (
                    blake2b_256_multi(&[&output.value, wallet.as_str().as_bytes()]),
                    wallet,
                )
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
        Ok(scored
            .into_iter()
            .take(self.count as usize)
            .map(|(_, wallet)| wallet.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    mod challenge_deadline;
    mod deadline;
    mod snapshot_proofs;

    use super::*;
    use crate::{OrchestratorSnapshot, VerificationOrchestrator, Vote};
    use burst_consensus::checkpoint::{CheckpointSignature, HumanDelegation, HumanRoster};
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};
    use burst_types::NetworkId;
    use burst_types::{ProtocolParams, Timestamp};
    use burst_vrf::DrandBeacon;

    const GENESIS: u64 = 1_595_431_050;

    fn address(n: u8) -> WalletAddress {
        derive_address(&keypair_from_seed(&[n; 32]).public)
    }
    fn request(pool: &[u8]) -> SelectionRequest {
        SelectionRequest::new(
            address(99),
            BlockHash::new([5; 32]),
            0,
            GENESIS - 1,
            BlockHash::new([3; 32]),
            pool.iter().map(|n| address(*n)).collect(),
            2,
        )
        .unwrap()
    }
    fn certificate(request: &SelectionRequest) -> (CheckpointAnchor, CheckpointCertificate) {
        let roster = HumanRoster::new(vec![HumanDelegation {
            human: address(4),
            representative: address(4),
        }])
        .unwrap();
        let cp = Checkpoint {
            network: NetworkId::Dev,
            genesis: BlockHash::new([1; 32]),
            parent: BlockHash::new([1; 32]),
            height: 1,
            cutoff: request.cutoff,
            ledger_root: BlockHash::new([2; 32]),
            params_hash: request.params_hash,
            next_roster: roster.commitment(),
            selection_root: request.commitment().unwrap(),
            next_quorum_bps: 6700,
        };
        let parent =
            CheckpointAnchor::genesis(NetworkId::Dev, cp.genesis, GENESIS - 2, roster, 6700)
                .unwrap();
        request
            .validate_before_signing(&cp, GENESIS - 1, 0)
            .unwrap();
        let signature = sign_message(
            &burst_consensus::checkpoint_round::Ballot::precommit(&cp, 0).signing_payload(),
            &keypair_from_seed(&[4; 32]).private,
        );
        (
            parent,
            CheckpointCertificate {
                checkpoint: cp,
                round: 0,
                signatures: vec![CheckpointSignature {
                    representative: address(4),
                    signature,
                }],
            },
        )
    }
    fn output() -> RandomOutput {
        DrandClient::beacon_to_random_output(&DrandBeacon {
            round: 1,
            randomness: "101297f1ca7dc44ef6088d94ad5fb7ba03455dc33d53ddb412bbc4564ed986ec".into(),
            signature: "8d61d9100567de44682506aea1a7a6fa6e5491cd27a0a0ed349ef6910ac5ac20ff7bc3e09d7c046566c9f7f3c6f3b10104990e7cb424998203d8f7de586fb7fa5f60045417a432684f85093b06ca91c769f0e7ca19268375e659c2a2352b4655".into(),
            previous_signature: Some("176f93498eac9ca337150b46d21dd58673ea4e3581185f869672e59fa4cb390a".into()),
        }).unwrap()
    }

    fn pending() -> (VerificationOrchestrator, ProtocolParams, SelectionRequest) {
        let params = ProtocolParams {
            endorsement_threshold: 1,
            endorsement_burn_amount: 100,
            num_verifiers: 2,
            ..ProtocolParams::default()
        };
        let mut orchestrator = VerificationOrchestrator::new();
        orchestrator
            .process_endorsement_at(
                &address(99),
                &address(90),
                100,
                &params,
                Timestamp::new(GENESIS - 2),
            )
            .unwrap();
        orchestrator.drain_events();
        let request = orchestrator
            .prepare_selection(
                &address(99),
                BlockHash::new([5; 32]),
                Timestamp::new(GENESIS - 1),
                &[address(1), address(2), address(90), address(99)],
                &params,
            )
            .unwrap();
        assert_eq!(request.pool.len(), 2);
        assert!(request.pool.contains(&address(1)) && request.pool.contains(&address(2)));
        (orchestrator, params, request)
    }

    #[test]
    fn certified_challenge_keeps_its_signed_origin_through_restart_and_outcome() {
        for reason in [
            crate::ChallengeReason::Fraud,
            crate::ChallengeReason::Inactivity,
        ] {
            let params = ProtocolParams {
                num_verifiers: 2,
                ..ProtocolParams::default()
            };
            let target = address(99);
            let challenge = BlockHash::new([71; 32]);
            let mut orchestrator = VerificationOrchestrator::new();
            orchestrator
                .genesis_verify_at(&target, &address(4), 1, 50, Timestamp::new(GENESIS - 2))
                .unwrap();
            orchestrator.drain_events();
            orchestrator
                .initiate_signed_challenge_at(
                    &target,
                    &address(3),
                    true,
                    params.challenge_stake_amount,
                    (reason, Timestamp::new(GENESIS - 1), challenge),
                    &params,
                )
                .unwrap();
            let request = orchestrator
                .prepare_selection(
                    &target,
                    challenge,
                    Timestamp::new(GENESIS - 1),
                    &[address(1), address(2)],
                    &params,
                )
                .unwrap();
            let (parent, certificate) = certificate(&request);
            let context = orchestrator
                .install_selection(&target, &parent, &certificate, &output())
                .unwrap();
            orchestrator.drain_events();
            orchestrator
                .process_certified_vote_at(
                    &target,
                    &address(1),
                    Vote::Illegitimate,
                    context,
                    &params,
                    Timestamp::new(GENESIS),
                )
                .unwrap();
            assert!(orchestrator
                .try_resolve_challenge(&target, &params)
                .unwrap()
                .is_none());
            let mut restored = VerificationOrchestrator::restore(
                OrchestratorSnapshot::decode(&orchestrator.snapshot().encode().unwrap()).unwrap(),
            );
            assert_eq!(
                restored
                    .install_selection(&target, &parent, &certificate, &output())
                    .unwrap(),
                context
            );
            restored
                .process_certified_vote_at(
                    &target,
                    &address(2),
                    Vote::Illegitimate,
                    context,
                    &params,
                    Timestamp::new(GENESIS),
                )
                .unwrap();
            let Some(crate::VerificationEvent::ChallengeResolved { outcome, .. }) =
                restored.try_resolve_challenge(&target, &params).unwrap()
            else {
                panic!("missing challenge outcome")
            };
            assert_eq!(outcome.challenge_context, Some(challenge));
            assert!(outcome
                .verifier_outcomes
                .iter()
                .all(|voter| voter.selection_context == Some(context)));
            let events = restored.drain_events();
            match reason {
                crate::ChallengeReason::Fraud => assert!(events.iter().any(|event| matches!(
                    event,
                    crate::VerificationEvent::WalletUnverified { .. }
                ))),
                crate::ChallengeReason::Inactivity => {
                    assert!(events.iter().any(|event| matches!(
                        event,
                        crate::VerificationEvent::WalletDeactivated { .. }
                    )));
                    assert!(!events.iter().any(|event| matches!(
                        event,
                        crate::VerificationEvent::WalletUnverified { .. }
                    )));
                }
            }
        }
    }

    #[test]
    fn restored_unbound_challenge_cannot_install_or_vote_into_an_old_selection() {
        for installed in [false, true] {
            let (mut orchestrator, params, request) = pending();
            let (parent, certificate) = certificate(&request);
            let context = request.vote_context(&certificate.checkpoint).unwrap();
            if installed {
                orchestrator
                    .install_selection(&address(99), &parent, &certificate, &output())
                    .unwrap();
            }
            let mut snapshot = orchestrator.snapshot();
            snapshot.active_challenges.insert(
                address(99),
                crate::challenge::Challenge {
                    challenger: address(3),
                    target: address(99),
                    stake_amount: params.challenge_stake_amount,
                    initiated_at: Timestamp::new(GENESIS - 1),
                    reason: crate::ChallengeReason::Fraud,
                },
            );
            if !installed {
                snapshot.states.get_mut(&address(99)).unwrap().phase =
                    crate::state::VerificationPhase::Challenged;
            }
            let mut restored = VerificationOrchestrator::restore(snapshot);
            let before = restored.snapshot().encode().unwrap();
            let error = restored
                .install_selection(&address(99), &parent, &certificate, &output())
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("challenge has no signed context"),
                "{error}"
            );
            assert!(restored
                .freeze_replayed_selection(&request, &params)
                .is_err());
            if installed {
                let error = restored
                    .process_certified_vote_at(
                        &address(99),
                        &address(1),
                        Vote::Legitimate,
                        context,
                        &params,
                        Timestamp::new(GENESIS),
                    )
                    .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("challenge has no signed context"),
                    "{error}"
                );
            }
            assert_eq!(restored.snapshot().encode().unwrap(), before);
            assert!(restored.drain_events().is_empty());
        }
    }

    #[test]
    fn frozen_request_and_certificate_survive_restart_without_losing_votes() {
        let (orchestrator, params, request) = pending();
        let (parent, certificate) = certificate(&request);
        let mut restored = VerificationOrchestrator::restore(
            OrchestratorSnapshot::decode(&orchestrator.snapshot().encode().unwrap()).unwrap(),
        );
        assert!(restored.selection_context(&address(99)).is_none());
        assert_eq!(restored.selection_request(&address(99)), Some(&request));
        let context = restored
            .install_selection(&address(99), &parent, &certificate, &output())
            .unwrap();
        assert_eq!(
            restored.get_state(&address(99)).unwrap().selected_verifiers,
            request.select(&parent, &certificate, &output()).unwrap()
        );
        restored
            .validate_certified_vote(
                &address(99),
                &address(1),
                context,
                params.params_hash(),
                Timestamp::new(GENESIS),
            )
            .unwrap();
        assert!(restored
            .process_vote_at(
                &address(99),
                &address(1),
                Vote::Legitimate,
                &params,
                Timestamp::new(GENESIS)
            )
            .is_err());
        restored
            .process_certified_vote_at(
                &address(99),
                &address(1),
                Vote::Legitimate,
                context,
                &params,
                Timestamp::new(GENESIS),
            )
            .unwrap();
        let mut restored = VerificationOrchestrator::restore(
            OrchestratorSnapshot::decode(&restored.snapshot().encode().unwrap()).unwrap(),
        );
        assert_eq!(
            restored
                .install_selection(&address(99), &parent, &certificate, &output())
                .unwrap(),
            context
        );
        assert_eq!(restored.get_state(&address(99)).unwrap().votes.len(), 1);
        assert!(restored
            .validate_certified_vote(
                &address(99),
                &address(1),
                context,
                params.params_hash(),
                Timestamp::new(GENESIS)
            )
            .is_err());
        restored
            .validate_certified_vote(
                &address(99),
                &address(2),
                context,
                params.params_hash(),
                Timestamp::new(GENESIS),
            )
            .unwrap();
    }

    #[test]
    fn wrong_certificates_beacons_and_changed_inputs_do_not_mutate_pending_selection() {
        let (mut orchestrator, params, request) = pending();
        let (parent, mut cert) = certificate(&request);
        let before = orchestrator.snapshot().encode().unwrap();
        let mut wrong = output();
        wrong.round += 1;
        assert!(orchestrator
            .install_selection(&address(99), &parent, &cert, &wrong)
            .is_err());
        cert.signatures.clear();
        assert!(orchestrator
            .install_selection(&address(99), &parent, &cert, &output())
            .is_err());
        assert!(orchestrator
            .prepare_selection(
                &address(99),
                BlockHash::new([6; 32]),
                Timestamp::new(GENESIS - 1),
                &[address(1), address(2)],
                &params
            )
            .is_err());
        assert!(orchestrator
            .select_verifiers(&address(99), &[address(1), address(2)], &[3; 32], &params)
            .is_err());
        assert!(orchestrator
            .process_endorsement_at(
                &address(99),
                &address(91),
                100,
                &params,
                Timestamp::new(GENESIS - 1)
            )
            .is_err());
        assert_eq!(orchestrator.snapshot().encode().unwrap(), before);
        assert!(orchestrator.drain_events().is_empty());
    }

    #[test]
    fn votes_bind_network_checkpoint_parameters_and_beacon_release() {
        let (mut orchestrator, params, request) = pending();
        let (parent, cert) = certificate(&request);
        let context = orchestrator
            .install_selection(&address(99), &parent, &cert, &output())
            .unwrap();
        let mut other = cert.checkpoint.clone();
        other.network = NetworkId::Test;
        let foreign = request.vote_context(&other).unwrap();
        assert_ne!(foreign, context);
        for (context, hash, at) in [
            (foreign, params.params_hash(), GENESIS),
            (BlockHash::ZERO, params.params_hash(), GENESIS),
            (context, BlockHash::new([7; 32]), GENESIS),
            (context, params.params_hash(), GENESIS - 1),
        ] {
            assert!(orchestrator
                .validate_certified_vote(
                    &address(99),
                    &address(1),
                    context,
                    hash,
                    Timestamp::new(at)
                )
                .is_err());
        }
        assert!(orchestrator
            .get_state(&address(99))
            .unwrap()
            .votes
            .is_empty());
    }

    #[test]
    fn later_penalties_do_not_refilter_the_certified_pool() {
        let (orchestrator, params, request) = pending();
        let (parent, certificate) = certificate(&request);
        let mut snapshot = orchestrator.snapshot();
        snapshot
            .penalized_verifiers
            .insert(address(1), GENESIS + 100_000);
        let mut restored = VerificationOrchestrator::restore(snapshot);
        let context = restored
            .install_selection(&address(99), &parent, &certificate, &output())
            .unwrap();
        assert!(restored.is_penalized(&address(1), GENESIS));
        assert!(restored
            .get_state(&address(99))
            .unwrap()
            .selected_verifiers
            .contains(&address(1)));
        restored
            .validate_certified_vote(
                &address(99),
                &address(1),
                context,
                params.params_hash(),
                Timestamp::new(GENESIS),
            )
            .unwrap();
    }

    #[test]
    fn revote_discards_old_context_and_requires_a_new_frozen_pool() {
        let (mut orchestrator, params, request) = pending();
        let (parent, cert) = certificate(&request);
        let context = orchestrator
            .install_selection(&address(99), &parent, &cert, &output())
            .unwrap();
        for voter in [address(1), address(2)] {
            orchestrator
                .process_certified_vote_at(
                    &address(99),
                    &voter,
                    Vote::Illegitimate,
                    context,
                    &params,
                    Timestamp::new(GENESIS),
                )
                .unwrap();
        }
        assert_eq!(
            orchestrator.get_state(&address(99)).unwrap().revote_count,
            1
        );
        assert!(orchestrator.selection_context(&address(99)).is_none());
        assert!(orchestrator
            .install_selection(&address(99), &parent, &cert, &output())
            .is_err());
        assert!(orchestrator
            .validate_certified_vote(
                &address(99),
                &address(1),
                context,
                params.params_hash(),
                Timestamp::new(GENESIS)
            )
            .is_err());
        assert!(orchestrator
            .prepare_selection(
                &address(99),
                BlockHash::new([6; 32]),
                Timestamp::new(GENESIS - 1),
                &[address(3), address(4)],
                &params
            )
            .is_err());
        let next = orchestrator
            .prepare_selection(
                &address(99),
                BlockHash::new([6; 32]),
                Timestamp::new(GENESIS),
                &[address(1), address(2), address(3), address(4)],
                &params,
            )
            .unwrap();
        assert_eq!(next.attempt, 1);
        assert_eq!(next.pool.len(), 2);
        assert!(!next.pool.contains(&address(1)) && !next.pool.contains(&address(2)));
        assert_ne!(next.commitment().unwrap(), request.commitment().unwrap());
        assert_ne!(next.round(), request.round());
    }

    #[test]
    fn reordered_inputs_and_restored_request_select_the_same_committee() {
        let first = request(&[1, 2, 3]);
        let other = request(&[3, 1, 2]);
        assert_eq!(first, other);
        let (parent, cert) = certificate(&first);
        let restored: SelectionRequest =
            bincode::deserialize(&bincode::serialize(&first).unwrap()).unwrap();
        let selected = first.select(&parent, &cert, &output()).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(other.select(&parent, &cert, &output()).unwrap(), selected);
        assert_eq!(
            restored.select(&parent, &cert, &output()).unwrap(),
            selected
        );
    }

    #[test]
    fn changed_pool_target_trigger_attempt_cutoff_or_parameters_cannot_reuse_certificate() {
        let first = request(&[1, 2, 3]);
        let (parent, cert) = certificate(&first);
        let mut changed = vec![request(&[1, 2, 5])];
        let mut r = first.clone();
        r.target = address(98);
        changed.push(r);
        let mut r = first.clone();
        r.trigger = BlockHash::new([6; 32]);
        changed.push(r);
        let mut r = first.clone();
        r.attempt = 1;
        changed.push(r);
        let mut r = first.clone();
        r.params_hash = BlockHash::new([7; 32]);
        changed.push(r);
        let mut r = first.clone();
        r.cutoff -= 1;
        changed.push(r);
        let mut r = first.clone();
        r.count = 1;
        changed.push(r);
        for altered in changed {
            assert_eq!(
                altered.select(&parent, &cert, &output()),
                Err(SelectionError::WrongCheckpoint)
            );
        }
    }

    #[test]
    fn known_beacon_cannot_be_committed_with_a_backdated_cutoff() {
        let request = request(&[1, 2, 3]);
        let (_, cert) = certificate(&request);
        for (now, error_bound) in [
            (GENESIS, 0),
            (GENESIS - 1, 1),
            (GENESIS + 1000, 0),
            (u64::MAX, 1),
        ] {
            assert_eq!(
                request.validate_before_signing(&cert.checkpoint, now, error_bound),
                Err(SelectionError::CommitmentTooLate)
            );
        }
        assert!(
            request
                .validate_before_signing(&cert.checkpoint, GENESIS - 2, 0)
                .is_err(),
            "no future-dated cutoff"
        );
    }

    #[test]
    fn wrong_round_forged_beacon_and_missing_quorum_fail_closed() {
        let request = request(&[1, 2, 3]);
        let (parent, mut cert) = certificate(&request);
        let mut beacon = output();
        beacon.round += 1;
        assert_eq!(
            request.select(&parent, &cert, &beacon),
            Err(SelectionError::InvalidBeacon)
        );
        let mut beacon = output();
        beacon.value[0] ^= 1;
        assert_eq!(
            request.select(&parent, &cert, &beacon),
            Err(SelectionError::InvalidBeacon)
        );
        cert.signatures.clear();
        assert_eq!(
            request.select(&parent, &cert, &output()),
            Err(SelectionError::InvalidCertificate)
        );
    }

    #[test]
    fn duplicate_or_short_pools_and_decoded_round_mutation_are_rejected() {
        for pool in [
            vec![address(1)],
            vec![address(1), address(1)],
            vec![address(1), address(99)],
        ] {
            assert!(SelectionRequest::new(
                address(99),
                BlockHash::new([5; 32]),
                0,
                GENESIS - 1,
                BlockHash::new([3; 32]),
                pool,
                2
            )
            .is_err());
        }
        let mut request = request(&[1, 2, 3]);
        request.round += 1;
        assert_eq!(request.commitment(), Err(SelectionError::InvalidRequest));
    }
}
