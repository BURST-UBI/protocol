use super::*;
use crate::checkpoint_store::historical::HistoricalAdmissions;

mod wire;

#[test]
fn genesis_export_reuses_history_across_selections_and_rechecks_earlier_proofs() {
    let (fixture, selections) = completed(ChallengeReason::Fraud);
    let (_dir, source, _) = restore_chain(&fixture, &selections);
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let mut heights = Vec::new();
    let mut proof_keys = Vec::new();
    let mut epochs = Vec::new();
    for event in events {
        if let CertifiedEvent::Selection(selection) = event {
            let proof = selection
                .beacon()
                .expect("completed selection has a beacon");
            let key = crate::checkpoint_store::archive_selection::beacon_key(
                selection.certificate().checkpoint.hash(),
            );
            source
                .meta_store()
                .put_meta(&key, &bincode::serialize(proof).unwrap())
                .unwrap();
            heights.push(selection.certificate().checkpoint.height);
            epochs.push(selection.certificate().checkpoint.hash());
            proof_keys.push(key);
        }
    }
    assert!(
        heights.len() >= 2,
        "fixture must exercise advancement, not just a repeated read"
    );
    let expected: Vec<_> = heights
        .iter()
        .map(|height| {
            CertifiedCheckpoint::export_record(&source, NetworkId::Dev, &fixture.params, *height)
                .unwrap()
                .unwrap()
        })
        .collect();
    let before = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let mut recovery = crate::checkpoint_store::ExportRecovery::default();
    for (height, bytes) in heights.iter().zip(&expected) {
        assert_eq!(
            CertifiedCheckpoint::export_record_with_recovery(
                &source,
                NetworkId::Dev,
                &fixture.params,
                *height,
                &mut recovery,
            )
            .unwrap()
            .unwrap(),
            *bytes
        );
        assert_eq!(recovery.height(), Some(*height));
    }
    for (height, bytes) in heights.iter().zip(&expected).rev() {
        assert_eq!(
            CertifiedCheckpoint::export_record_with_recovery(
                &source,
                NetworkId::Dev,
                &fixture.params,
                *height,
                &mut recovery,
            )
            .unwrap()
            .unwrap(),
            *bytes
        );
        assert_eq!(recovery.height(), heights.last().copied());
    }
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let startup = crate::confirmed_replay::snapshot::installed::ReplayCache {
        base: None,
        recovered: crate::confirmed_replay::snapshot::installed::RecoveredSnapshot::recover_genesis_locally(
            &source, NetworkId::Dev, &fixture.params, *epochs.last().unwrap(),
        ).unwrap(),
    };
    let mut seeded = crate::checkpoint_store::ExportRecovery::from_recovery(
        Some(&startup),
        &source,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    assert_eq!(seeded.height(), heights.last().copied());
    for (height, bytes) in heights.iter().zip(&expected) {
        assert_eq!(
            CertifiedCheckpoint::export_record_with_recovery(
                &source,
                NetworkId::Dev,
                &fixture.params,
                *height,
                &mut seeded,
            )
            .unwrap()
            .unwrap(),
            *bytes
        );
        assert_eq!(seeded.height(), heights.last().copied());
    }
    assert_eq!(
        startup.recovered.continuation.anchor().hash(),
        *epochs.last().unwrap()
    );
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    source
        .meta_store()
        .put_meta(&proof_keys[0], b"changed earlier beacon")
        .unwrap();
    assert!(
        CertifiedCheckpoint::export_record_with_recovery(
            &source,
            NetworkId::Dev,
            &fixture.params,
            *heights.last().unwrap(),
            &mut seeded,
        )
        .is_err(),
        "startup handoff must not conceal a changed earlier proof"
    );
    assert!(CertifiedCheckpoint::export_record_with_recovery(
        &source,
        NetworkId::Dev,
        &fixture.params,
        *heights.last().unwrap(),
        &mut recovery,
    )
    .is_err());
    assert_eq!(
        source.meta_store().get_meta(&proof_keys[0]).unwrap(),
        b"changed earlier beacon"
    );
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u8,
    record: Vec<u8>,
    blocks: Vec<StateBlock>,
}

#[test]
fn mixed_historical_transfer_authenticates_outcomes_without_installing_peer_state() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    pending_successor(&mut fixture);
    let (_source_dir, source, heads) = restore_chain(&fixture, &selections);
    persist_outcome(&fixture, &source, &heads);
    let next = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(21),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(21),
    )
    .unwrap();
    let third = CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 3)
        .unwrap()
        .unwrap();
    let fourth = CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 4)
        .unwrap()
        .unwrap();
    let partial = Fixture {
        params: fixture.params.clone(),
        blocks: fixture
            .blocks
            .iter()
            .filter(|block| block.timestamp.as_secs() <= CUTOFF + 35)
            .cloned()
            .collect(),
    };
    let (_partial_dir, dest, _) = restore_chain(&partial, &selections);
    let candidate = fixture.blocks.last().unwrap();
    assert!(!dest.block_store().exists(&candidate.hash).unwrap());
    let admissions = HistoricalAdmissions::default();
    assert!(CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Dev,
        &fixture.params,
        4,
        &fourth,
        &admissions
    )
    .is_err());
    assert!(!admissions.contains(&candidate.hash));
    let original: Envelope = bincode::deserialize(&third).unwrap();
    for mutation in 0..5 {
        let mut altered = original.clone();
        match mutation {
            0 => altered.version = 2,
            1 => {
                altered.blocks.pop();
            }
            2 => altered.blocks.push(altered.blocks.last().unwrap().clone()),
            3 => altered.blocks.last_mut().unwrap().signature = Signature([0; 64]),
            _ => {
                altered.record[0] = 1;
                altered.record.push(0);
            }
        }
        assert!(
            CertifiedCheckpoint::import_transfer(
                &dest,
                NetworkId::Dev,
                &fixture.params,
                3,
                &bincode::serialize(&altered).unwrap(),
                &admissions
            )
            .is_err(),
            "mutation {mutation}"
        );
        assert!(
            !admissions.contains(&candidate.hash),
            "mutation {mutation} granted an age exception"
        );
        assert!(!dest.block_store().exists(&candidate.hash).unwrap());
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&dest, NetworkId::Dev, &fixture.params).unwrap(),
            2
        );
    }
    assert!(CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Test,
        &fixture.params,
        3,
        &third,
        &admissions
    )
    .is_err());
    assert!(!admissions.contains(&candidate.hash));
    // Authenticated history and its certificate are retained for replay. The
    // transfer alone does not install accounts, balances, or ordinary blocks.
    CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Dev,
        &fixture.params,
        3,
        &third,
        &admissions,
    )
    .unwrap();
    assert!(admissions.contains(&candidate.hash));
    assert!(!dest.block_store().exists(&candidate.hash).unwrap());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&dest, NetworkId::Dev, &fixture.params).unwrap(),
        3
    );

    let (dir, dest, _) = restore_chain(&fixture, &selections);
    let before =
        bincode::serialize(&dest.account_store().get_account(&address(20)).unwrap()).unwrap();
    let admissions = HistoricalAdmissions::default();
    for (height, bytes) in [(3, &third), (4, &fourth), (3, &third)] {
        CertifiedCheckpoint::import_transfer(
            &dest,
            NetworkId::Dev,
            &fixture.params,
            height,
            bytes,
            &admissions,
        )
        .unwrap();
        assert!(admissions.contains(&candidate.hash));
    }
    assert_eq!(
        bincode::serialize(&dest.account_store().get_account(&address(20)).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&dest, NetworkId::Dev, &fixture.params).unwrap(),
        4
    );
    drop(dest);
    let dest = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let events = chain::load(&dest, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].next_roster().weight(&address(20)), 0);
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 72),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(
        replay.account(&address(20)).unwrap().state,
        WalletState::Revoked
    );
}
