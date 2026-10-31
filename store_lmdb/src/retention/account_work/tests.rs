use super::*;
use burst_store::account::AccountStore;
use burst_types::{Timestamp, WalletAddress};

fn fixture() -> (tempfile::TempDir, LmdbEnvironment, AccountInfo, BlockHash) {
    let (dir, store, _) = super::super::tests::fixture();
    let mut account = AccountInfo::unopened(WalletAddress::new("brst_frontier"));
    account.head = BlockHash::new([21; 32]);
    account.block_count = 7;
    account.confirmation_height = 4;
    account.representative = WalletAddress::new("brst_delegate");
    account.verified_at = Some(Timestamp::new(15));
    account.trst_balance = 23;
    account.revoked_trst = 9;
    store.account_store().put_account(&account).unwrap();
    let queued = BlockHash::new([22; 32]);
    store.meta_store().enqueue_confirmation(&queued).unwrap();
    (dir, store, account, queued)
}
#[test]
fn account_work_pages_preserve_full_frontiers_and_pending_work_at_pinned_cut() {
    let (_dir, store, account, queued) = fixture();
    let reader = store.trst_reference_reader().unwrap();
    let mut work = reader.account_work().unwrap();
    let mut later = account.clone();
    later.head = BlockHash::new([24; 32]);
    later.block_count += 1;
    store.account_store().put_account(&later).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.acknowledge_confirmation(&queued).unwrap();
    batch.commit().unwrap();
    let mut accounts = 0;
    let mut queue = 0;
    let mut bytes = 0;
    let mut done = false;
    for _ in 0..5 {
        let page = work
            .visit_next_page(1, 4096, |row| {
                match row {
                    AccountWorkRecord::Account(value) => {
                        assert_eq!(value, &account);
                        accounts += 1;
                    }
                    AccountWorkRecord::PendingConfirmation(value) => {
                        assert_eq!(value, queued);
                        queue += 1;
                    }
                }
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        bytes += page.bytes;
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!((accounts, queue), (1, 1));
    assert_eq!(
        bytes,
        account.address.as_str().len()
            + bincode::serialize(&account).unwrap().len()
            + crate::confirmation_queue::PREFIX.len()
            + 32
            + 1
    );
    assert!(
        work.visit_next_page(1, 4096, |_| panic!("finished"))
            .unwrap()
            .done
    );
    drop(work);
    drop(reader);
    let reader = store.trst_reference_reader().unwrap();
    let mut work = reader.account_work().unwrap();
    let page = work
        .visit_next_page(100, 4096, |row| {
            match row {
                AccountWorkRecord::Account(value) => assert_eq!(value, &later),
                _ => panic!("removed queue item"),
            };
            Ok(())
        })
        .unwrap();
    assert_eq!(page.rows, 1);
    assert!(page.done);
}
#[test]
fn account_work_pages_reject_corruption_unknown_versions_and_poison_errors() {
    let (_dir, store, account, queued) = fixture();
    let reader = store.trst_reference_reader().unwrap();
    for (rows, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut work = reader.account_work().unwrap();
        assert!(work
            .visit_next_page(rows, bytes, |_| panic!("budget"))
            .is_err());
        assert!(work.visit_next_page(1, 4096, |_| Ok(())).is_err());
    }
    let mut work = reader.account_work().unwrap();
    let mut calls = 0;
    assert!(work
        .visit_next_page(100, 4096, |_| {
            calls += 1;
            Err(invalid("stop"))
        })
        .is_err());
    assert_eq!(calls, 1);
    assert!(work.visit_next_page(100, 4096, |_| Ok(())).is_err());
    drop(work);
    drop(reader);
    let bads = [
        (b"pending_confirmation/v2/unknown".to_vec(), vec![1]),
        (crate::confirmation_queue::key(&queued), vec![2]),
        (crate::confirmation_queue::key(&BlockHash::ZERO), vec![1]),
    ];
    for (key, value) in bads {
        let mut txn = store.env().write_txn().unwrap();
        store.meta_db.put(&mut txn, &key, &value).unwrap();
        txn.commit().unwrap();
        let reader = store.trst_reference_reader().unwrap();
        let mut work = reader.account_work().unwrap();
        assert!(work.visit_next_page(100, 4096, |_| Ok(())).is_err());
        assert!(work.visit_next_page(100, 4096, |_| Ok(())).is_err());
        drop(work);
        drop(reader);
        let mut txn = store.env().write_txn().unwrap();
        store.meta_db.delete(&mut txn, &key).unwrap();
        txn.commit().unwrap();
    }
    let mut broken = account.clone();
    broken.confirmation_height = broken.block_count + 1;
    let mut txn = store.env().write_txn().unwrap();
    store
        .accounts_db
        .put(
            &mut txn,
            account.address.as_str().as_bytes(),
            &bincode::serialize(&broken).unwrap(),
        )
        .unwrap();
    txn.commit().unwrap();
    let reader = store.trst_reference_reader().unwrap();
    let mut work = reader.account_work().unwrap();
    assert!(work
        .visit_next_page(100, 4096, |_| panic!("bad account"))
        .is_err());
}
#[test]
fn account_work_pages_enforce_queue_capacity_across_page_boundaries() {
    let (_dir, store, _) = super::super::tests::fixture();
    let mut txn = store.env().write_txn().unwrap();
    for n in 1..=crate::confirmation_queue::CAPACITY + 1 {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
        store
            .meta_db
            .put(
                &mut txn,
                &crate::confirmation_queue::key(&BlockHash::new(bytes)),
                &[1],
            )
            .unwrap();
    }
    txn.commit().unwrap();
    let reader = store.trst_reference_reader().unwrap();
    let mut work = reader.account_work().unwrap();
    let mut count = 0;
    loop {
        match work.visit_next_page(100, 16384, |_| {
            count += 1;
            Ok(())
        }) {
            Ok(page) => assert!(!page.done),
            Err(_) => break,
        }
    }
    assert_eq!(count, crate::confirmation_queue::CAPACITY);
    assert!(work.visit_next_page(100, 16384, |_| Ok(())).is_err());
}
