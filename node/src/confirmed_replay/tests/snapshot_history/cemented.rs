use super::*;
use crate::confirmed_replay::snapshot::{tail::cemented::CementedTail, SnapshotContinuation};
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};

pub(in crate::confirmed_replay::tests) fn sparse(
    base: &SnapshotContinuation,
    source: &LmdbStore,
) -> (tempfile::TempDir, LmdbStore) {
    let (dir, store) = suffix_store(&[]);
    let mut batch = store.tx_begin_write().unwrap();
    for account in base.state().accounts.values() {
        batch.put_account_info(account, false).unwrap();
        if account.block_count > 0 {
            batch.put_frontier(&account.address, &account.head).unwrap();
            batch
                .put_block_with_account(
                    &account.head,
                    &source.block_store().get_block(&account.head).unwrap(),
                    &account.address,
                    account.block_count,
                )
                .unwrap();
        }
    }
    base.state()
        .persist_projection(&mut batch, base.anchor())
        .unwrap();
    batch
        .put_meta(
            burst_store::meta::PAYMENT_EPOCH_KEY,
            base.anchor().hash().as_bytes(),
        )
        .unwrap();
    batch.commit().unwrap();
    assert!(
        store.block_store().block_count().unwrap() < source.block_store().block_count().unwrap()
    );
    (dir, store)
}

pub(super) fn append(store: &LmdbStore, block: &StateBlock, confirmed: bool) {
    append_batch(store, std::slice::from_ref(block), confirmed);
}

pub(super) fn append_batch(store: &LmdbStore, blocks: &[StateBlock], confirmed: bool) {
    let mut batch = store.tx_begin_write().unwrap();
    let mut schedule = confirmed.then(|| {
        crate::confirmed_governance::ConfirmedGovernance::load(&batch)
            .unwrap()
            .unwrap()
    });
    for block in blocks {
        let mut account = match batch.get_account(&block.account) {
            Ok(account) => account,
            Err(burst_store::StoreError::NotFound(_)) => {
                assert_eq!(block.block_type, BlockType::Open);
                assert!(block.previous.is_zero());
                burst_store::account::AccountInfo::unopened(block.account.clone())
            }
            Err(error) => panic!("cannot stage confirmed account: {error}"),
        };
        assert_eq!(account.head, block.previous);
        account.head = block.hash;
        account.block_count += 1;
        if let Some(schedule) = &mut schedule {
            account.confirmation_height = account.block_count;
            schedule.promote(&batch, block).unwrap();
        }
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                account.block_count,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
    }
    if let Some(schedule) = schedule {
        schedule.persist(&mut batch).unwrap();
    }
    batch.commit().unwrap();
}

pub(super) fn finish(
    base: &SnapshotContinuation,
    store: &LmdbStore,
    source: &LmdbStore,
) -> CementedTail {
    let plan = base.plan_cemented_tail(store).unwrap();
    let needed = plan.witness_plan().witness_hashes().to_vec();
    let proofs = if needed.is_empty() {
        Vec::new()
    } else {
        witness_transfer::exchange(
            crate::checkpoint_sync::witness::Roots::for_plan(base, plan.witness_plan()).unwrap(),
            needed,
            source,
            store,
        )
        .unwrap()
    };
    plan.finish(proofs).unwrap()
}

#[test]
fn compact_confirmed_cut_excludes_future_dependencies_without_the_pruned_prefix() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = sparse(&base, &source);
    let at = base.anchor().cutoff() + 1;
    let send = fixture.push(0, BlockType::Send, at + 2, |block| {
        block.payment_epoch = base.anchor().hash();
        block.link = destination(1);
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    let receive = fixture.push(1, BlockType::Receive, at, |block| {
        block.payment_epoch = base.anchor().hash();
        block.link = send;
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    fixture.push(1, BlockType::ChangeRepresentative, at + 1, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    fixture.push(2, BlockType::ChangeRepresentative, at, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    fixture.push(2, BlockType::ChangeRepresentative, at + 1, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    append(&local, fixture.blocks.last().unwrap(), false);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let tail = finish(&base, &local, &source);
    assert!(ConfirmedHistory::heads_at(&local, at).is_err());
    assert!(base
        .cemented_heads_at(&tail, base.anchor().cutoff() - 1)
        .is_err());
    let early = base.cemented_heads_at(&tail, at + 1).unwrap();
    for seed in [0, 1] {
        let head = early
            .iter()
            .find(|head| head.account == address(seed))
            .unwrap();
        assert_eq!(
            head.hash,
            base.state().account(&address(seed)).unwrap().head
        );
    }
    let independent = early
        .iter()
        .find(|head| head.account == address(2))
        .unwrap();
    assert_eq!(
        independent.height,
        base.state().account(&address(2)).unwrap().block_count + 1
    );
    let late = base.cemented_heads_at(&tail, at + 2).unwrap();
    assert_eq!(late, tail.tail().heads());
    assert!(late.iter().any(|head| head.hash == send));
    assert!(!early.iter().any(|head| head.hash == receive));
    // Fixture::store confirms every supplied block. Leave out the provisional
    // tip instead of trying to lower the store's monotonic finality boundary.
    fixture.blocks.pop().unwrap();
    let (_full_dir, full, _) = fixture.store();
    for cutoff in [base.anchor().cutoff(), at, at + 1, at + 2] {
        assert_eq!(
            base.cemented_heads_at(&tail, cutoff).unwrap(),
            ConfirmedHistory::heads_at(&full, cutoff).unwrap()
        );
    }
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[test]
fn compact_startup_checks_cemented_governance_without_reading_the_pruned_prefix() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (dir, local) = sparse(&base, &source);
    assert!(ConfirmedHistory::heads_at(&local, u64::MAX).is_err());
    let at = base.anchor().cutoff() + 1;
    let proposal = fixture.push(0, BlockType::GovernanceProposal, at, |block| {
        block.payment_epoch = base.anchor().hash();
        block.origin = TxHash::ZERO;
        block.link = BlockHash::new(
            burst_governance::ProposalContent::ParameterChange {
                param: burst_governance::GovernableParam::BrnRate,
                new_value: 2,
            }
            .encode_compact()
            .unwrap(),
        );
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    let endorsement = fixture.push(1, BlockType::GovernanceEndorse, at + 1, |block| {
        block.payment_epoch = base.anchor().hash();
        block.link = proposal;
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    let withdrawal = fixture.push(0, BlockType::GovernanceWithdraw, at + 2, |block| {
        block.payment_epoch = base.anchor().hash();
        block.link = proposal;
    });
    append(&local, fixture.blocks.last().unwrap(), false);
    local.force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let tail = finish(&base, &local, &source);
    let hashes: Vec<_> = tail.tail().block_hashes().collect();
    assert_eq!(hashes, vec![proposal, endorsement]);
    assert!(!hashes.contains(&withdrawal));
    base.verify_installed_metadata(&local, &tail).unwrap();
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let limit = burst_store::meta::decode_payment_vote_limit(
        &local
            .meta_store()
            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
            .unwrap(),
        base.anchor().hash(),
    )
    .unwrap();
    assert!(limit.is_some());
}

#[test]
fn compact_startup_rejects_corrupt_boundaries_and_metadata_without_repairs() {
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    for case in 0..11 {
        let (_dir, local) = sparse(&base, &source);
        let tail = finish(&base, &local, &source);
        let account = local.account_store().get_account(&address(0)).unwrap();
        match case {
            0 => local
                .meta_store()
                .put_meta(crate::confirmed_replay::installation::INSTALLED_KEY, b"bad")
                .unwrap(),
            1 => local
                .meta_store()
                .delete_meta(crate::confirmed_replay::installation::CUTOFF_KEY)
                .unwrap(),
            2 => local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY, b"bad")
                .unwrap(),
            3 => local
                .meta_store()
                .put_meta(crate::confirmed_governance::KEY, b"bad")
                .unwrap(),
            4 => local
                .meta_store()
                .put_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY, b"bad")
                .unwrap(),
            5 => local
                .meta_store()
                .put_meta(
                    &burst_store::meta::closed_payment_epoch_key(&base.anchor().hash()),
                    b"closed",
                )
                .unwrap(),
            6 => {
                let mut changed = account.clone();
                changed.confirmation_height -= 1;
                let mut batch = local.tx_begin_write().unwrap();
                batch
                    .put_account(&changed.address, &bincode::serialize(&changed).unwrap())
                    .unwrap();
                batch.commit().unwrap();
                assert!(base.plan_cemented_tail(&local).is_err());
                assert!(base.plan_installed_tails(&local).is_err());
            }
            7 => {
                local.block_store().delete_block(&account.head).unwrap();
                assert!(base.plan_cemented_tail(&local).is_err());
                assert!(base.plan_installed_tails(&local).is_err());
            }
            8 => {
                let mut body = source.block_store().get_block(&account.head).unwrap();
                body[0] ^= 1;
                local.block_store().put_block(&account.head, &body).unwrap();
                assert!(base.plan_cemented_tail(&local).is_err());
                assert!(base.plan_installed_tails(&local).is_err());
            }
            9 => {
                let bytes = source.block_store().get_block(&account.head).unwrap();
                let mut block: StateBlock = bincode::deserialize(&bytes).unwrap();
                block.work ^= 1;
                assert_eq!(block.compute_hash(), account.head);
                let changed = bincode::serialize(&block).unwrap();
                assert_eq!(changed.len(), bytes.len());
                local
                    .block_store()
                    .put_block(&account.head, &changed)
                    .unwrap();
                // Historical membership deliberately excludes the work nonce.
                // A fresh read is valid; the older read set must still change.
                let fresh = finish(&base, &local, &source);
                base.verify_installed_metadata(&local, &fresh).unwrap();
            }
            _ => {
                let env = local.env();
                let mut txn = env.write_txn().unwrap();
                let accounts = env
                    .open_database::<heed::types::Bytes, heed::types::Bytes>(&txn, Some("accounts"))
                    .unwrap()
                    .unwrap();
                accounts
                    .delete(&mut txn, account.address.as_str().as_bytes())
                    .unwrap();
                let replacement = burst_store::account::AccountInfo::unopened(address(99));
                accounts
                    .put(
                        &mut txn,
                        replacement.address.as_str().as_bytes(),
                        &bincode::serialize(&replacement).unwrap(),
                    )
                    .unwrap();
                txn.commit().unwrap();
                // Equal inventory length does not preserve the required base identities.
                let error = base
                    .plan_cemented_tail(&local)
                    .err()
                    .expect("missing base identity");
                assert!(error
                    .to_string()
                    .contains("local ledger omits a snapshot account"));
            }
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            base.verify_installed_metadata(&local, &tail).is_err(),
            "case {case}"
        );
        assert!(base.verify_discovery_metadata_in(&local.tx_begin_write().unwrap(), &tail).is_err(),
            "discovery corruption case {case}");
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn compact_startup_does_not_treat_newer_accounts_as_the_same_checked_cut() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = sparse(&base, &source);
    let checked = finish(&base, &local, &source);
    base.verify_installed_metadata(&local, &checked).unwrap();
    fixture.push(
        0,
        BlockType::ChangeRepresentative,
        base.anchor().cutoff() + 1,
        |block| {
            block.payment_epoch = base.anchor().hash();
        },
    );
    append(&local, fixture.blocks.last().unwrap(), true);
    assert!(base.verify_installed_metadata(&local, &checked).is_err());
    base.verify_installed_metadata(&local, &finish(&base, &local, &source))
        .unwrap();
}

#[tokio::test]
async fn paired_tail_planning_preserves_distinct_cuts_and_rejects_changed_history() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = sparse(&base, &source);
    let at = base.anchor().cutoff() + 1;
    fixture.push(0, BlockType::ChangeRepresentative, at, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    append(&local, fixture.blocks.last().unwrap(), true);
    let provisional = fixture.push(0, BlockType::ChangeRepresentative, at + 1, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    append(&local, fixture.blocks.last().unwrap(), false);

    let (confirmed, applied, authenticated) = base.plan_installed_tails(&local).unwrap();
    let separate_confirmed = base.plan_cemented_tail(&local).unwrap();
    let separate_applied = base.plan_applied_tail(&local).unwrap();
    for (paired, separate) in [
        (confirmed.witness_plan(), separate_confirmed.witness_plan()),
        (applied.witness_plan(), separate_applied.witness_plan()),
    ] {
        assert_eq!(paired.heads(), separate.heads());
        assert_eq!(paired.witness_hashes(), separate.witness_hashes());
    }
    assert!(!confirmed
        .witness_plan()
        .heads()
        .iter()
        .any(|head| head.hash == provisional));
    assert!(applied
        .witness_plan()
        .heads()
        .iter()
        .any(|head| head.hash == provisional));
    let confirmed_proofs =
        crate::checkpoint_sync::witness::read_plan(&source, &base, confirmed.witness_plan())
            .unwrap();
    let applied_proofs =
        crate::checkpoint_sync::witness::read_plan(&source, &base, applied.witness_plan()).unwrap();
    let authenticated_hashes: std::collections::HashSet<_> = authenticated
        .iter()
        .map(|proof| proof.block().hash)
        .collect();
    assert_eq!(
        authenticated.len(),
        base.state()
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
            .count()
    );
    assert!(authenticated
        .iter()
        .all(|proof| proof.context() == base.history_context()));
    let required = confirmed.witness_plan().witness_hashes();
    let missing = *required
        .iter()
        .find(|hash| authenticated_hashes.contains(hash))
        .expect("snapshot head required by tail");
    let older = confirmed_proofs
        .iter()
        .filter(|proof| !authenticated_hashes.contains(&proof.block().hash))
        .cloned();
    let mut available = authenticated;
    available.extend(older);
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    // No service is registered. Complete retained evidence must satisfy this
    // request directly; removing a needed head must still require a fetch.
    let reused = crate::checkpoint_sync::witness::fetch_plan_reusing(
        &registry,
        &base,
        confirmed.witness_plan(),
        available.clone(),
    )
    .await
    .unwrap();
    assert_eq!(reused.len(), required.len());
    available.retain(|proof| proof.block().hash != missing);
    assert!(crate::checkpoint_sync::witness::fetch_plan_reusing(
        &registry,
        &base,
        confirmed.witness_plan(),
        available,
    )
    .await
    .is_err());
    assert_eq!(
        confirmed.witness_plan().witness_hashes(),
        applied.witness_plan().witness_hashes()
    );
    let staged = std::sync::Arc::new(
        crate::confirmed_replay::snapshot::history::HistoricalBlocks::stage(
            local.env().path(),
            reused,
        )
        .unwrap(),
    );
    let confirmed = confirmed.finish_staged(staged.clone()).unwrap();
    let applied = applied.finish_staged(staged.clone()).unwrap();
    assert_eq!(std::sync::Arc::strong_count(&staged), 3);
    for tail in [confirmed.tail(), applied.tail()] {
        let input = crate::confirmed_replay::blocks::source::Source::from_tail(tail);
        assert_eq!(std::sync::Arc::strong_count(&staged), 4);
        let expected: Vec<_> = tail
            .retained()
            .iter()
            .map(|proof| proof.map(|proof| proof.into_block()))
            .chain(tail.blocks())
            .map(|block| bincode::serialize(&block.unwrap()).unwrap())
            .collect();
        let actual: Vec<_> = input
            .iter()
            .map(|block| bincode::serialize(block.unwrap().as_ref()).unwrap())
            .collect();
        assert_eq!(actual, expected);
        let mut scanned = Vec::new();
        input
            .visit_unordered(|block| {
                scanned.push(bincode::serialize(block).unwrap());
                Ok::<_, burst_store::StoreError>(())
            })
            .unwrap();
        let mut expected_scan = expected.clone();
        expected_scan.sort();
        scanned.sort();
        assert_eq!(scanned, expected_scan);
        assert_eq!(input.directory(), Some(local.env().path()));
        assert!(input.get(&BlockHash::ZERO).is_err());
    }
    assert_eq!(std::sync::Arc::strong_count(&staged), 3);
    drop(staged);
    assert_eq!(
        confirmed.tail().commitment(),
        separate_confirmed
            .finish(confirmed_proofs)
            .unwrap()
            .tail()
            .commitment()
    );
    assert_eq!(
        applied.tail().commitment(),
        separate_applied
            .finish(applied_proofs)
            .unwrap()
            .tail()
            .commitment()
    );
    {
        let batch = local.tx_begin_write().unwrap();
        confirmed.verify_unchanged(&batch).unwrap();
        applied.verify_unchanged(&batch).unwrap();
    }

    let hash = base.state().account(&address(0)).unwrap().head;
    let bytes = local.block_store().get_block(&hash).unwrap();
    let mut changed: StateBlock = bincode::deserialize(&bytes).unwrap();
    changed.work ^= 1;
    assert_eq!(changed.compute_hash(), hash);
    local
        .block_store()
        .put_block(&hash, &bincode::serialize(&changed).unwrap())
        .unwrap();
    {
        let batch = local.tx_begin_write().unwrap();
        assert!(confirmed.verify_unchanged(&batch).is_err());
        assert!(applied.verify_unchanged(&batch).is_err());
    }
    // Historical work is not signed, but its bytes remain in both read sets.
    base.plan_installed_tails(&local).unwrap();
    changed.signature.0[0] ^= 1;
    local
        .block_store()
        .put_block(&hash, &bincode::serialize(&changed).unwrap())
        .unwrap();
    assert!(base.plan_installed_tails(&local).is_err());
    local.block_store().put_block(&hash, &bytes).unwrap();
    let mut unsigned = fixture.blocks.last().unwrap().clone();
    unsigned.signature.0[0] ^= 1;
    let captured = local.tx_begin_read().unwrap();
    let (captured_confirmed, captured_applied, _) =
        base.plan_installed_tails_view(&local, &captured).unwrap();
    local
        .block_store()
        .put_block(&provisional, &bincode::serialize(&unsigned).unwrap())
        .unwrap();
    let (unchanged_confirmed, unchanged_applied, _) =
        base.plan_installed_tails_view(&local, &captured).unwrap();
    assert_eq!(
        captured_confirmed.witness_plan().heads(),
        unchanged_confirmed.witness_plan().heads()
    );
    assert_eq!(
        captured_applied.witness_plan().heads(),
        unchanged_applied.witness_plan().heads()
    );
    let view = local.read_view(&captured).unwrap();
    confirmed.verify_view(&view).unwrap();
    applied.verify_view(&view).unwrap();
    for at in ["metadata:tail-account", "metadata:tail-body"] {
        let reached = std::cell::Cell::new(false);
        assert!(applied
            .verify_view_with_poll(&view, &|stage| {
                if stage == at {
                    reached.set(true);
                    return Err(burst_store::StoreError::Backend(
                        "tail checkpoint sentinel".into(),
                    ));
                }
                Ok(())
            })
            .unwrap_err()
            .to_string()
            .contains("tail checkpoint sentinel"));
        assert!(reached.get());
        applied.verify_view(&view).unwrap();
    }
    {
        let batch = local.tx_begin_write().unwrap();
        assert!(applied.verify_unchanged(&batch).is_err());
    }
    // LMDB permits only one TLS read transaction per environment on this thread.
    drop(captured);
    assert!(base.plan_installed_tails(&local).is_err());
}

#[test]
fn discovery_tolerates_provisional_append_but_rechecks_confirmed_cut_and_indices() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = sparse(&base, &source);
    let checked = finish(&base, &local, &source);
    fixture.push(0, BlockType::ChangeRepresentative, base.anchor().cutoff() + 1, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    let block = fixture.blocks.last().unwrap();
    append(&local, block, false);
    assert!(base.verify_installed_metadata(&local, &checked).is_err());
    base.verify_discovery_metadata_in(&local.tx_begin_write().unwrap(), &checked).unwrap();

    let mut account = local.account_store().get_account(&address(0)).unwrap();
    let valid = account.clone();
    account.head = BlockHash::new([91; 32]);
    let mut batch = local.tx_begin_write().unwrap();
    batch.put_account(&account.address, &bincode::serialize(&account).unwrap()).unwrap();
    batch.commit().unwrap();
    assert!(base.verify_discovery_metadata_in(&local.tx_begin_write().unwrap(), &checked).is_err());

    account = valid;
    account.confirmation_height = account.block_count;
    let mut batch = local.tx_begin_write().unwrap();
    batch.put_account(&account.address, &bincode::serialize(&account).unwrap()).unwrap();
    batch.commit().unwrap();
    assert!(base.verify_discovery_metadata_in(&local.tx_begin_write().unwrap(), &checked).is_err());
    let fresh = finish(&base, &local, &source);
    base.verify_discovery_metadata_in(&local.tx_begin_write().unwrap(), &fresh).unwrap();
}
