use super::*;
use crate::checkpoint_store::chain;
use crate::confirmed_replay::snapshot::installed;

fn chain_prefix(fixture: &Fixture) -> String {
    format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash)
}

#[test]
fn panel_reads_reuse_authenticated_certificate_only_inside_one_replay() {
    let (fixture, _dir, source, _, _) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let certificate = events.iter().find_map(|event| event.selection()).unwrap();
    crate::confirmed_replay::selections::assert_scoped_reuse(certificate);
}

#[test]
fn recovery_reads_selection_history_once_and_keeps_beacon_and_trailing_guards() {
    use crate::confirmed_replay::ReplayError;
    use std::{borrow::Cow, cell::Cell};
    struct Counted<'a> {
        rows: Vec<&'a CertifiedCheckpoint>,
        reads: Cell<usize>,
        trailing_error: bool,
    }
    impl SelectionSource for Counted<'_> {
        fn len(&self) -> usize {
            self.rows.len()
        }
        fn read_selection(
            &self,
            position: usize,
        ) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
            self.reads.set(self.reads.get() + 1);
            self.rows.read_selection(position)
        }
        fn read_all(
            &self,
        ) -> impl Iterator<Item = Result<Cow<'_, CertifiedCheckpoint>, ReplayError>> {
            (0..self.len())
                .map(|position| self.read_selection(position))
                .chain(self.trailing_error.then(|| {
                    Err(crate::confirmed_replay::invalid(
                        "trailing authority failure",
                    ))
                }))
        }
    }
    let (fixture, _dir, source, _, _) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let mut anchors = vec![events[0].parent().clone()];
    for event in &events {
        anchors.push(
            event
                .parent()
                .advance(event.certificate(), event.next_roster().clone())
                .unwrap(),
        );
    }
    let mut selections = Counted {
        rows: events
            .iter()
            .filter_map(|event| event.selection())
            .collect(),
        reads: Cell::new(0),
        trailing_error: false,
    };
    assert!(!selections.rows.is_empty());
    let records = crate::checkpoint_store::compact::read_after(
        &source,
        anchors.iter().cloned().map(Ok),
        &selections,
    )
    .unwrap();
    assert_eq!(selections.reads.get(), selections.len());
    assert_eq!(records.successors.len(), 0);
    records
        .observations
        .verify(&source.tx_begin_write().unwrap())
        .unwrap();
    let beacon_key = crate::checkpoint_store::archive_selection::beacon_key(
        selections.rows[0].certificate().checkpoint.hash(),
    );
    source
        .meta_store()
        .put_meta(&beacon_key, b"damaged beacon")
        .unwrap();
    assert!(records
        .observations
        .verify(&source.tx_begin_write().unwrap())
        .is_err());
    assert!(crate::checkpoint_store::compact::read_after(
        &source,
        anchors.iter().cloned().map(Ok),
        &selections,
    )
    .is_err());
    source.meta_store().delete_meta(&beacon_key).unwrap();
    selections.trailing_error = true;
    let error = crate::checkpoint_store::compact::read_after(
        &source,
        anchors.into_iter().map(Ok),
        &selections,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("trailing authority failure"));
}

#[test]
fn archived_successors_require_the_original_certified_history() {
    for case in 0..4 {
        let (fixture, _dir, source, _, _) = selection::setup();
        let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
        let snapshot = authenticate(
            &source,
            &fixture.params,
            events[0].certificate().checkpoint.hash(),
        );
        let expected = authenticate(
            &source,
            &fixture.params,
            events[1].certificate().checkpoint.hash(),
        )
        .into_state();
        let prefix = chain_prefix(&fixture);
        let key = format!("{prefix}/2");
        let archived = format!(
            "checkpoint_record_archive/v1/dev/{}/2",
            fixture.blocks[0].hash
        );
        let mut bytes = source.meta_store().get_meta(&key).unwrap();
        if case == 2 {
            bytes = source
                .meta_store()
                .get_meta(&format!("{prefix}/1"))
                .unwrap();
        } else if case == 3 {
            bytes[0] = 255;
        }
        if case != 1 {
            source.meta_store().put_meta(&archived, &bytes).unwrap();
        }
        source.meta_store().delete_meta(&key).unwrap();
        let before = source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let result = snapshot.recover_stored_successors(&source, |base, plan, _| {
            crate::checkpoint_sync::witness::read_plan(&source, base, plan)
        });
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        if let Ok(recovered) = result {
            compact::assert_same(recovered.continuation.state(), &expected);
            recovered
                .verify_records_unchanged(&source.tx_begin_write().unwrap())
                .unwrap();
        }
        assert_eq!(
            source
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn successor_references_reload_exact_bodies_and_reject_replacement_or_removal() {
    use crate::checkpoint_store::compact::{read_after, ExportRecords};
    let (fixture, _dir, source, _, _) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let genesis = events[0].parent().clone();
    let records = read_after(&source, std::iter::once(Ok(genesis.clone())), &[]).unwrap();
    assert_eq!(records.successors.len(), events.len() as u64);
    let mut second = None;
    let mut index = 0;
    records
        .successors
        .visit(|reference| {
            let event = &events[index];
            let loaded = reference.load(&source)?;
            assert_eq!(loaded.certificate, *event.certificate());
            assert_eq!(loaded.manifest, event.manifest());
            if index == 1 {
                second = Some(reference);
            }
            index += 1;
            Ok(())
        })
        .unwrap();
    let mut export = ExportRecords::new(&source, genesis).unwrap();
    assert_eq!(
        export.next().unwrap().unwrap().manifest,
        events[0].manifest()
    );
    let reference = second.unwrap();
    let key = format!("{}/{}", chain_prefix(&fixture), reference.height);
    let original = source.meta_store().get_meta(&key).unwrap();
    source.meta_store().put_meta(&key, b"changed").unwrap();
    assert!(reference
        .load(&source)
        .err()
        .unwrap()
        .to_string()
        .contains("changed"));
    assert!(export.next().err().unwrap().to_string().contains("changed"));
    source.meta_store().delete_meta(&key).unwrap();
    assert!(reference
        .load(&source)
        .err()
        .unwrap()
        .to_string()
        .contains("disappeared"));
    source.meta_store().put_meta(&key, &original).unwrap();
    assert_eq!(
        reference.load(&source).unwrap().manifest,
        events[1].manifest()
    );
    let batch = source.tx_begin_write().unwrap();
    records.observations.verify(&batch).unwrap();
}

pub(super) fn assert_stored_chain(fixture: &Fixture, source: &LmdbStore, start: usize) {
    let events = chain::load(source, NetworkId::Dev, &fixture.params).unwrap();
    let epoch = events[start].certificate().checkpoint.hash();
    let snapshot = authenticate(source, &fixture.params, epoch);
    let floors: HashMap<_, _> = events[start]
        .heads()
        .iter()
        .map(|head| (head.account.clone(), head.height))
        .collect();
    let last = events.last().unwrap();
    let history = ConfirmedHistory::read_certified(
        source,
        NetworkId::Dev,
        fixture.blocks[0].hash,
        last.certificate().checkpoint.cutoff,
        last.heads(),
        &[],
    )
    .unwrap();
    let mut heights = HashMap::new();
    let blocks: Vec<_> = history
        .replay_blocks()
        .iter()
        .filter(|block| {
            let height = heights.entry(block.account.clone()).or_insert(0);
            *height += 1;
            *height > *floors.get(&block.account).unwrap_or(&0)
        })
        .cloned()
        .collect();
    assert!(blocks.len() < history.replay_blocks().len());
    let (dir, suffix) = suffix_store(&blocks);
    let prefix = chain_prefix(fixture);
    // A snapshot taken before a beacon arrived cannot contain that proof.
    // Retain the later proof-bearing selection record, but no old ledger bodies.
    for event in &events[..=start] {
        if event
            .selection()
            .is_some_and(|selection| selection.beacon().is_some())
        {
            let key = format!("{prefix}/{}", event.certificate().checkpoint.height);
            suffix
                .meta_store()
                .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
                .unwrap();
        }
    }
    for height in start as u64 + 2..=last.certificate().checkpoint.height {
        let key = format!("{prefix}/{height}");
        suffix
            .meta_store()
            .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
            .unwrap();
    }
    suffix
        .meta_store()
        .put_meta(
            &format!("{prefix}/head"),
            &last.certificate().checkpoint.height.to_be_bytes(),
        )
        .unwrap();
    for witness in history.proposal_witnesses() {
        suffix
            .meta_store()
            .put_meta(
                &crate::governance_content::key(witness.block),
                &witness.bytes,
            )
            .unwrap();
    }
    // A real archive survives closing every LMDB handle. No pre-base ledger
    // body is copied into this restarted input store.
    let export = SnapshotExport::prepare(source, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let signatures: Vec<_> = (0..32)
        .filter(|seed| snapshot.anchor().roster().weight(&address(*seed)) > 0)
        .map(|seed| {
            export
                .attest(
                    source,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect();
    let mut archive = Vec::new();
    export.write(&signatures, &mut archive).unwrap();
    suffix
        .meta_store()
        .put_meta(installed::BASE_KEY, &archive)
        .unwrap();
    suffix.force_sync().unwrap();
    drop(suffix);
    let suffix = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let before = suffix
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let snapshot = installed::load_base(&suffix, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    let recovered = snapshot
        .recover_stored_successors(&suffix, |base, plan, _| {
            let mut needed: HashSet<_> = plan.witness_hashes().iter().copied().collect();
            let mut retained = Vec::new();
            for account in floors
                .keys()
                .chain(plan.heads().iter().map(|head| &head.account))
                .collect::<HashSet<_>>()
            {
                let Some(mut cursor) = base.history_cursor(account).unwrap() else {
                    continue;
                };
                while let Some(next) = cursor.next() {
                    for witness in cursor
                        .accept(&read_chunk(source, next.hash, MAX_WITNESS_BLOCKS).unwrap())
                        .unwrap()
                    {
                        if needed.remove(&witness.block().hash) {
                            retained.push(witness);
                        }
                    }
                }
            }
            assert!(needed.is_empty());
            let local = crate::checkpoint_sync::witness::read_plan(source, base, plan).unwrap();
            assert_eq!(
                local
                    .iter()
                    .map(|proof| proof.block().hash)
                    .collect::<HashSet<_>>(),
                retained
                    .iter()
                    .map(|proof| proof.block().hash)
                    .collect::<HashSet<_>>()
            );
            Ok(retained)
        })
        .unwrap();
    let expected =
        ConfirmedReplay::replay_with_events(&history, NetworkId::Dev, &fixture.params, &events)
            .unwrap();
    compact::assert_same(recovered.continuation.state(), &expected);
    assert_eq!(
        recovered.continuation.anchor().hash(),
        last.certificate().checkpoint.hash()
    );
    let now = std::time::Instant::now();
    let relay = recovered.replayed_decision_relay(now).unwrap();
    if start + 1 == events.len() {
        assert!(relay.is_none());
    } else {
        let (context, mut relay) = relay.expect("replayed successor must retain its decision");
        let (expected_context, mut expected_relay) = crate::checkpoint_transport::decision_relay(
            last.parent(),
            last.certificate().clone(),
            last.manifest(),
            now,
        )
        .unwrap();
        assert_eq!(context, expected_context);
        let peers = vec!["decision-observer".to_owned()];
        let actual = relay.poll(&peers, now);
        let expected = expected_relay.poll(&peers, now);
        assert!(!actual.is_empty());
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(actual.peer, expected.peer);
            assert_eq!(
                bincode::serialize(&actual.packet).unwrap(),
                bincode::serialize(&expected.packet).unwrap()
            );
        }
    }
    for selection in recovered.selections.read_all() {
        let selection = selection.unwrap();
        assert!(selection.replayed().history_hashes().is_err());
    }
    assert_eq!(
        suffix
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[test]
fn stored_snapshot_reauthenticates_retained_beacons_without_prefix_blocks() {
    let (fixture, _dir, source, _, certificate) = selection::setup();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let valid_beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let prefix = chain_prefix(&fixture);
    let key = format!("{prefix}/2");
    let original = source.meta_store().get_meta(&key).unwrap();
    for case in 0..3 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local) = suffix_store(&[]);
        let mut record: (
            u8,
            burst_consensus::checkpoint::CheckpointCertificate,
            Vec<crate::confirmed_history::HistoryHead>,
            WalletAddress,
            Option<burst_vrf::RandomOutput>,
        ) = bincode::deserialize(&original).unwrap();
        let mut beacon = valid_beacon.clone();
        if case == 1 {
            beacon.value[0] ^= 1;
        }
        if case == 2 {
            record.1.signatures.clear();
        }
        record.4 = Some(beacon);
        local
            .meta_store()
            .put_meta(&key, &bincode::serialize(&record).unwrap())
            .unwrap();
        local
            .meta_store()
            .put_meta(&format!("{prefix}/head"), &2u64.to_be_bytes())
            .unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let result = snapshot.recover_stored_successors(&local, |_, _, _| panic!("no successor"));
        assert_eq!(result.is_ok(), case == 0);
        if let Ok(recovered) = result {
            assert_eq!(
                recovered
                    .selections
                    .read_selection(0)
                    .unwrap()
                    .beacon()
                    .unwrap()
                    .value,
                valid_beacon.value
            );
            assert!(recovered
                .selections
                .read_selection(0)
                .unwrap()
                .replayed()
                .history_hashes()
                .is_err());
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
}

#[test]
fn stored_snapshot_recovers_successor_without_prebase_ledger_or_records() {
    let (fixture, _dir, source, _, _) = selection::setup();
    assert_stored_chain(&fixture, &source, 0);
    assert_stored_chain(&fixture, &source, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stored_snapshot_recovers_from_peer_plans_without_local_prefix_bodies() {
    use std::sync::Arc;
    use tokio::sync::{mpsc, RwLock};
    let (mut fixture, _dir, source, selected, _) = selection::setup();
    let epoch = selected.checkpoint.hash();
    let snapshot = authenticate(&source, &fixture.params, epoch);
    fixture.push(
        1,
        BlockType::ChangeRepresentative,
        selected.checkpoint.cutoff + 2,
        |block| {
            block.payment_epoch = epoch;
            block.representative = address(0);
        },
    );
    fixture.append_confirmed_prefix(&source, selected.checkpoint.cutoff + 3);
    let manifest = crate::checkpoint_roster::PreparedRoster::prepare_next(
        &source,
        NetworkId::Dev,
        &fixture.params,
        selected.checkpoint.cutoff + 4,
    )
    .unwrap()
    .unwrap()
    .manifest()
    .clone();
    let certificate = certified_votes::certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certificate.clone(),
        &crate::checkpoint_agreement::ReplayManifest::Roster(manifest.clone()),
    )
    .unwrap();
    let (_dir, local) = suffix_store(&[fixture.blocks.last().unwrap().clone()]);
    let prefix = chain_prefix(&fixture);
    for key in [format!("{prefix}/3"), format!("{prefix}/head")] {
        local
            .meta_store()
            .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
            .unwrap();
    }
    local.force_sync().unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let expected = ConfirmedReplay::replay_with_events(
        &fixture.history(certificate.checkpoint.cutoff),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    let registry = Arc::new(RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    let (sender, mut requests) = mpsc::channel::<crate::checkpoint_sync::witness::Command>(1);
    registry.write().await.set_witness_requests(sender);
    let local = Arc::new(local);
    let receiver_store = Arc::clone(&local);
    let source = Arc::new(source);
    let worker = tokio::spawn(async move {
        let mut served = false;
        while let Some(command) = requests.recv().await {
            let source = Arc::clone(&source);
            let receiver_store = Arc::clone(&receiver_store);
            let proofs = tokio::task::spawn_blocking(move || {
                witness_transfer::exchange(command.roots, command.wanted, &source, &receiver_store)
            })
            .await
            .unwrap();
            let _ = command.result.send(proofs);
            served = true;
        }
        served
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        snapshot.recover_stored_successors_from_peers(Arc::clone(&local), registry),
    )
    .await;
    if !matches!(&result, Ok(Ok(_))) {
        worker.abort();
    }
    let served = worker.await;
    let recovered = result.expect("peer recovery timed out").unwrap();
    assert!(served.unwrap(), "successor must request historical proofs");
    compact::assert_same(recovered.continuation.state(), &expected);
    assert_eq!(
        recovered.continuation.anchor().hash(),
        certificate.checkpoint.hash()
    );
    assert_eq!(recovered.selections.len(), 1);
    assert!(recovered
        .selections
        .read_selection(0)
        .unwrap()
        .replayed()
        .history_hashes()
        .is_err());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
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
fn stored_snapshot_rejects_missing_misplaced_and_damaged_successors_without_writes() {
    let (fixture, _dir, source, _, _) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let epoch = events[0].certificate().checkpoint.hash();
    let prefix = chain_prefix(&fixture);
    let valid = source
        .meta_store()
        .get_meta(&format!("{prefix}/2"))
        .unwrap();
    for case in 0..9 {
        let snapshot = authenticate(&source, &fixture.params, epoch);
        let (_dir, local) = suffix_store(&[]);
        if case != 1 {
            local
                .meta_store()
                .put_meta(&format!("{prefix}/head"), &2u64.to_be_bytes())
                .unwrap();
        }
        let bytes = match case {
            0 => None,
            2 => Some(
                source
                    .meta_store()
                    .get_meta(&format!("{prefix}/1"))
                    .unwrap(),
            ),
            3 => Some(valid[..valid.len() - 1].to_vec()),
            4 => {
                let mut bytes = valid.clone();
                bytes.push(0);
                Some(bytes)
            }
            5..=7 => {
                let mut record: (
                    u8,
                    burst_consensus::checkpoint::CheckpointCertificate,
                    Vec<crate::confirmed_history::HistoryHead>,
                    WalletAddress,
                    Option<burst_vrf::RandomOutput>,
                ) = bincode::deserialize(&valid).unwrap();
                match case {
                    5 => record.1.signatures.clear(),
                    6 => record.2[0].hash = BlockHash::new([87; 32]),
                    7 => record.1.checkpoint.parent = BlockHash::new([88; 32]),
                    _ => unreachable!(),
                }
                Some(bincode::serialize(&record).unwrap())
            }
            _ => Some(valid.clone()),
        };
        if let Some(bytes) = bytes {
            local
                .meta_store()
                .put_meta(&format!("{prefix}/2"), &bytes)
                .unwrap();
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let mut requested = false;
        let result = snapshot.recover_stored_successors(&local, |_, _, _| {
            assert_eq!(case, 8, "invalid records reached replay");
            requested = true;
            Err(burst_store::StoreError::NotFound(
                "required historical witness".into(),
            ))
        });
        assert!(result.is_err(), "case {case}");
        assert_eq!(requested, case == 8);
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
