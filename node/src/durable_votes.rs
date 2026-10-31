//! Persist local voting intent before releasing a signed vote.

use burst_consensus::{GeneratedVote, VoteGenerator};
use burst_ledger::StateBlock;
use burst_store::{block::BlockStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::BlockHash;

#[derive(Debug, thiserror::Error)]
pub(crate) enum VoteGenerationError {
    #[error("vote dependencies are not confirmed")]
    DependenciesPending(Vec<BlockHash>),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Answer only for a locally applied canonical block. Final responses require
/// local cementation or a cached final promise validated against durable state.
pub(crate) fn respond_vote(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hash: BlockHash,
) -> Result<Option<GeneratedVote>, StoreError> {
    use burst_store::account::AccountStore;
    if !generator.is_representative {
        return Ok(None);
    }
    let block: StateBlock = match store.block_store().get_block(&hash) {
        Ok(bytes) => {
            bincode::deserialize(&bytes).map_err(|e| StoreError::Serialization(e.to_string()))?
        }
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let account = store.account_store().get_account(&block.account)?;
    let Some(height) = store.block_store().height_of_block(&hash)? else {
        return Ok(None);
    };
    if store
        .block_store()
        .block_at_height(&block.account, height)?
        != Some(hash)
    {
        return Ok(None);
    }
    if height > account.confirmation_height {
        // A final signature may precede local cementation. Repair must replay
        // that durable promise instead of creating another soft vote while
        // peers are still waiting for final quorum.
        if generator.cached_final_response(&hash).is_some() {
            return match generate_vote(store, generator, hash, true) {
                Ok(vote) => Ok(Some(vote)),
                Err(VoteGenerationError::DependenciesPending(_)) => Ok(None),
                Err(VoteGenerationError::Store(error)) => Err(error),
            };
        }
        if let Some(vote) = generator.cached_soft_response(&block.account, block.previous, hash) {
            if block.hash != hash || block.compute_hash() != hash {
                return Err(StoreError::Corruption("vote candidate hash mismatch".into()));
            }
            store.meta_store().validate_reserved_soft_vote(&block.payment_epoch,
                &generator.representative, &block.account, &block.previous, &hash,
                vote.sequence, block.timestamp.as_secs())?;
            return Ok(Some(vote.clone()));
        }
    }
    match generate_vote(
        store,
        generator,
        hash,
        height <= account.confirmation_height,
    ) {
        Ok(vote) => Ok(Some(vote)),
        Err(VoteGenerationError::DependenciesPending(_)) => Ok(None),
        Err(VoteGenerationError::Store(error)) => Err(error),
    }
}

/// Reserve voting intent before signing, or revalidate a cached final vote.
/// Final votes require confirmed dependencies; the candidate itself may still
/// be provisional. Candidate admission and signer eligibility belong to the caller.
pub(crate) fn generate_vote(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hash: BlockHash,
    is_final: bool,
) -> Result<GeneratedVote, VoteGenerationError> {
    let block = vote_candidate(store, hash)?;
    let governance_limit = if is_final { final_vote_limit(store, &block)? } else { None };
    if is_final && governance_limit.is_none() {
        if let Some(vote) = generator.cached_final_response(&hash) {
            store.meta_store().validate_reserved_final_vote(
                &block.payment_epoch, &generator.representative, &block.account,
                &block.previous, &hash, vote.sequence, block.timestamp.as_secs(),
            )?;
            let vote = vote.clone();
            generator.remember_response_choice(block.account, block.previous, &vote);
            return Ok(vote);
        }
    }
    let sequence = store
        .meta_store()
        .reserve_consensus_vote_with_governance_limit(
            &block.payment_epoch,
            &generator.representative,
            &block.account,
            &block.previous,
            &hash,
            is_final,
            generator.sequence(),
            Some(block.timestamp.as_secs()),
            governance_limit,
        )?;
    generator.advance_sequence_to(sequence - 1);
    let vote = if is_final {
        generator.generate_final_vote(hash)
    } else {
        generator.generate_vote(hash)
    };
    if is_final && governance_limit.is_none() {
        generator.remember_final_response(vote.clone());
    }
    generator.remember_response_choice(block.account, block.previous, &vote);
    Ok(vote)
}

fn vote_candidate(store: &LmdbStore, hash: BlockHash) -> Result<StateBlock, VoteGenerationError> {
    // Load stored or retained fork bytes and verify their hash. This does not
    // repeat signature, work or economic admission checks.
    let bytes = match store.block_store().get_block(&hash) {
        Ok(bytes) => bytes,
        Err(StoreError::NotFound(_)) => store
            .meta_store()
            .fork_candidate(&hash)?
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))?,
        Err(error) => return Err(error.into()),
    };
    let block: StateBlock = bincode::deserialize(&bytes)
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    if block.hash != hash || block.compute_hash() != hash {
        return Err(StoreError::Corruption("vote candidate hash mismatch".into()).into());
    }
    Ok(block)
}

fn final_vote_limit(store: &LmdbStore, block: &StateBlock) -> Result<Option<u64>, VoteGenerationError> {
    if needs_governance_readiness(block) {
        let batch = store.tx_begin_write()?;
        final_vote_limit_in(&batch, block)
    } else {
        let snapshot = crate::verification_commit::dependencies::view::Snapshot::new(store)?;
        check_final_dependencies(&snapshot, block)?;
        Ok(None)
    }
}

fn final_vote_limit_in(
    batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<Option<u64>, VoteGenerationError> {
    check_final_dependencies(batch, block)?;
    if needs_governance_readiness(block) {
        Ok(crate::confirmed_governance::ConfirmedGovernance::load(batch)?
            .map(|schedule| schedule.promise_limit(batch, block)).transpose()?.flatten())
    } else {
        Ok(None)
    }
}

fn needs_governance_readiness(block: &StateBlock) -> bool {
    matches!(block.block_type,
        burst_ledger::BlockType::GovernanceProposal | burst_ledger::BlockType::GovernanceEndorse)
}

fn check_final_dependencies(
    batch: &impl crate::verification_commit::dependencies::view::DependencyView,
    block: &StateBlock,
) -> Result<(), VoteGenerationError> {
    let pending = crate::causal_cementation::pending_dependencies(batch, block)?;
    if !pending.is_empty() {
        return Err(VoteGenerationError::DependenciesPending(pending));
    }
    Ok(())
}

mod final_batch;
pub(crate) use final_batch::{generate_final_votes, respond_votes};
pub(crate) mod worker;

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::vote_signing_payload;
    use burst_crypto::{generate_keypair, verify_signature};
    use burst_ledger::{BlockType, CURRENT_BLOCK_VERSION};
    use burst_types::{Signature, Timestamp, TxHash, WalletAddress};

    #[test]
    fn one_equivocating_rep_cannot_certify_both_partition_checkpoints() {
        use burst_consensus::checkpoint::{
            Checkpoint, CheckpointAnchor, CheckpointCertificate, CheckpointSignature,
            HumanDelegation, HumanRoster,
        };
        use burst_crypto::{derive_address, keypair_from_seed, sign_message};
        use burst_types::NetworkId;

        let keys: Vec<_> = (1..=4).map(|n| keypair_from_seed(&[n; 32])).collect();
        let addresses: Vec<_> = keys.iter().map(|key| derive_address(&key.public)).collect();
        let roster = HumanRoster::new(
            addresses
                .iter()
                .map(|address| HumanDelegation {
                    human: address.clone(),
                    representative: address.clone(),
                })
                .collect(),
        )
        .unwrap();
        let genesis = BlockHash::new([7; 32]);
        let first = Checkpoint {
            network: NetworkId::Dev,
            genesis,
            parent: genesis,
            height: 1,
            cutoff: 101,
            ledger_root: BlockHash::new([8; 32]),
            params_hash: BlockHash::new([9; 32]),
            next_roster: roster.commitment(),
            selection_root: BlockHash::new([10; 32]),
            next_quorum_bps: 6700,
        };
        let mut conflict = first.clone();
        conflict.ledger_root = BlockHash::new([11; 32]);
        let anchor = CheckpointAnchor::genesis(NetworkId::Dev, genesis, 100, roster, 6700).unwrap();
        let sign = |voter: usize, checkpoint: &Checkpoint| CheckpointSignature {
            representative: addresses[voter].clone(),
            signature: sign_message(
                &burst_consensus::checkpoint_round::Ballot::precommit(checkpoint, 0).signing_payload(),
                &keys[voter].private,
            ),
        };
        // Voter zero equivocates. Every other representative persists its choice.
        let mut first_certificate = CheckpointCertificate {
            checkpoint: first.clone(),
            round: 0,
            signatures: vec![sign(0, &first)],
        };
        let mut conflicting_certificate = CheckpointCertificate {
            checkpoint: conflict.clone(),
            round: 0,
            signatures: vec![sign(0, &conflict)],
        };
        let directories: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
        for (voter, checkpoint) in [(1, &first), (2, &first), (3, &conflict)] {
            anchor.validate_successor(checkpoint).unwrap();
            let store = LmdbStore::open(directories[voter].path(), 32, 16 * 1024 * 1024).unwrap();
            store
                .meta_store()
                .reserve_checkpoint_vote(
                    checkpoint.network,
                    &genesis,
                    &addresses[voter],
                    checkpoint.height,
                    &checkpoint.hash(),
                )
                .unwrap();
            if voter == 3 {
                conflicting_certificate.signatures.push(sign(voter, checkpoint));
            } else {
                first_certificate.signatures.push(sign(voter, checkpoint));
            }
        }
        let restarted = LmdbStore::open(directories[2].path(), 32, 16 * 1024 * 1024).unwrap();
        assert!(
            restarted
                .meta_store()
                .reserve_checkpoint_vote(
                    conflict.network,
                    &genesis,
                    &addresses[2],
                    conflict.height,
                    &conflict.hash(),
                )
                .is_err(),
            "the overlapping honest voter cannot sign the conflicting branch after restart"
        );
        assert!(anchor.verify_certificate(&first_certificate).is_ok());
        assert!(anchor.verify_certificate(&conflicting_certificate).is_err());
    }

    pub(super) fn opening() -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: WalletAddress::new("brst_alice"),
            previous: BlockHash::ZERO,
            representative: WalletAddress::new("brst_rep"),
            brn_spent: 0,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(100),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    pub(super) fn candidate(amount: u128) -> StateBlock {
        let opening = opening();
        let mut block = StateBlock {
            block_type: BlockType::Send,
            previous: opening.hash,
            trst_balance: amount,
            link: BlockHash::new([2; 32]),
            ..opening
        };
        block.hash = block.compute_hash();
        block
    }

    fn save_confirmed_opening(store: &LmdbStore) {
        let opening = opening();
        let account =
            crate::genesis_state::account_to_write(None, opening.account.clone(), opening.hash)
                .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, opening.payment_epoch).unwrap();
        batch
            .put_block_with_account(
                &opening.hash,
                &bincode::serialize(&opening).unwrap(),
                &opening.account,
                1,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch
            .advance_confirmation_height(&opening.account, 1)
            .unwrap();
        batch.commit().unwrap();
    }

    #[test]
    fn vote_by_hash_is_signed_and_cannot_promote_an_unconfirmed_or_cached_fork() {
        use burst_store::account::AccountStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        save_confirmed_opening(&store);
        let block = candidate(1);
        let keys = generate_keypair();
        let representative = burst_crypto::derive_address(&keys.public);
        let mut generator = VoteGenerator::new(representative.clone(), keys.private.0);
        let mut account =
            crate::genesis_state::account_to_write(None, block.account.clone(), block.hash)
                .unwrap();
        account.block_count = 2;
        account.confirmation_height = 1;
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(&block).unwrap(),
                &block.account,
                2,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        assert!(respond_vote(&store, &mut generator, block.hash)
            .unwrap()
            .is_none());
        generator.set_representative(true);
        let soft = respond_vote(&store, &mut generator, block.hash)
            .unwrap()
            .unwrap();
        assert!(!soft.is_final);
        let repeated_soft = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert_eq!(repeated_soft.signature, soft.signature);
        assert_eq!(repeated_soft.sequence, soft.sequence);
        assert_eq!(generator.sequence(), soft.sequence);
        let competing = candidate(2);
        store.meta_store().put_fork_candidate(&competing.hash,
            &bincode::serialize(&competing).unwrap()).unwrap();
        let fork_soft = generate_vote(&store, &mut generator, competing.hash, false).unwrap();
        let canonical_again = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert!(canonical_again.sequence > fork_soft.sequence);
        assert_ne!(canonical_again.signature, soft.signature);
        let canonical_repeat = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert_eq!(canonical_repeat.signature, canonical_again.signature);
        let promised = generate_vote(&store, &mut generator, block.hash, true).unwrap();
        let before_cementation = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert!(before_cementation.is_final);
        assert_eq!(before_cementation.signature, promised.signature);
        assert_eq!(before_cementation.sequence, promised.sequence);
        assert_eq!(generator.sequence(), promised.sequence);
        assert_eq!(store.account_store().get_account(&block.account).unwrap().confirmation_height, 1);
        account.confirmation_height = 2;
        store.account_store().put_account(&account).unwrap();
        let final_vote = respond_vote(&store, &mut generator, block.hash)
            .unwrap()
            .unwrap();
        assert!(final_vote.is_final);
        assert!(final_vote.sequence > soft.sequence);
        let reserved_sequence = generator.sequence();
        let repeated = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert_eq!(repeated.sequence, final_vote.sequence);
        assert_eq!(repeated.timestamp, final_vote.timestamp);
        assert_eq!(repeated.signature, final_vote.signature);
        assert_eq!(generator.sequence(), reserved_sequence);
        assert!(verify_signature(
            &vote_signing_payload(
                &representative,
                true,
                final_vote.timestamp,
                final_vote.sequence,
                &[block.hash]
            ),
            &final_vote.signature,
            &keys.public
        ));
        use burst_store::meta::{MetaStore, encode_payment_vote_limit, GOVERNANCE_COUNTING_LIMIT_KEY};
        store.meta_store().put_meta(GOVERNANCE_COUNTING_LIMIT_KEY,
            &encode_payment_vote_limit(block.payment_epoch, Some(block.timestamp.as_secs() - 1))).unwrap();
        assert!(respond_vote(&store, &mut generator, block.hash).is_err());
        assert_eq!(generator.sequence(), reserved_sequence);
        store.meta_store().delete_meta(GOVERNANCE_COUNTING_LIMIT_KEY).unwrap();
        let restored = respond_vote(&store, &mut generator, block.hash).unwrap().unwrap();
        assert_eq!(restored.signature, final_vote.signature);
        let fork = candidate(2);
        store
            .meta_store()
            .put_fork_candidate(&fork.hash, &bincode::serialize(&fork).unwrap())
            .unwrap();
        assert!(respond_vote(&store, &mut generator, fork.hash)
            .unwrap()
            .is_none());
        assert!(
            respond_vote(&store, &mut generator, BlockHash::new([99; 32]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn final_vote_waits_for_dependency_cementation_without_reserving_a_choice() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut opening = candidate(0);
        opening.block_type = BlockType::Open;
        opening.previous = BlockHash::ZERO;
        opening.link = BlockHash::ZERO;
        opening.hash = opening.compute_hash();
        let account =
            crate::genesis_state::account_to_write(None, opening.account.clone(), opening.hash)
                .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, opening.payment_epoch).unwrap();
        batch
            .put_block_with_account(
                &opening.hash,
                &bincode::serialize(&opening).unwrap(),
                &opening.account,
                1,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        let mut fork = opening.clone();
        fork.block_type = BlockType::ChangeRepresentative;
        fork.previous = opening.hash;
        fork.representative = WalletAddress::new("brst_other_rep");
        fork.hash = fork.compute_hash();
        store
            .meta_store()
            .put_fork_candidate(&fork.hash, &bincode::serialize(&fork).unwrap())
            .unwrap();
        let mut generator = VoteGenerator::new(WalletAddress::new("brst_rep"), [7; 32]);
        assert!(matches!(generate_vote(&store, &mut generator, fork.hash, true),
            Err(VoteGenerationError::DependenciesPending(dependencies)) if dependencies == vec![opening.hash]));
        assert_eq!(generator.sequence(), 0);
        store
            .meta_store()
            .enqueue_confirmation(&opening.hash)
            .unwrap();
        assert!(matches!(generate_vote(&store, &mut generator, fork.hash, true),
            Err(VoteGenerationError::DependenciesPending(dependencies)) if dependencies == vec![opening.hash]));
        assert_eq!(generator.sequence(), 0);
        let soft = generate_vote(&store, &mut generator, fork.hash, false).unwrap();
        assert_eq!(soft.sequence, 1);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .advance_confirmation_height(&opening.account, 1)
            .unwrap();
        batch.commit().unwrap();
        let final_vote = generate_vote(&store, &mut generator, fork.hash, true).unwrap();
        assert_eq!(final_vote.sequence, 2);
        assert!(final_vote.is_final);
        assert!(!store.block_store().exists(&fork.hash).unwrap());
    }

    #[test]
    fn cached_fork_can_be_voted_without_installing_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        save_confirmed_opening(&store);
        let key = generate_keypair();
        let fork = candidate(7);
        store
            .meta_store()
            .put_fork_candidate(&fork.hash, &bincode::serialize(&fork).unwrap())
            .unwrap();
        let mut generator = VoteGenerator::new(WalletAddress::new("brst_rep"), key.private.0);
        let vote = generate_vote(&store, &mut generator, fork.hash, true).unwrap();
        assert_eq!(vote.sequence, 1);
        assert!(!store.block_store().exists(&fork.hash).unwrap());
        let conflict = candidate(8);
        store
            .meta_store()
            .put_fork_candidate(&conflict.hash, &bincode::serialize(&conflict).unwrap())
            .unwrap();
        assert!(generate_vote(&store, &mut generator, conflict.hash, true).is_err());
        assert!(generate_vote(&store, &mut generator, conflict.hash, false).is_err());
    }

    #[test]
    fn restarted_signer_cannot_final_or_soft_vote_for_a_competing_block() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate_keypair();
        let first = candidate(10);
        let fork = candidate(9);
        {
            let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
            save_confirmed_opening(&store);
            for block in [&first, &fork] {
                store
                    .block_store()
                    .put_block(&block.hash, &bincode::serialize(block).unwrap())
                    .unwrap();
            }
            let mut generator = VoteGenerator::new(WalletAddress::new("brst_rep"), key.private.0);
            assert_eq!(
                generate_vote(&store, &mut generator, first.hash, true)
                    .unwrap()
                    .sequence,
                1
            );
        }
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut generator = VoteGenerator::new(WalletAddress::new("brst_rep"), key.private.0);
        assert!(generate_vote(&store, &mut generator, fork.hash, true).is_err());
        assert!(generate_vote(&store, &mut generator, fork.hash, false).is_err());
        assert_eq!(generator.sequence(), 0, "no signing on reservation failure");
        let vote = generate_vote(&store, &mut generator, first.hash, true).unwrap();
        assert_eq!(vote.sequence, 2);
        assert!(verify_signature(
            &vote_signing_payload(
                &vote.voter,
                vote.is_final,
                vote.timestamp,
                vote.sequence,
                &[vote.block_hash]
            ),
            &vote.signature,
            &key.public
        ));
    }

    #[test]
    fn missing_and_corrupt_candidates_are_never_signed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut generator = VoteGenerator::new(WalletAddress::new("brst_rep"), [7; 32]);
        let mut block = candidate(10);
        assert!(generate_vote(&store, &mut generator, block.hash, true).is_err());
        block.trst_balance = 11;
        store
            .block_store()
            .put_block(&block.hash, &bincode::serialize(&block).unwrap())
            .unwrap();
        assert!(generate_vote(&store, &mut generator, block.hash, true).is_err());
        assert_eq!(generator.sequence(), 0);
    }
}
