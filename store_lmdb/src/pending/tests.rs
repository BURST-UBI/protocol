use super::*;
use crate::LmdbEnvironment;
use burst_types::Timestamp;

fn info(source: &WalletAddress, amount: u128) -> PendingInfo {
    PendingInfo {
        return_credit: false,
        source: source.clone(),
        amount,
        timestamp: Timestamp::new(10),
        provenance: Vec::new(),
    }
}

#[test]
fn return_credit_is_claimable_but_is_not_outgoing_value() {
    let dir = tempfile::tempdir().unwrap();
    let owner = WalletAddress::new("brst_owner");
    let hash = TxHash::new([9; 32]);
    for reopening in [false, true] {
        let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let store = env.pending_store();
        if !reopening {
            let mut receipt = info(&owner, 40);
            receipt.return_credit = true;
            store.put_pending(&owner, &hash, &receipt).unwrap();
        }
        assert!(store.get_pending_from_account(&owner).unwrap().is_empty());
        let claims = store.get_pending_for_account_with_hashes(&owner).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].0, hash);
        assert_eq!(claims[0].1.amount, 40);
        assert!(claims[0].1.return_credit);
    }
}

#[test]
fn sender_index_tracks_overwrites_deletions_and_aborted_batches() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let store = env.pending_store();
    let alice = WalletAddress::new("brst_alice");
    let bob = WalletAddress::new("brst_bob");
    let alice_suffix = WalletAddress::new("brst_alice_suffix");
    let hash = TxHash::new([1; 32]);
    let other = TxHash::new([2; 32]);
    store.put_pending(&bob, &hash, &info(&alice, 10)).unwrap();
    store
        .put_pending(&bob, &other, &info(&alice_suffix, 20))
        .unwrap();
    let outgoing = store.get_pending_from_account(&alice).unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        (&outgoing[0].0, outgoing[0].1, outgoing[0].2.amount),
        (&bob, hash, 10)
    );
    assert!(store.get_pending_from_account(&bob).unwrap().is_empty());
    {
        let mut batch = env.tx_begin_write().unwrap();
        batch.delete_pending(&bob, hash.as_bytes()).unwrap();
        batch
            .put_pending(
                &alice,
                hash.as_bytes(),
                &bincode::serialize(&info(&bob, 50)).unwrap(),
            )
            .unwrap();
    }
    assert_eq!(
        store.get_pending_from_account(&alice).unwrap()[0].2.amount,
        10
    );
    assert!(store.get_pending_from_account(&bob).unwrap().is_empty());
    store.put_pending(&bob, &hash, &info(&bob, 30)).unwrap();
    assert!(store.get_pending_from_account(&alice).unwrap().is_empty());
    assert_eq!(
        store.get_pending_from_account(&bob).unwrap()[0].2.amount,
        30
    );
    store.delete_pending(&bob, &hash).unwrap();
    store.delete_pending(&bob, &hash).unwrap();
    assert!(store.get_pending_from_account(&bob).unwrap().is_empty());
    assert_eq!(
        store.get_pending_from_account(&alice_suffix).unwrap().len(),
        1
    );
}

#[test]
fn existing_pending_rows_are_indexed_once_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let alice = WalletAddress::new("brst_alice");
    let bob = WalletAddress::new("brst_bob");
    let hash = TxHash::new([3; 32]);
    {
        let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let mut txn = env.env().write_txn().unwrap();
        env.pending_db
            .put(
                &mut txn,
                &pending_key(&bob, &hash),
                &bincode::serialize(&info(&alice, 40)).unwrap(),
            )
            .unwrap();
        for number in 0..2048u64 {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&number.to_be_bytes());
            env.pending_db
                .put(
                    &mut txn,
                    &pending_key(&bob, &TxHash::new(bytes)),
                    &bincode::serialize(&info(&alice, 1)).unwrap(),
                )
                .unwrap();
        }
        env.meta_db
            .delete(&mut txn, b"pending_sender_index_version")
            .unwrap();
        txn.commit().unwrap();
    }
    for _ in 0..2 {
        let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let outgoing = env
            .pending_store()
            .get_pending_from_account(&alice)
            .unwrap();
        assert_eq!(outgoing.len(), 2049);
        let original = outgoing
            .iter()
            .find(|(_, source, _)| *source == hash)
            .unwrap();
        assert_eq!(
            (&original.0, original.1, original.2.amount),
            (&bob, hash, 40)
        );
    }
}

#[test]
fn stale_or_mismatched_sender_index_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let alice = WalletAddress::new("brst_alice");
    let bob = WalletAddress::new("brst_bob");
    let hash = TxHash::new([4; 32]);
    let key = pending_key(&bob, &hash);
    env.pending_store()
        .put_pending(&bob, &hash, &info(&alice, 10))
        .unwrap();
    for replacement in [Some(bincode::serialize(&info(&bob, 10)).unwrap()), None] {
        let mut txn = env.env().write_txn().unwrap();
        if let Some(value) = replacement {
            env.pending_db.put(&mut txn, &key, &value).unwrap();
        } else {
            env.pending_db.delete(&mut txn, &key).unwrap();
        }
        txn.commit().unwrap();
        assert!(matches!(
            env.pending_store().get_pending_from_account(&alice),
            Err(StoreError::Corruption(_))
        ));
    }
}

#[test]
fn malformed_pending_write_aborts_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let alice = WalletAddress::new("brst_alice");
    let bob = WalletAddress::new("brst_bob");
    let hash = TxHash::new([5; 32]);
    let mut batch = env.tx_begin_write().unwrap();
    batch
        .put_pending(
            &bob,
            hash.as_bytes(),
            &bincode::serialize(&info(&alice, 10)).unwrap(),
        )
        .unwrap();
    assert!(batch
        .put_pending(&bob, hash.as_bytes(), b"invalid")
        .is_err());
    assert!(batch.commit().is_err());
    assert_eq!(env.pending_store().pending_count().unwrap(), 0);
    assert!(env
        .pending_store()
        .get_pending_from_account(&alice)
        .unwrap()
        .is_empty());
}

#[test]
fn amount_pages_match_legacy_sort_and_bound_payload_reads() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 32 << 20).unwrap();
    let store = env.pending_store();
    let owner = WalletAddress::new("brst_page_owner");
    let other = WalletAddress::new("brst_page_owner_suffix");
    let mut batch = env.tx_begin_write().unwrap();
    for n in 0..4096u64 {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&n.to_be_bytes());
        batch
            .put_pending(
                &owner,
                &hash,
                &bincode::serialize(&info(&owner, (n % 17) as u128)).unwrap(),
            )
            .unwrap();
    }
    batch.commit().unwrap();
    let mut old = store.get_pending_for_account_with_hashes(&owner).unwrap();
    old.sort_by_key(|(_, p)| std::cmp::Reverse(p.amount));
    // The synthetic overlapping address tests index isolation, independently
    // of the legacy scan's fixed-length address precondition.
    store
        .put_pending(&other, &TxHash::new([1; 32]), &info(&owner, u128::MAX))
        .unwrap();
    for threshold in [0, 1, 8, 16, 17, u128::MAX] {
        let filtered: Vec<_> = old.iter().filter(|(_, p)| p.amount >= threshold).collect();
        for offset in [0, 1, 100, 4095, 4096, u64::MAX] {
            let expected: Vec<_> = filtered
                .iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(7)
                .map(|(h, p)| (*h, p.amount))
                .collect();
            let page = store
                .get_pending_page(&owner, threshold, offset, 7)
                .unwrap();
            assert_eq!(
                page.entries
                    .iter()
                    .map(|(h, p)| (*h, p.amount))
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                page.has_more,
                (offset as u128 + expected.len() as u128) < filtered.len() as u128
            );
        }
    }
    for threshold in [0, 8, 17] {
        let mut after = None;
        let mut seen = Vec::new();
        loop {
            let page = store
                .get_pending_page_after(&owner, threshold, after, 127)
                .unwrap();
            seen.extend(page.entries.iter().map(|(hash, info)| (*hash, info.amount)));
            if !page.has_more {
                break;
            }
            let (hash, info) = page.entries.last().unwrap();
            after = Some((info.amount, *hash));
        }
        assert_eq!(
            seen,
            old.iter()
                .filter(|(_, p)| p.amount >= threshold)
                .map(|(hash, info)| (*hash, info.amount))
                .collect::<Vec<_>>()
        );
    }
    // A keyset must seek rather than walk the prefix: malformed preceding
    // index data would fail any offset scan, but cannot affect this range.
    let (boundary, pending) = &old[2000];
    let after = Some((pending.amount, *boundary));
    let mut invalid = owner.as_str().as_bytes().to_vec();
    invalid.extend_from_slice(&[0, 0]);
    let mut txn = env.env().write_txn().unwrap();
    env.pending_amount_db
        .put(&mut txn, &invalid, b"bad index")
        .unwrap();
    txn.commit().unwrap();
    assert!(store.get_pending_page(&owner, 0, 2001, 7).is_err());
    let page = store.get_pending_page_after(&owner, 0, after, 7).unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|(h, p)| (*h, p.amount))
            .collect::<Vec<_>>(),
        old[2001..2008]
            .iter()
            .map(|(h, p)| (*h, p.amount))
            .collect::<Vec<_>>()
    );
    let mut txn = env.env().write_txn().unwrap();
    env.pending_amount_db.delete(&mut txn, &invalid).unwrap();
    txn.commit().unwrap();
    store.delete_pending(&owner, boundary).unwrap();
    assert_eq!(
        store
            .get_pending_page_after(&owner, 0, after, 7)
            .unwrap()
            .entries
            .iter()
            .map(|(h, p)| (*h, p.amount))
            .collect::<Vec<_>>(),
        old[2001..2008]
            .iter()
            .map(|(h, p)| (*h, p.amount))
            .collect::<Vec<_>>()
    );
    store.put_pending(&owner, boundary, pending).unwrap();
    assert!(store
        .get_pending_page_after(&owner, 17, Some((16, *boundary)), 7)
        .unwrap()
        .entries
        .is_empty());
    // An off-page payload is not fetched or decoded to return the first page.
    let bad = pending_key(&owner, &old.last().unwrap().0);
    let mut txn = env.env().write_txn().unwrap();
    env.pending_db.put(&mut txn, &bad, b"bad payload").unwrap();
    txn.commit().unwrap();
    assert_eq!(
        store
            .get_pending_page(&owner, 0, 0, 1)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(store.get_pending_page(&owner, 0, 4095, 1).is_err());
}

#[test]
fn amount_index_survives_abort_undo_and_version_one_upgrade() {
    use burst_types::BlockHash;
    let dir = tempfile::tempdir().unwrap();
    let owner = WalletAddress::new("brst_amount_undo");
    let hash = TxHash::new([1; 32]);
    let block = BlockHash::new([2; 32]);
    {
        let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let store = env.pending_store();
        store.put_pending(&owner, &hash, &info(&owner, 7)).unwrap();
        {
            let mut batch = env.tx_begin_write().unwrap();
            batch.delete_pending(&owner, hash.as_bytes()).unwrap();
        }
        assert_eq!(
            store.get_pending_page(&owner, 0, 0, 1).unwrap().entries[0]
                .1
                .amount,
            7
        );
        let mut batch = env.tx_begin_write().unwrap();
        batch.begin_block_undo(block, owner.clone(), 1).unwrap();
        let mut account = burst_store::account::AccountInfo::unopened(owner.clone());
        account.head = block;
        account.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &owner, 1)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&owner, &block).unwrap();
        batch
            .put_pending(
                &owner,
                hash.as_bytes(),
                &bincode::serialize(&info(&owner, u128::MAX)).unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        assert_eq!(
            store
                .get_pending_page(&owner, u128::MAX, 0, 1)
                .unwrap()
                .entries[0]
                .1
                .amount,
            u128::MAX
        );
        let mut batch = env.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        batch.verify_checkpoint_lookup_indexes().unwrap();
        batch.commit().unwrap();
        assert_eq!(
            store.get_pending_page(&owner, 0, 0, 1).unwrap().entries[0]
                .1
                .amount,
            7
        );
        let mut txn = env.env().write_txn().unwrap();
        env.pending_amount_db.clear(&mut txn).unwrap();
        env.meta_db
            .put(&mut txn, b"pending_sender_index_version", &[1])
            .unwrap();
        txn.commit().unwrap();
    }
    let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        env.pending_store()
            .get_pending_page(&owner, 0, 0, 1)
            .unwrap()
            .entries[0]
            .1
            .amount,
        7
    );
    env.tx_begin_write()
        .unwrap()
        .verify_checkpoint_lookup_indexes()
        .unwrap();
    env.pending_store().delete_pending(&owner, &hash).unwrap();
    assert!(env
        .pending_store()
        .get_pending_page(&owner, 0, 0, 1)
        .unwrap()
        .entries
        .is_empty());
    env.tx_begin_write()
        .unwrap()
        .verify_checkpoint_lookup_indexes()
        .unwrap();
}

#[test]
fn captured_outgoing_snapshot_keeps_rows_and_index_from_one_cut() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let store = env.pending_store();
    let source = WalletAddress::new("brst_snapshot_source");
    let destination = WalletAddress::new("brst_snapshot_destination");
    let first = TxHash::new([1; 32]);
    let second = TxHash::new([2; 32]);
    store
        .put_pending(&destination, &first, &info(&source, 7))
        .unwrap();
    let snapshot = store.capture_pending_from_account(&source).unwrap();
    store.delete_pending(&destination, &first).unwrap();
    store
        .put_pending(&destination, &second, &info(&source, 11))
        .unwrap();
    let old = snapshot.into_entries().unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(
        (&old[0].0, old[0].1, old[0].2.amount),
        (&destination, first, 7)
    );
    let current = store.get_pending_from_account(&source).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!((current[0].1, current[0].2.amount), (second, 11));

    // Capture itself must not decode rows under the live publication lock.
    let mut txn = env.env().write_txn().unwrap();
    env.pending_db
        .put(
            &mut txn,
            &pending_key(&destination, &second),
            b"bad payload",
        )
        .unwrap();
    txn.commit().unwrap();
    let snapshot = store.capture_pending_from_account(&source).unwrap();
    assert!(snapshot.into_entries().is_err());
}

#[test]
fn outgoing_totals_stream_same_cut_and_reject_invalid_totals() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), 32, 32 << 20).unwrap();
    let store = env.pending_store();
    let owner = WalletAddress::new("brst_totals");
    let mut batch = env.tx_begin_write().unwrap();
    for n in 0..4096u64 {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&n.to_be_bytes());
        let mut pending = info(&owner, 3);
        pending.return_credit = n % 2 == 0;
        batch
            .put_pending(&owner, &hash, &bincode::serialize(&pending).unwrap())
            .unwrap();
    }
    batch.commit().unwrap();
    let snapshot = store.capture_pending_from_account(&owner).unwrap();
    store
        .put_pending(&owner, &TxHash::new([255; 32]), &info(&owner, u128::MAX))
        .unwrap();
    let totals = snapshot.totals(&owner).unwrap();
    assert_eq!((totals.amount, totals.count), (6144, 2048));
    assert!(store
        .capture_pending_from_account(&owner)
        .unwrap()
        .totals(&owner)
        .is_err());
    store
        .delete_pending(&owner, &TxHash::new([255; 32]))
        .unwrap();
    assert!(store
        .capture_pending_from_account(&owner)
        .unwrap()
        .totals(&WalletAddress::new("brst_wrong_sender"))
        .is_err());
    let mut totals = burst_store::pending::PendingSourceTotals {
        amount: 4,
        count: u64::MAX,
    };
    assert!(totals.include(&owner, &info(&owner, 1)).is_err());
    assert_eq!((totals.amount, totals.count), (4, u64::MAX));
}
