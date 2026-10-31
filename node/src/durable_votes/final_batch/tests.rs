use super::*;
use crate::durable_votes::tests::{candidate, opening};
use burst_store::meta::MetaStore;
use burst_types::WalletAddress;

fn independent(store: &LmdbStore, id: u8, confirmed: bool) -> StateBlock {
    let mut opening = opening();
    opening.account = WalletAddress::new(format!("brst_group_account_{id}"));
    opening.hash = opening.compute_hash();
    let account =
        crate::genesis_state::account_to_write(None, opening.account.clone(), opening.hash)
            .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    if batch
        .get_meta(burst_store::meta::PAYMENT_EPOCH_KEY)
        .unwrap()
        .is_none()
    {
        crate::payment_epoch::initialize(&mut batch, opening.payment_epoch).unwrap();
    }
    batch
        .put_block_with_account(
            &opening.hash,
            &bincode::serialize(&opening).unwrap(),
            &opening.account,
            1,
        )
        .unwrap();
    batch.put_account_info(&account, false).unwrap();
    if confirmed {
        batch
            .advance_confirmation_height(&opening.account, 1)
            .unwrap();
    }
    batch.commit().unwrap();
    let mut block = candidate(u128::from(id));
    block.account = opening.account;
    block.previous = opening.hash;
    block.hash = block.compute_hash();
    store
        .meta_store()
        .put_fork_candidate(&block.hash, &bincode::serialize(&block).unwrap())
        .unwrap();
    block
}

fn signer() -> (burst_types::KeyPair, VoteGenerator) {
    let key = burst_crypto::keypair_from_seed(&[7; 32]);
    let generator = VoteGenerator::new(burst_crypto::derive_address(&key.public), key.private.0);
    (key, generator)
}

#[test]
fn indexed_predecessor_deferral_skips_bodies_but_never_authorizes_a_signature() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let pending = independent(&store, 1, false);
    let ready = independent(&store, 2, true);
    let mut batch = store.tx_begin_write().unwrap();
    let mut account = batch.get_account(&pending.account).unwrap();
    account.head = pending.hash;
    account.block_count = 2;
    batch.put_account_info(&account, false).unwrap();
    // An unreadable body distinguishes an index-only deferral from full loading.
    // Once its predecessor is confirmed, that body must still fail validation.
    batch
        .put_block_with_account(&pending.hash, &[0], &pending.account, 2)
        .unwrap();
    batch.commit().unwrap();
    let (_, mut generator) = signer();
    let generation = store.env().info().last_txn_id;
    let result = generate_final_votes(&store, &mut generator, &[pending.hash, ready.hash]);
    assert!(
        matches!(&result[0], Err(VoteGenerationError::DependenciesPending(hashes))
        if hashes == &vec![pending.previous])
    );
    assert_eq!(result[1].as_ref().unwrap().block_hash, ready.hash);
    assert_eq!(generator.sequence(), 1);
    assert_eq!(store.env().info().last_txn_id, generation + 1);
    assert!(generator.cached_final_response(&pending.hash).is_none());
    let snapshot = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .advance_confirmation_height(&pending.account, 1)
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(
        indexed_pending_predecessor(&store, &snapshot, &pending.hash).unwrap(),
        Some(pending.previous)
    );
    drop(snapshot);
    assert!(
        indexed_pending_predecessor(&store, &store.tx_begin_read().unwrap(), &pending.hash)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        &generate_final_votes(&store, &mut generator, &[pending.hash])[0],
        Err(VoteGenerationError::Store(_))
    ));
    assert_eq!(generator.sequence(), 1);
    store
        .block_store()
        .put_block(&pending.hash, &bincode::serialize(&pending).unwrap())
        .unwrap();
    let vote = generate_final_votes(&store, &mut generator, &[pending.hash])
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(vote.block_hash, pending.hash);
    assert_eq!(vote.sequence, 2);
    // A new call always rereads the index/account snapshot; no wakeup is required.
    assert!(
        indexed_pending_predecessor(&store, &store.tx_begin_read().unwrap(), &ready.hash)
            .unwrap()
            .is_none()
    );
}

#[test]
fn grouped_final_votes_sign_after_one_durable_cut_and_preserve_restart_choices() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let first = independent(&store, 1, true);
    let second = independent(&store, 2, true);
    let (key, mut generator) = signer();
    let generation = store.env().info().last_txn_id;
    let votes: Vec<_> = generate_final_votes(&store, &mut generator, &[first.hash, second.hash])
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(store.env().info().last_txn_id, generation + 1);
    assert_eq!(generator.sequence(), 2);
    for (index, vote) in votes.iter().enumerate() {
        assert_eq!(vote.sequence, index as u64 + 1);
        assert!(vote.is_final);
        assert!(burst_crypto::verify_signature(
            &burst_consensus::vote_signing_payload(
                &vote.voter,
                true,
                vote.timestamp,
                vote.sequence,
                &[vote.block_hash]
            ),
            &vote.signature,
            &key.public
        ));
    }
    let again: Vec<_> = generate_final_votes(&store, &mut generator, &[second.hash, first.hash])
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(again[0].signature, votes[1].signature);
    assert_eq!(again[1].signature, votes[0].signature);
    assert_eq!(store.env().info().last_txn_id, generation + 1);
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (_, mut restarted) = signer();
    let mut conflict = first.clone();
    conflict.trst_balance += 1;
    conflict.hash = conflict.compute_hash();
    store
        .meta_store()
        .put_fork_candidate(&conflict.hash, &bincode::serialize(&conflict).unwrap())
        .unwrap();
    let result = generate_final_votes(&store, &mut restarted, &[conflict.hash, second.hash]);
    assert!(result[0].is_err());
    assert_eq!(result[1].as_ref().unwrap().sequence, 3);
}

#[test]
fn group_failure_leaves_signer_untouched_and_fallback_isolates_targets() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let first = independent(&store, 1, true);
    let second = independent(&store, 2, true);
    let pending = independent(&store, 3, false);
    let (_, mut generator) = signer();
    let mut conflict = first.clone();
    conflict.trst_balance += 1;
    conflict.hash = conflict.compute_hash();
    store
        .meta_store()
        .put_fork_candidate(&conflict.hash, &bincode::serialize(&conflict).unwrap())
        .unwrap();
    let generation = store.env().info().last_txn_id;
    assert!(fresh_group(&store, &mut generator, &[first.hash, conflict.hash]).is_err());
    assert_eq!(generator.sequence(), 0);
    assert!(generator.cached_final_response(&first.hash).is_none());
    assert_eq!(store.env().info().last_txn_id, generation);
    let result = generate_final_votes(
        &store,
        &mut generator,
        &[pending.hash, first.hash, conflict.hash, second.hash],
    );
    assert!(
        matches!(&result[0], Err(VoteGenerationError::DependenciesPending(hashes)) if hashes == &vec![pending.previous])
    );
    assert_eq!(result[1].as_ref().unwrap().sequence, 1);
    assert!(result[2].is_err());
    assert_eq!(result[3].as_ref().unwrap().sequence, 2);
    // Cached groups still recheck newly installed authority limits.
    store
        .meta_store()
        .put_meta(
            burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
            &burst_store::meta::encode_payment_vote_limit(first.payment_epoch, Some(99)),
        )
        .unwrap();
    assert!(
        generate_final_votes(&store, &mut generator, &[first.hash, second.hash])
            .iter()
            .all(Result::is_err)
    );
    assert_eq!(generator.sequence(), 2);
    let too_many = vec![first.hash; LmdbMetaStore::MAX_CONSENSUS_RESERVATIONS + 1];
    assert!(generate_final_votes(&store, &mut generator, &too_many)
        .iter()
        .all(Result::is_err));
    assert_eq!(generator.sequence(), 2);
}

#[test]
fn pending_dependencies_preserve_ready_batch_and_result_order() {
    for deferred_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let first = independent(&store, 1, true);
        let second = independent(&store, 2, true);
        let pending = independent(&store, 3, false);
        let (key, mut generator) = signer();
        let hashes = if deferred_first {
            [pending.hash, first.hash, second.hash]
        } else {
            [first.hash, pending.hash, second.hash]
        };
        let generation = store.env().info().last_txn_id;
        let results = generate_final_votes(&store, &mut generator, &hashes);
        assert_eq!(results.len(), hashes.len());
        assert_eq!(
            store.env().info().last_txn_id,
            generation + 1,
            "a deferred target must not split ready votes into singleton commits"
        );
        assert_eq!(generator.sequence(), 2);
        let mut next_sequence = 1;
        for (hash, result) in hashes.into_iter().zip(results) {
            if hash == pending.hash {
                assert!(
                    matches!(result, Err(VoteGenerationError::DependenciesPending(ref dependencies))
                    if dependencies == &vec![pending.previous])
                );
            } else {
                let vote = result.unwrap();
                assert_eq!(vote.block_hash, hash);
                assert_eq!(vote.sequence, next_sequence);
                next_sequence += 1;
                assert!(vote.is_final);
                assert!(burst_crypto::verify_signature(
                    &burst_consensus::vote_signing_payload(
                        &vote.voter,
                        true,
                        vote.timestamp,
                        vote.sequence,
                        &[hash]
                    ),
                    &vote.signature,
                    &key.public
                ));
            }
        }
        assert!(generator.cached_final_response(&pending.hash).is_none());
        let generation = store.env().info().last_txn_id;
        assert!(matches!(
            &generate_final_votes(&store, &mut generator, &[pending.hash])[0],
            Err(VoteGenerationError::DependenciesPending(_))
        ));
        assert_eq!(store.env().info().last_txn_id, generation);
        assert_eq!(generator.sequence(), 2);
        // Confirming the predecessor makes the deferred slot signable.
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .advance_confirmation_height(&pending.account, 1)
            .unwrap();
        batch.commit().unwrap();
        let vote = generate_final_votes(&store, &mut generator, &[pending.hash])
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(vote.sequence, 3);
        assert_eq!(vote.block_hash, pending.hash);
    }
}

#[tokio::test]
async fn blocking_vote_entry_points_preserve_signatures_sequences_and_current_fences() {
    use crate::durable_votes::worker;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
    let first = independent(&store, 1, true);
    let second = independent(&store, 2, true);
    let (key, generator) = signer();
    let generator = Arc::new(tokio::sync::Mutex::new(generator));
    let vote = worker::generate(
        store.clone(),
        generator.clone().lock_owned().await,
        first.hash,
        false,
    )
    .await
    .unwrap();
    assert_eq!(vote.sequence, 1);
    assert!(!vote.is_final);
    let votes = worker::finals(
        store.clone(),
        generator.clone().lock_owned().await,
        vec![first.hash, second.hash],
    )
    .await
    .unwrap();
    for (index, vote) in votes.into_iter().enumerate() {
        let vote = vote.unwrap();
        assert_eq!(vote.sequence, index as u64 + 2);
        assert!(vote.is_final);
        assert!(burst_crypto::verify_signature(
            &burst_consensus::vote_signing_payload(
                &vote.voter,
                true,
                vote.timestamp,
                vote.sequence,
                &[vote.block_hash]
            ),
            &vote.signature,
            &key.public
        ));
    }
    // A cached fork remains ineligible for an ordinary peer response.
    assert!(worker::respond(
        store.clone(),
        generator.clone().lock_owned().await,
        first.hash
    )
    .await
    .unwrap()
    .is_none());
    store
        .meta_store()
        .put_meta(
            burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
            &burst_store::meta::encode_payment_vote_limit(first.payment_epoch, Some(99)),
        )
        .unwrap();
    assert!(worker::finals(
        store.clone(),
        generator.clone().lock_owned().await,
        vec![first.hash, second.hash]
    )
    .await
    .unwrap()
    .iter()
    .all(Result::is_err));
    assert_eq!(generator.lock().await.sequence(), 3);
}

#[test]
fn payment_readiness_uses_committed_snapshot_without_waiting_for_writer() {
    use crate::verification_commit::dependencies::view::{DependencyView, Snapshot};
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let block = independent(&store, 1, false);
    let snapshot = Snapshot::new(&store).unwrap();
    let mut writer = store.tx_begin_write().unwrap();
    writer
        .advance_confirmation_height(&block.account, 1)
        .unwrap();
    // The writer sees its own change, but a reader must neither wait for it nor
    // treat the uncommitted parent as confirmed.
    assert!(check_final_dependencies(&writer, &block).is_ok());
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let store_ref = &store;
        let block_ref = &block;
        scope.spawn(move || {
            let _ = tx.send(final_vote_limit(store_ref, block_ref));
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        writer.commit().unwrap(); // Release even on failure so the thread can join.
        assert!(
            matches!(result.unwrap(), Err(VoteGenerationError::DependenciesPending(hashes))
            if hashes == vec![block.previous])
        );
    });
    assert!(matches!(
        check_final_dependencies(&snapshot, &block),
        Err(VoteGenerationError::DependenciesPending(_))
    ));
    assert!(snapshot
        .block_height(&WalletAddress::new("brst_wrong_account"), &block.previous)
        .is_err());
    drop(snapshot);
    assert!(final_vote_limit(&store, &block).unwrap().is_none());
}

fn canonical_soft(store: &LmdbStore, id: u8) -> StateBlock {
    use burst_store::account::AccountStore;
    let block = independent(store, id, true);
    let mut account = store.account_store().get_account(&block.account).unwrap();
    account.head = block.hash;
    account.block_count = 2;
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
    block
}

#[test]
fn grouped_soft_responses_share_durable_cut_and_cached_retries_preserve_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let first = canonical_soft(&store, 1);
    let second = canonical_soft(&store, 2);
    let (key, mut generator) = signer();
    generator.is_representative = true;
    let generation = store.env().info().last_txn_id;
    let votes: Vec<_> = respond_votes(&store, &mut generator, &[first.hash, second.hash])
        .into_iter()
        .map(|v| v.unwrap().unwrap())
        .collect();
    assert_eq!(store.env().info().last_txn_id, generation + 1);
    assert_eq!(generator.sequence(), 2);
    for (i, vote) in votes.iter().enumerate() {
        assert!(!vote.is_final);
        assert_eq!(vote.sequence, i as u64 + 1);
        assert!(burst_crypto::verify_signature(
            &burst_consensus::vote_signing_payload(
                &vote.voter,
                false,
                vote.timestamp,
                vote.sequence,
                &[vote.block_hash]
            ),
            &vote.signature,
            &key.public
        ));
    }
    let again = respond_votes(
        &store,
        &mut generator,
        &[second.hash, first.hash, first.hash],
    );
    for (result, expected) in again.iter().zip([&votes[1], &votes[0], &votes[0]]) {
        assert_eq!(
            result.as_ref().unwrap().as_ref().unwrap().signature,
            expected.signature
        );
    }
    assert_eq!(store.env().info().last_txn_id, generation + 1);
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (_, mut restarted) = signer();
    restarted.is_representative = true;
    let next = respond_votes(&store, &mut restarted, &[first.hash, second.hash]);
    assert_eq!(next[0].as_ref().unwrap().as_ref().unwrap().sequence, 3);
    assert_eq!(next[1].as_ref().unwrap().as_ref().unwrap().sequence, 4);
}

#[test]
fn soft_group_failure_reserves_nothing_and_scalar_fallback_isolates_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let first = canonical_soft(&store, 1);
    let second = canonical_soft(&store, 2);
    let mut conflict = second.clone();
    conflict.trst_balance += 1;
    conflict.hash = conflict.compute_hash();
    store
        .meta_store()
        .put_fork_candidate(&conflict.hash, &bincode::serialize(&conflict).unwrap())
        .unwrap();
    let (_, mut generator) = signer();
    generator.is_representative = true;
    generate_vote(&store, &mut generator, conflict.hash, true).unwrap();
    let generation = store.env().info().last_txn_id;
    let sequence = generator.sequence();
    assert!(fresh_soft_group(&store, &mut generator, &[first.hash, second.hash]).is_err());
    assert_eq!(store.env().info().last_txn_id, generation);
    assert_eq!(generator.sequence(), sequence);
    assert!(generator
        .cached_soft_response(&first.account, first.previous, first.hash)
        .is_none());
    let result = respond_votes(&store, &mut generator, &[first.hash, second.hash]);
    assert!(result[0].as_ref().unwrap().is_some());
    assert!(result[1].is_err());
    assert_eq!(generator.sequence(), sequence + 1);
}

#[test]
fn soft_response_group_preserves_final_kind_missing_candidates_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let first = canonical_soft(&store, 1);
    let second = canonical_soft(&store, 2);
    let (_, mut generator) = signer();
    generator.is_representative = true;
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .advance_confirmation_height(&second.account, 2)
        .unwrap();
    batch.commit().unwrap();
    let result = respond_votes(
        &store,
        &mut generator,
        &[first.hash, second.hash, BlockHash::ZERO],
    );
    assert!(!result[0].as_ref().unwrap().as_ref().unwrap().is_final);
    assert!(result[1].as_ref().unwrap().as_ref().unwrap().is_final);
    assert!(result[2].as_ref().unwrap().is_none());
    let sequence = generator.sequence();
    assert!(respond_votes(
        &store,
        &mut generator,
        &vec![first.hash; LmdbMetaStore::MAX_CONSENSUS_RESERVATIONS + 1]
    )
    .iter()
    .all(Result::is_err));
    assert_eq!(generator.sequence(), sequence);
}
