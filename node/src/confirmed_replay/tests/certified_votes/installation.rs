use super::*;
use crate::confirmed_replay::installation::{PreparedInstallation, CUTOFF_KEY, INSTALLED_KEY};
use burst_store::{BrnStore, StoreError};

#[cfg(unix)]
mod process_crash;

fn accepted(fixture: &Fixture, store: &LmdbStore, heads: Vec<HistoryHead>) -> CertifiedCheckpoint {
    let candidate = CertifiedCheckpoint::prepare(
        store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        store,
        NetworkId::Dev,
        &fixture.params,
        certify(candidate.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap()
}

fn prepare(fixture: &Fixture, store: &LmdbStore) -> PreparedInstallation {
    PreparedInstallation::next(store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
}

fn tail(fixture: &mut Fixture, store: &LmdbStore, kind: BlockType) -> BlockHash {
    let burn = kind == BlockType::Burn;
    let hash = fixture.push(0, kind, CUTOFF + 1, |block| {
        if burn {
            block.link = destination(21);
            block.brn_spent += 1;
        }
    });
    let block = fixture.blocks.last().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut account = batch.get_account(&block.account).unwrap();
    account.block_count += 1;
    account.head = hash;
    batch
        .put_block_with_account(
            &hash,
            &bincode::serialize(block).unwrap(),
            &account.address,
            account.block_count,
        )
        .unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.put_frontier(&account.address, &hash).unwrap();
    batch.commit().unwrap();
    hash
}

#[test]
fn checkpoint_installation_replaces_poisoned_projections_and_retains_minority_evidence() {
    let mut fixture = Fixture::selection();
    let (dir, store, heads) = fixture.store();
    let record = accepted(&fixture, &store, heads);
    let expected = ConfirmedReplay::replay_with_certificates(
        &fixture.history(CUTOFF),
        NetworkId::Dev,
        &fixture.params,
        std::slice::from_ref(&record),
    )
    .unwrap();
    let installation = prepare(&fixture, &store);
    let epoch = installation.epoch();
    assert_eq!(installation.roster().total_humans(), 9);
    let excluded = tail(&mut fixture, &store, BlockType::Burn);
    let old_epoch = fixture.blocks[0].hash;
    let block = fixture.blocks.last().unwrap();
    store
        .meta_store()
        .reserve_consensus_vote(
            &old_epoch,
            &address(0),
            &address(0),
            &block.previous,
            &excluded,
            true,
            0,
        )
        .unwrap();
    let old_vote_key = format!(
        "consensus_final/v2/{old_epoch}/{}/{}/{}",
        address(0),
        address(0),
        block.previous
    );
    let promise = store.meta_store().get_meta(&old_vote_key).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta("verification_stake/v2/stale", b"stale")
        .unwrap();
    batch
        .put_meta("verification_identity_bound/v1/stale", b"stale")
        .unwrap();
    batch.put_meta("application-preference", b"keep").unwrap();
    batch.put_brn_wallet(&address(99), b"stale").unwrap();
    let stale_pending = burst_store::pending::PendingInfo {
        return_credit: false,
        source: address(99),
        amount: 99,
        timestamp: Timestamp::new(CUTOFF),
        provenance: Vec::new(),
    };
    batch
        .put_pending(
            &address(99),
            &[99; 32],
            &bincode::serialize(&stale_pending).unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store
            .pending_store()
            .get_pending_from_account(&address(99))
            .unwrap()
            .len(),
        1
    );
    assert!(!store
        .tx_begin_write()
        .unwrap()
        .uses_stable_trst_records()
        .unwrap());
    let installed = installation.commit(&store).unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .uses_stable_trst_records()
        .unwrap());
    assert_eq!(installed.retired_blocks, vec![excluded]);
    assert_eq!(installed.minority_promises, vec![excluded]);
    assert_eq!(installed.anchor.hash(), epoch);
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        store.meta_store().get_meta(CUTOFF_KEY).unwrap(),
        CUTOFF.to_be_bytes()
    );
    assert_eq!(store.meta_store().get_meta(&old_vote_key).unwrap(), promise);
    assert_eq!(
        store
            .meta_store()
            .get_meta("application-preference")
            .unwrap(),
        b"keep"
    );
    assert!(store
        .meta_store()
        .get_meta("verification_stake/v2/stale")
        .is_err());
    assert!(store
        .meta_store()
        .get_meta("verification_identity_bound/v1/stale")
        .is_err());
    assert!(store
        .brn_store()
        .get_wallet_state(&address(99))
        .unwrap()
        .is_none());
    assert!(store
        .pending_store()
        .get_pending(&address(99), &TxHash::new([99; 32]))
        .is_err());
    assert!(store
        .pending_store()
        .get_pending_from_account(&address(99))
        .unwrap()
        .is_empty());
    assert!(store.block_store().get_block(&excluded).is_err());
    assert_eq!(
        store
            .meta_store()
            .get_meta(&format!("checkpoint_orphan/v1/{old_epoch}/{excluded}"))
            .unwrap(),
        bincode::serialize(block).unwrap()
    );
    for (address, account) in &expected.accounts {
        assert_eq!(
            bincode::serialize(&store.account_store().get_account(address).unwrap()).unwrap(),
            bincode::serialize(account).unwrap()
        );
    }
    assert_eq!(installed.trst.save_wallets(), expected.trst.save_wallets());
    assert_eq!(
        installed.verification.selection_request(&address(20)),
        expected.verification.selection_request(&address(20))
    );
    assert!(installed
        .verification
        .selection_context(&address(20))
        .is_none());
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert!(store
        .tx_begin_write()
        .unwrap()
        .uses_stable_trst_records()
        .unwrap());
    assert!(store.meta_store().get_meta(TrstEngine::meta_key()).is_err());
    assert_eq!(
        crate::trst_storage::load(
            &store.tx_begin_write().unwrap(),
            fixture.params.trst_expiry_secs
        )
        .unwrap()
        .save_wallets(),
        installed.trst.save_wallets()
    );
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
}

#[test]
fn checkpoint_installation_never_replaces_cemented_or_durably_decided_tails() {
    for cemented in [false, true] {
        let mut fixture = Fixture::selection();
        let (_dir, store, heads) = fixture.store();
        accepted(&fixture, &store, heads);
        let installation = prepare(&fixture, &store);
        let hash = tail(&mut fixture, &store, BlockType::Burn);
        if cemented {
            let mut batch = store.tx_begin_write().unwrap();
            let account = batch.get_account(&address(0)).unwrap();
            batch
                .advance_confirmation_height(&account.address, account.block_count)
                .unwrap();
            batch.commit().unwrap();
        } else {
            store.meta_store().enqueue_confirmation(&hash).unwrap();
        }
        let error = installation.commit(&store).err().unwrap().to_string();
        assert!(
            error.contains(if cemented {
                "cemented history"
            } else {
                "durable confirmation"
            }),
            "{error}"
        );
        assert_eq!(
            store.account_store().get_account(&address(0)).unwrap().head,
            hash
        );
        assert_eq!(
            store.meta_store().payment_epoch().unwrap(),
            fixture.blocks[0].hash
        );
        assert!(store.meta_store().get_meta(INSTALLED_KEY).is_err());
        assert_eq!(
            store.meta_store().get_meta("merger_graph").unwrap(),
            b"invalid snapshot"
        );
    }
}

#[test]
fn checkpoint_installation_rolls_back_projection_if_later_tail_validation_fails() {
    let mut fixture = Fixture::selection();
    let (_dir, store, heads) = fixture.store();
    accepted(&fixture, &store, heads);
    let installation = prepare(&fixture, &store);
    let hash = tail(&mut fixture, &store, BlockType::GovernanceActivation);
    let bytes = store.block_store().get_block(&hash).unwrap();
    let error = installation.commit(&store).err().unwrap().to_string();
    assert!(error.contains("unsupported lifecycle"), "{error}");
    assert_eq!(store.block_store().get_block(&hash).unwrap(), bytes);
    assert_eq!(
        store.account_store().get_account(&address(0)).unwrap().head,
        hash
    );
    assert!(store
        .meta_store()
        .get_meta(&format!(
            "checkpoint_orphan/v1/{}/{hash}",
            fixture.blocks[0].hash
        ))
        .is_err());
    assert!(store.meta_store().get_meta(INSTALLED_KEY).is_err());
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        fixture.blocks[0].hash
    );
}

#[test]
fn checkpoint_installation_rechecks_record_and_context_after_replay() {
    let fixture = Fixture::selection();
    let (_dir, store, heads) = fixture.store();
    let mut record = accepted(&fixture, &store, heads);
    let installation = prepare(&fixture, &store);
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    record
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
    assert!(installation
        .commit(&store)
        .err()
        .unwrap()
        .to_string()
        .contains("became stale"));
    let installation = prepare(&fixture, &store);
    store
        .meta_store()
        .put_meta(
            burst_store::meta::PAYMENT_EPOCH_KEY,
            record.certificate().checkpoint.hash().as_bytes(),
        )
        .unwrap();
    assert!(installation
        .commit(&store)
        .err()
        .unwrap()
        .to_string()
        .contains("became stale"));
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .err()
            .unwrap()
            .to_string()
            .contains("authority and cutoff")
    );
    assert!(store.meta_store().get_meta(INSTALLED_KEY).is_err());
}

#[test]
fn installed_cutoff_is_enforced_at_live_admission() {
    let mut fixture = Fixture::selection();
    let (_dir, store, heads) = fixture.store();
    accepted(&fixture, &store, heads);
    let installed = prepare(&fixture, &store).commit(&store).unwrap();
    fixture.push(0, BlockType::Burn, CUTOFF, |block| {
        block.payment_epoch = installed.anchor.hash();
        block.link = destination(21);
        block.brn_spent += 1;
    });
    let block = fixture.blocks.last_mut().unwrap();
    for (time, accepted) in [(CUTOFF - 1, false), (CUTOFF, true), (CUTOFF + 1, true)] {
        block.timestamp = Timestamp::new(time);
        sign(0, block);
        let batch = store.tx_begin_write().unwrap();
        let result = ledger_updater::predecessor_account(&batch, block);
        assert_eq!(result.is_ok(), accepted, "{result:?}");
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(CUTOFF_KEY, &[1]).unwrap();
    assert!(matches!(
        crate::payment_epoch::require_block(&batch, block),
        Err(StoreError::Corruption(_))
    ));
}
