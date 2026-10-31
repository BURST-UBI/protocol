use super::*;
use crate::checkpoint_store::chain;
use burst_consensus::snapshot::{SnapshotAuthorityStep, SnapshotCertificate};

#[test]
fn selection_stream_propagates_staging_failure_and_can_be_retried() {
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let mut visits = 0;
    let error = snapshot
        .try_visit_selections(|selection| {
            visits += 1;
            assert_eq!(selection.certificate(), &certificate);
            Err(invalid("selection staging write failed"))
        })
        .unwrap_err();
    assert_eq!(visits, 1);
    assert!(error.to_string().contains("selection staging write failed"));
    let mut hashes = Vec::new();
    snapshot
        .try_visit_selections(|selection| {
            hashes.push(selection.certificate().checkpoint.hash());
            Ok(())
        })
        .unwrap();
    assert_eq!(hashes, vec![certificate.checkpoint.hash()]);
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[test]
fn snapshot_beacon_supplements_are_verified_without_changing_economic_state() {
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let epoch = certificate.checkpoint.hash();
    let before = SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let mut record = chain::load(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .into_iter()
        .find_map(|event| match event {
            chain::CertifiedEvent::Selection(record) => Some(*record),
            _ => None,
        })
        .unwrap();
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
    let export = SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
    assert_eq!(before.manifest(), export.manifest());
    let attestations: Vec<_> = (0..7)
        .map(|seed| {
            export
                .attest(
                    &store,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect();
    let mut bytes = Vec::new();
    export.write(&attestations, &mut bytes).unwrap();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let restored = AuthenticatedSnapshot::read(&genesis, &mut bytes.as_slice()).unwrap();
    restored.validate_selections().unwrap();
    let selection_passes = std::cell::Cell::new(0);
    let validated = AuthenticatedSnapshot::read_validated_in_until(
        &genesis,
        &mut bytes.as_slice(),
        store.env().path(),
        &|stage| {
            if stage == "snapshot:selections-finished" {
                selection_passes.set(selection_passes.get() + 1);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        selection_passes.get(),
        1,
        "installed archives validate selections once"
    );
    assert_eq!(validated.anchor().hash(), restored.anchor().hash());
    let streamed = AuthenticatedSnapshot::read_seekable_validated_in_until(
        &genesis,
        &mut std::io::Cursor::new(&bytes),
        store.env().path(),
        &|_| Ok(()),
    )
    .unwrap();
    assert_eq!(streamed.anchor().hash(), restored.anchor().hash());
    let error = AuthenticatedSnapshot::read_seekable_validated_in_until(
        &genesis,
        &mut std::io::Cursor::new(&bytes),
        store.env().path(),
        &|stage| {
            if stage == "snapshot:seek" {
                Err(invalid("cancel archive seek"))
            } else {
                Ok(())
            }
        },
    )
    .err()
    .expect("seek cancellation must reject the snapshot");
    assert!(error.to_string().contains("cancel archive seek"));
    for case in 0..3 {
        let mut damaged = bytes.clone();
        match case {
            0 => {
                damaged.pop();
            }
            1 => damaged.push(0),
            _ => {
                *damaged.last_mut().unwrap() ^= 1;
            }
        }
        let decoded = std::cell::Cell::new(false);
        assert!(
            AuthenticatedSnapshot::read_seekable_validated_in_until(
                &genesis,
                &mut std::io::Cursor::new(damaged),
                store.env().path(),
                &|stage| {
                    if stage == "snapshot:restore-state" {
                        decoded.set(true);
                    }
                    Ok(())
                },
            )
            .is_err(),
            "streamed body corruption case {case}"
        );
        assert!(
            !decoded.get(),
            "invalid bodies must fail before state decoding"
        );
    }

    for stop_at in [
        "snapshot:read",
        "snapshot:authority-step",
        "snapshot:certificate",
        "snapshot:restore-state",
        "snapshot:state-restored",
        "snapshot:selection-step",
        "snapshot:selections-finished",
        "snapshot:decode-bytes",
        "snapshot:decoded-state",
        "snapshot:restore-brn",
        "snapshot:restore-wallets",
        "snapshot:restore-graph",
        "snapshot:restore-continuation",
        "snapshot:receipt-context",
        "snapshot:roster-context",
        "snapshot:governance-context",
        "snapshot:canonical-state",
        "snapshot:canonical-bytes",
        "snapshot:canonical-finished",
    ] {
        let error = AuthenticatedSnapshot::read_validated_in_until(
            &genesis,
            &mut bytes.as_slice(),
            store.env().path(),
            &|stage| {
                if stage == stop_at {
                    Err(invalid("cancel archive authentication"))
                } else {
                    Ok(())
                }
            },
        )
        .err()
        .expect("cancellation must not produce a snapshot");
        assert!(
            error.to_string().contains("cancel archive authentication"),
            "{stop_at}: {error}"
        );
        let error = AuthenticatedSnapshot::read_seekable_validated_in_until(
            &genesis,
            &mut std::io::Cursor::new(&bytes),
            store.env().path(),
            &|stage| {
                if stage == stop_at {
                    Err(invalid("cancel archive authentication"))
                } else {
                    Ok(())
                }
            },
        )
        .err()
        .expect("streamed cancellation must not produce a snapshot");
        assert!(
            error.to_string().contains("cancel archive authentication"),
            "{stop_at}: {error}"
        );
    }
    assert_eq!(
        bincode::serialize(&restored.recover_selections().unwrap()[0].beacon()).unwrap(),
        bincode::serialize(&record.beacon()).unwrap()
    );
    type Header = (
        Vec<SnapshotAuthorityStep>,
        SnapshotCertificate,
        Vec<(BlockHash, burst_vrf::RandomOutput)>,
    );
    let end = 24 + u64::from_be_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let original: Header = bincode::deserialize(&restored.legacy_header_bytes().unwrap()).unwrap();
    let encoded = bincode::serialize(&original).unwrap();
    let mut legacy = b"BURST-SNAPSHOT\0\x02".to_vec();
    legacy.extend_from_slice(&(encoded.len() as u64).to_be_bytes());
    legacy.extend_from_slice(&encoded);
    legacy.extend_from_slice(&bytes[end..]);
    assert_eq!(
        AuthenticatedSnapshot::read(&genesis, &mut legacy.as_slice())
            .unwrap()
            .anchor()
            .hash(),
        restored.anchor().hash()
    );
    assert_eq!(original.2.len(), 1);
    for case in 0..6 {
        let mut header = original.clone();
        match case {
            0 => header.2[0].1.proof[0] ^= 1,
            1 => {
                header.2[0].1 =
                    burst_vrf::DrandClient::beacon_to_random_output(&beacons[1]).unwrap()
            }
            2 => header.2[0].0 = BlockHash::ZERO,
            3 => header.2.push(header.2[0].clone()),
            4 => header.2[0].1.value[0] ^= 1,
            _ => header.2[0].1.proof = vec![0; 4097],
        }
        let encoded = bincode::serialize(&header).unwrap();
        let mut damaged = b"BURST-SNAPSHOT\0\x02".to_vec();
        damaged.extend_from_slice(&(encoded.len() as u64).to_be_bytes());
        damaged.extend_from_slice(&encoded);
        damaged.extend_from_slice(&bytes[end..]);
        assert!(
            AuthenticatedSnapshot::read(&genesis, &mut damaged.as_slice()).is_err(),
            "case {case}"
        );
        assert!(
            AuthenticatedSnapshot::read_validated_in_until(
                &genesis,
                &mut damaged.as_slice(),
                store.env().path(),
                &|_| Ok(()),
            )
            .is_err(),
            "installed invalid beacon case {case}"
        );
    }
}

#[test]
fn snapshot_recovers_selection_inputs_after_the_source_ledger_is_gone() {
    let (fixture, dir, store, manifest, certificate) = selection::setup();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let original = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let selection = original
        .iter()
        .find_map(chain::CertifiedEvent::selection)
        .unwrap();
    let expected_request = selection.replayed().request().clone();
    assert!(std::ptr::eq(
        selection.replayed().roster().humans(),
        selection.parent().roster().humans(),
    ));
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let expected_panel = expected_request
        .select(selection.parent(), &certificate, &beacon)
        .unwrap();
    drop(original);
    drop(store);
    drop(dir);
    let epoch = certificate.checkpoint.hash();
    assert!(snapshot
        .recover_selections_for(&Default::default())
        .unwrap()
        .is_empty());
    assert!(snapshot
        .recover_selections_for(&[BlockHash::ZERO].into())
        .unwrap()
        .is_empty());
    let requested = snapshot
        .recover_selections_for(&[epoch, BlockHash::ZERO].into())
        .unwrap();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[&epoch].certificate(), &certificate);
    assert_eq!(requested[&epoch].replayed().manifest(), manifest);
    assert_eq!(requested[&epoch].replayed().request(), &expected_request);
    let mut recovered = snapshot.recover_selections().unwrap();
    assert_eq!(recovered.len(), 1);
    let selected = snapshot
        .recover_selection(certificate.checkpoint.height)
        .unwrap()
        .unwrap();
    assert_eq!(selected.certificate(), &certificate);
    assert_eq!(selected.replayed().request(), &expected_request);
    assert_eq!(selected.replayed().manifest(), manifest);
    assert!(snapshot.recover_selection(0).unwrap().is_none());
    assert!(snapshot
        .recover_selection(certificate.checkpoint.height + 1)
        .unwrap()
        .is_none());
    let record = &mut recovered[0];
    assert!(std::ptr::eq(
        record.replayed().roster().humans(),
        record.parent().roster().humans(),
    ));
    assert_eq!(record.replayed().request(), &expected_request);
    assert_eq!(record.replayed().manifest(), manifest);
    assert!(record.replayed().history_hashes().is_err());
    assert!(record.replayed().validate_parent(record.parent()).is_err());
    assert!(record.beacon().is_none());
    record.attach_compact_beacon(beacon).unwrap();
    assert_eq!(
        record
            .replayed()
            .request()
            .select(
                record.parent(),
                record.certificate(),
                record.beacon().unwrap()
            )
            .unwrap(),
        expected_panel
    );
}

#[test]
fn candidate_replay_shares_frozen_receipts_without_changing_snapshot_encoding() {
    // The receipt layout before its in-memory data became shared.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct LegacyReceipt {
        checkpoint: burst_consensus::checkpoint::Checkpoint,
        roster: Vec<burst_consensus::checkpoint::HumanDelegation>,
        request: burst_verification::selection_request::SelectionRequest,
        heads: Vec<crate::confirmed_history::HistoryHead>,
        endorsements: BlockHash,
        parent_roster: BlockHash,
        parent_quorum_bps: u16,
        bootstrap_complete: bool,
        selection_history: BlockHash,
        lifecycle: crate::checkpoint_replay::SelectionLifecycle,
        payment_epoch: BlockHash,
    }
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let anchor = snapshot.anchor().clone();
    let state = snapshot.into_state();
    let mut candidate = state.clone();
    assert!(state
        .selection_receipts
        .shares_storage(&candidate.selection_receipts));
    let original = state.selection_receipts.get(0).unwrap().unwrap();
    let json = serde_json::to_value(original.as_ref()).unwrap();
    let legacy: LegacyReceipt = serde_json::from_value(json.clone()).unwrap();
    let bytes = bincode::serialize(&legacy).unwrap();
    assert_eq!(bincode::serialize(original.as_ref()).unwrap(), bytes);
    let decoded: crate::checkpoint_replay::SelectionReceipt = bincode::deserialize(&bytes).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), json);
    assert!(!std::ptr::eq(original.checkpoint(), decoded.checkpoint()));
    let before = state.snapshot_bytes().unwrap();
    assert_eq!(candidate.snapshot_bytes().unwrap(), before);

    let mut changed = json;
    changed["payment_epoch"] = serde_json::to_value(BlockHash::ZERO).unwrap();
    candidate
        .selection_receipts
        .replace_for_test(0, serde_json::from_value(changed).unwrap());
    assert!(candidate.prepare_snapshot(&anchor).is_err());
    assert_eq!(state.snapshot_bytes().unwrap(), before);
    state.prepare_snapshot(&anchor).unwrap();
}

#[test]
fn staged_receipts_preserve_wire_encoding_and_propagate_storage_failure() {
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let anchor = snapshot.anchor().clone();
    let state = snapshot.into_state();
    let rows: Vec<_> = state
        .selection_receipts
        .iter()
        .map(|row| row.unwrap().into_owned())
        .collect();
    assert!(!rows.is_empty());
    let encoded = bincode::serialize(&rows).unwrap();
    assert_eq!(
        bincode::serialize(&state.selection_receipts).unwrap(),
        encoded
    );
    let decoded: super::super::super::receipts::ReceiptHistory =
        bincode::deserialize(&encoded).unwrap();
    assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
    let mut memory = state.clone();
    memory.selection_receipts = super::super::super::receipts::ReceiptHistory::Memory(rows);
    assert_eq!(
        memory.snapshot_bytes().unwrap(),
        state.snapshot_bytes().unwrap()
    );
    state.prepare_snapshot(&anchor).unwrap();
    state.selection_receipts.truncate_for_test();
    assert!(state.selection_receipts.get(0).is_err());
    assert!(state.snapshot_bytes().is_err());
    assert!(state.prepare_snapshot(&anchor).is_err());
    memory.prepare_snapshot(&anchor).unwrap();
}

#[test]
fn staged_checkpoint_inventory_preserves_wire_encoding_and_fork_isolation() {
    use super::super::super::receipts::CheckpointHistory;
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let anchor = snapshot.anchor().clone();
    let state = snapshot.into_state();
    let hashes: Vec<_> = state
        .selection_checkpoints
        .iter()
        .map(|hash| *hash.unwrap())
        .collect();
    assert!(!hashes.is_empty());
    let encoded = bincode::serialize(&hashes).unwrap();
    assert_eq!(
        bincode::serialize(&state.selection_checkpoints).unwrap(),
        encoded
    );
    let mut decoded: CheckpointHistory = bincode::deserialize(&encoded).unwrap();
    assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
    let sibling = decoded.clone();
    decoded.push(BlockHash::new([91; 32])).unwrap();
    assert_eq!(decoded.len(), hashes.len() + 1);
    assert_eq!(bincode::serialize(&sibling).unwrap(), encoded);
    let mut memory = state.clone();
    memory.selection_checkpoints = CheckpointHistory::Memory(hashes);
    assert_eq!(
        memory.snapshot_bytes().unwrap(),
        state.snapshot_bytes().unwrap()
    );
    state.prepare_snapshot(&anchor).unwrap();
    state.selection_checkpoints.truncate_for_test();
    assert!(state.selection_checkpoints.get(0).is_err());
    assert!(state.snapshot_bytes().is_err());
    assert!(state.prepare_snapshot(&anchor).is_err());
    memory.prepare_snapshot(&anchor).unwrap();
}

#[test]
fn selection_receipt_comparison_checks_every_frozen_input_without_encoding() {
    let (fixture, _dir, store, _, _) = selection::setup();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let selection = events
        .iter()
        .find_map(chain::CertifiedEvent::selection)
        .unwrap();
    let replay = selection.replayed();
    let receipt = replay.receipt();
    assert!(receipt.matches_replay(replay));
    let original = serde_json::to_value(&receipt).unwrap();
    let encoded = bincode::serialize(&receipt).unwrap();
    let fields = original
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for field in fields {
        let mut changed = original.clone();
        match field.as_str() {
            "checkpoint" => changed[&field]["height"] = serde_json::json!(0),
            "roster" | "heads" => changed[&field] = serde_json::json!([]),
            "request" => changed[&field]["target"] = serde_json::to_value(address(21)).unwrap(),
            "parent_quorum_bps" => changed[&field] = serde_json::json!(1),
            "bootstrap_complete" => {
                changed[&field] = serde_json::json!(!original[&field].as_bool().unwrap());
            }
            "lifecycle" => {
                changed[&field] = serde_json::to_value(
                    crate::checkpoint_replay::SelectionLifecycle::Challenge(BlockHash::ZERO),
                )
                .unwrap();
            }
            "endorsements" | "parent_roster" | "selection_history" | "payment_epoch" => {
                changed[&field] = serde_json::to_value(BlockHash::ZERO).unwrap();
            }
            _ => panic!("add a mutation for receipt field {field}"),
        }
        let changed: crate::checkpoint_replay::SelectionReceipt =
            serde_json::from_value(changed).unwrap();
        assert_ne!(bincode::serialize(&changed).unwrap(), encoded, "{field}");
        assert!(!changed.matches_replay(replay), "{field}");
    }
}

#[test]
fn snapshot_rejects_inconsistent_frozen_selection_receipts_before_export() {
    let (fixture, _dir, store, _, certificate) = selection::setup();
    for case in 0..11 {
        let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
        let anchor = snapshot.anchor().clone();
        let mut state = snapshot.into_state();
        assert_eq!(state.network, anchor.network());
        if case == 10 {
            state.network = NetworkId::Live;
        } else if case == 0 {
            state.selection_receipts = Default::default();
        } else if case == 1 {
            let receipt = state
                .selection_receipts
                .get(0)
                .unwrap()
                .unwrap()
                .into_owned();
            state.selection_receipts.push(receipt).unwrap();
            let hash = *state.selection_checkpoints.get(0).unwrap().unwrap();
            state.selection_checkpoints.push(hash).unwrap();
        } else {
            let mut receipt =
                serde_json::to_value(state.selection_receipts.get(0).unwrap().unwrap().as_ref())
                    .unwrap();
            match case {
                2 => receipt["heads"] = serde_json::json!([]),
                3 => receipt["request"]["pool"].as_array_mut().unwrap().reverse(),
                4 => {
                    receipt["checkpoint"]["genesis"] =
                        serde_json::to_value(BlockHash::new([89; 32])).unwrap()
                }
                5 => receipt["payment_epoch"] = serde_json::to_value(BlockHash::ZERO).unwrap(),
                6 => {
                    receipt["lifecycle"] = serde_json::to_value(
                        crate::checkpoint_replay::SelectionLifecycle::Verification(BlockHash::ZERO),
                    )
                    .unwrap()
                }
                7 => receipt["roster"] = serde_json::json!([]),
                8 => receipt["parent_quorum_bps"] = serde_json::json!(6000),
                _ => receipt["request"]["target"] = serde_json::to_value(address(21)).unwrap(),
            }
            state
                .selection_receipts
                .replace_for_test(0, serde_json::from_value(receipt).unwrap());
        }
        assert!(state.prepare_snapshot(&anchor).is_err(), "case {case}");
    }
}

fn archive(
    state: &ConfirmedReplay,
    anchor: &burst_consensus::checkpoint::CheckpointAnchor,
    base: &burst_consensus::checkpoint::CheckpointAnchor,
    authority: &[SnapshotAuthorityStep],
) -> Vec<u8> {
    let prepared = state.prepare_snapshot(anchor).unwrap();
    let certificate = SnapshotCertificate {
        manifest: prepared.manifest().clone(),
        signatures: (0..7)
            .map(|seed| {
                prepared
                    .sign(
                        anchor,
                        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )
                    .unwrap()
            })
            .collect(),
    };
    let mut bytes = Vec::new();
    prepared
        .write_archive(base, authority, &certificate, &mut bytes)
        .unwrap();
    bytes
}

#[test]
fn snapshot_selection_recovery_requires_its_actual_historical_parent_authority() {
    let (fixture, _dir, store, _, certificate) = selection::setup();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let anchor = snapshot.anchor().clone();
    let mut state = snapshot.into_state();
    // A locally pinned tip can authenticate a body without supplying old parents.
    let bytes = archive(&state, &anchor, &anchor, &[]);
    assert!(
        AuthenticatedSnapshot::read_validated_in_until(
            &anchor,
            &mut bytes.as_slice(),
            store.env().path(),
            &|_| Ok(()),
        )
        .is_err(),
        "installed read must validate selections without beacons"
    );
    let short = AuthenticatedSnapshot::read(&anchor, &mut bytes.as_slice()).unwrap();
    assert!(short
        .recover_selection(certificate.checkpoint.height)
        .is_err());
    assert!(short.recover_selection(u64::MAX).is_err());
    assert!(short
        .validate_selections()
        .unwrap_err()
        .to_string()
        .contains("omits historical"));
    assert!(short
        .recover_selections()
        .err()
        .unwrap()
        .to_string()
        .contains("omits historical"));

    // Even an attested body's parent binding must match the authenticated path.
    let mut receipt =
        serde_json::to_value(state.selection_receipts.get(0).unwrap().unwrap().as_ref()).unwrap();
    receipt["parent_roster"] = serde_json::to_value(BlockHash::new([90; 32])).unwrap();
    state
        .selection_receipts
        .replace_for_test(0, serde_json::from_value(receipt).unwrap());
    let authority: Vec<_> = events
        .iter()
        .map(|event| SnapshotAuthorityStep {
            certificate: event.certificate().clone(),
            roster: event.next_roster().humans().to_vec(),
        })
        .collect();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let bytes = archive(&state, &anchor, &genesis, &authority);
    let forged = AuthenticatedSnapshot::read(&genesis, &mut bytes.as_slice()).unwrap();
    assert!(forged
        .recover_selection(certificate.checkpoint.height)
        .is_err());
    assert!(forged.recover_selection(u64::MAX).is_err());
    assert!(forged
        .validate_selections()
        .unwrap_err()
        .to_string()
        .contains("different parent authority"));
    assert!(forged
        .recover_selections()
        .err()
        .unwrap()
        .to_string()
        .contains("different parent authority"));
}
