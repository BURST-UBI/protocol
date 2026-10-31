use super::*;
use burst_governance::signing_delegation::{
    view::{index_key, Direction},
    DelegationAuthority, Grant, KeyScope, Revocation,
};
use burst_store::meta::MetaStore;
use burst_types::TxHash;

fn genesis() -> BlockHash {
    BlockHash::new([81; 32])
}
fn fixture() -> (tempfile::TempDir, LmdbStore, String, DelegationAuthority) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = burst_crypto::keypair_from_seed(&[82; 32]);
    let delegate = burst_crypto::keypair_from_seed(&[83; 32]);
    let scope = KeyScope::Proposal(TxHash::new([84; 32]));
    let account = burst_crypto::derive_address(&owner.public);
    let recipient = burst_crypto::derive_address(&delegate.public);
    let key = scoped::key(NetworkId::Dev, genesis(), &account, &scope).unwrap();
    let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
    authority
        .apply_grant(
            Grant::create(&owner, delegate.public, scope, 1, NetworkId::Dev, genesis()).unwrap(),
        )
        .unwrap();
    store
        .meta_store()
        .put_meta(&key, &authority.save().unwrap())
        .unwrap();
    for (direction, person) in [
        (Direction::Outgoing, &account),
        (Direction::Incoming, &recipient),
    ] {
        store
            .meta_store()
            .put_meta(
                &index_key(direction, person, &key).unwrap(),
                BlockHash::new([85; 32]).as_bytes(),
            )
            .unwrap();
    }
    (dir, store, key, authority)
}

#[test]
fn delegation_pages_pin_signed_grants_across_revocation_and_keep_tombstones() {
    let (_dir, store, key, mut authority) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        DelegationReferenceReader::new(&store, &view, NetworkId::Dev, genesis()).unwrap();
    let owner = burst_crypto::keypair_from_seed(&[82; 32]);
    let account = burst_crypto::derive_address(&owner.public);
    let recipient =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[83; 32]).public);
    authority
        .apply_revocation(
            &Revocation::create(
                &owner,
                KeyScope::Proposal(TxHash::new([84; 32])),
                2,
                NetworkId::Dev,
                genesis(),
            )
            .unwrap(),
        )
        .unwrap();
    store
        .meta_store()
        .put_meta(&key, &authority.save().unwrap())
        .unwrap();
    store
        .meta_store()
        .put_meta(
            &index_key(Direction::Incoming, &recipient, &key).unwrap(),
            BlockHash::ZERO.as_bytes(),
        )
        .unwrap();
    store
        .meta_store()
        .put_meta(
            &index_key(Direction::Outgoing, &account, &key).unwrap(),
            BlockHash::new([86; 32]).as_bytes(),
        )
        .unwrap();
    let mut grants = 0;
    let mut blocks = Vec::new();
    let mut rows = 0;
    let mut done = false;
    for _ in 0..4 {
        let page = reader
            .visit_next_page(1, 4096, |r| {
                match r {
                    Reference::Grant(_) => grants += 1,
                    Reference::Block(hash) => blocks.push(hash),
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        rows += page.rows;
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(rows, 3);
    assert_eq!(grants, 1);
    assert_eq!(blocks, [BlockHash::new([85; 32]); 2]);
    drop(reader);
    drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        DelegationReferenceReader::new(&store, &view, NetworkId::Dev, genesis()).unwrap();
    let mut scopes = 0;
    let mut blocks = Vec::new();
    let page = reader
        .visit_next_page(100, 4096, |r| {
            match r {
                Reference::Grant(_) => panic!("revoked grant"),
                Reference::DelegationScope(_) => scopes += 1,
                Reference::Block(hash) => blocks.push(hash),
                _ => {}
            }
            Ok(())
        })
        .unwrap();
    assert!(page.done);
    assert_eq!(page.rows, 3);
    assert_eq!(scopes, 3);
    assert_eq!(blocks, [BlockHash::new([86; 32])]);
}

#[test]
fn delegation_pages_reject_wrong_chain_schema_budgets_and_callback_failures() {
    let (_dir, store, _, _) = fixture();
    let view = store.tx_begin_read().unwrap();
    assert!(
        DelegationReferenceReader::new(&store, &view, NetworkId::Dev, BlockHash::ZERO).is_err()
    );
    for (rows, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut reader =
            DelegationReferenceReader::new(&store, &view, NetworkId::Dev, genesis()).unwrap();
        assert!(reader
            .visit_next_page(rows, bytes, |_| panic!("bad budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    }
    let mut reader =
        DelegationReferenceReader::new(&store, &view, NetworkId::Test, genesis()).unwrap();
    assert!(reader
        .visit_next_page(100, 4096, |_| panic!("wrong chain"))
        .is_err());
    drop(reader);
    let mut reader =
        DelegationReferenceReader::new(&store, &view, NetworkId::Dev, genesis()).unwrap();
    let mut calls = 0;
    assert!(
        matches!(reader.visit_next_page(100,4096,|_|{calls+=1;Err(StoreError::Backend("stop".into()))}),Err(StoreError::Backend(reason)) if reason=="stop")
    );
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    drop(reader);
    drop(view);
    for key in ["key_delegation/v1/legacy", "key_delegation/v3/unknown"] {
        store.meta_store().put_meta(key, b"invalid").unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader =
            DelegationReferenceReader::new(&store, &view, NetworkId::Dev, genesis()).unwrap();
        assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
        drop(reader);
        drop(view);
        store.meta_store().delete_meta(key).unwrap();
    }
}

#[test]
fn delegation_lifecycle_inventory_covers_versions_and_atomic_retirement_boundaries() {
    let (_dir, store, _, _) = fixture();
    for key in [
        "key_delegation/v1/legacy",
        "key_delegation/v3/unknown",
        "key_delegation-other",
    ] {
        store.meta_store().put_meta(key, b"retained").unwrap();
    }
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let mut keys = Vec::new();
        batch
            .visit_checkpoint_lifecycle(|key, _| {
                keys.push(key.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(keys.len(), 5);
        assert!(keys.contains(&b"key_delegation/v1/legacy".to_vec()));
        assert!(keys.contains(&b"key_delegation/v3/unknown".to_vec()));
        batch.clear_checkpoint_lifecycle().unwrap();
        assert!(batch
            .checkpoint_metadata(b"key_delegation/")
            .unwrap()
            .is_empty());
        assert_eq!(
            batch.get_meta("key_delegation-other").unwrap(),
            Some(b"retained".to_vec())
        );
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.checkpoint_metadata(b"key_delegation/").unwrap().len(),
            if commit { 0 } else { 5 }
        );
        assert_eq!(
            batch.get_meta("key_delegation-other").unwrap(),
            Some(b"retained".to_vec())
        );
    }
}
