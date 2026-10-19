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
