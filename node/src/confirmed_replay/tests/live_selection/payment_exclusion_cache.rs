use super::*;
use burst_store::{account::AccountStore, meta::MetaStore};
use burst_types::payment_recovery::ExclusionQuery;
use std::sync::Arc;

#[test]
fn exclusion_cache_reuses_authority_but_rechecks_frontier_closure_and_reopen() {
    let (dir, store, fixture, _, _) = setup();
    let store = Arc::new(store);
    let reader = crate::payment_exclusion::Reader::new(store.clone());
    let account = address(0);
    let info = store.account_store().get_account(&account).unwrap();
    let old_epoch = crate::genesis_state::block(NetworkId::Dev, &fixture.params).hash;
    let query = ExclusionQuery {
        account: account.to_string(),
        previous: info.head.to_string(),
        payment_epoch: old_epoch.to_string(),
    };
    let current = ExclusionQuery {
        payment_epoch: store.meta_store().payment_epoch().unwrap().to_string(),
        ..query.clone()
    };
    assert!(!reader.read(current).unwrap().excluded);
    assert_eq!(reader.validation_count(), 0);
    for _ in 0..3 {
        assert!(reader.read(query.clone()).unwrap().excluded);
    }
    assert_eq!(reader.validation_count(), 1);

    let mut changed = info.clone();
    changed.head = BlockHash::new([97; 32]);
    changed.block_count += 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&changed, false).unwrap();
    batch.commit().unwrap();
    assert!(!reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 1);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.commit().unwrap();
    assert!(reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 1);

    let key = burst_store::meta::closed_payment_epoch_key(&old_epoch);
    let closure = store.meta_store().get_meta(&key).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.delete_meta(&key).unwrap();
    batch.commit().unwrap();
    assert!(!reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 2);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&key, &closure).unwrap();
    batch.commit().unwrap();
    assert!(reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 3);
    store
        .meta_store()
        .put_meta("exclusion-untracked", b"write")
        .unwrap();
    assert!(reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 4);
    drop(reader);
    drop(store);
    let reopened = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let reader = crate::payment_exclusion::Reader::new(reopened);
    assert!(reader.read(query).unwrap().excluded);
    assert_eq!(reader.validation_count(), 1);
}

#[test]
fn exclusion_cache_rejects_page_only_damage_and_never_caches_errors() {
    use crate::paged_metadata::Location;
    use std::io::Write;
    struct AuthorityPages;
    impl Location for AuthorityPages {
        fn descriptor_key(&self) -> &str {
            burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY
        }
        fn page_key(&self, index: usize) -> String {
            format!(
                "{}{:08x}",
                burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX,
                index
            )
        }
    }
    let (_dir, store, fixture, _, _) = setup();
    let store = Arc::new(store);
    let bytes = crate::payment_authority::storage::read(&store)
        .unwrap()
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut writer = crate::paged_metadata::Writer::new_at(AuthorityPages, &mut batch).unwrap();
    writer.write_all(&bytes).unwrap();
    writer.finish().unwrap();
    batch.commit().unwrap();
    let reader = crate::payment_exclusion::Reader::new(store.clone());
    let old_epoch = crate::genesis_state::block(NetworkId::Dev, &fixture.params).hash;
    let query = ExclusionQuery {
        account: address(99).to_string(),
        previous: BlockHash::ZERO.to_string(),
        payment_epoch: old_epoch.to_string(),
    };
    assert!(reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 1);
    let key = AuthorityPages.page_key(0);
    let page = store.meta_store().get_meta(&key).unwrap();
    let descriptor = store
        .meta_store()
        .get_meta(AuthorityPages.descriptor_key())
        .unwrap();
    let mut damaged = page.clone();
    damaged[0] ^= 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&key, &damaged).unwrap();
    drop(batch);
    assert!(reader.read(query.clone()).unwrap().excluded);
    assert_eq!(reader.validation_count(), 1);
    let installed = store.meta_store().payment_epoch().unwrap();
    let pinned = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&key, &damaged).unwrap();
    batch.commit().unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                for expected in [2, 3] {
                    assert!(reader.read(query.clone()).is_err());
                    assert_eq!(reader.validation_count(), expected);
                }
            })
            .join()
            .unwrap()
    });
    assert_eq!(store.read_payment_authority_identity(&pinned), None);
    assert_eq!(
        reader.authority_epoch_for_test(&pinned).unwrap(),
        Some(installed)
    );
    assert_eq!(reader.validation_count(), 4);
    drop(pinned);
    assert_eq!(
        store
            .meta_store()
            .get_meta(AuthorityPages.descriptor_key())
            .unwrap(),
        descriptor
    );
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&key, &page).unwrap();
    batch.commit().unwrap();
    for _ in 0..2 {
        assert!(reader.read(query.clone()).unwrap().excluded);
    }
    assert_eq!(reader.validation_count(), 5);
}
