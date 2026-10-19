use super::*;
use crate::checkpoint_sync::{
    snapshot::{Config, Fetch, Transfer},
    Incoming,
};
use std::time::{Duration, Instant};

#[test]
fn snapshot_long_transfer_completes_from_replayed_accounts_without_absolute_deadline() {
    transfer(4000, false);
}

#[test]
#[ignore = "manual growing-state resource probe; run through limited-run.sh"]
fn snapshot_growing_state_resource_probe() {
    let accounts = std::env::var("BURST_SNAPSHOT_PROBE_ACCOUNTS")
        .expect("set BURST_SNAPSHOT_PROBE_ACCOUNTS")
        .parse::<u32>()
        .expect("account count must be an integer");
    assert!((4000..=32000).contains(&accounts));
    transfer(accounts, true);
}

fn transfer(account_count: u32, measure: bool) {
    let measured_at = Instant::now();
    let report = |stage: &str| {
        if measure {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let rss = status.lines().find(|line| line.starts_with("VmRSS:"));
            let peak = status.lines().find(|line| line.starts_with("VmHWM:"));
            eprintln!("snapshot resource probe accounts={account_count} stage={stage} elapsed_ms={} rss={} peak={}",
                measured_at.elapsed().as_millis(), rss.unwrap_or("unavailable"), peak.unwrap_or("unavailable"));
        }
    };
    let mut fixture = Fixture::selection();
    // Real signed opens enlarge the replayed economic body, not padding that a
    // decoder would reject or a test-only archive format.
    for index in 0..account_count {
        let mut seed = [42; 32];
        seed[..4].copy_from_slice(&index.to_be_bytes());
        let key = burst_crypto::keypair_from_seed(&seed);
        let account = burst_crypto::derive_address(&key.public);
        let mut block = fixture.blocks[0].clone();
        block.block_type = BlockType::Open;
        block.payment_epoch = fixture.blocks[0].hash;
        block.account = account.clone();
        block.representative = account;
        block.previous = BlockHash::ZERO;
        block.brn_balance = 0;
        block.trst_balance = 0;
        block.timestamp = Timestamp::new(1_595_434_000);
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &key.private);
        fixture.blocks.push(block);
    }
    let (_directory, store, heads) = if measure {
        fixture.store_with_map_size(128 << 20)
    } else {
        fixture.store()
    };
    report("stored-fixture");
    let candidate = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        1_595_434_050,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
    let epoch = certificate.checkpoint.hash();
    CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate,
        heads,
        address(20),
    )
    .unwrap();
    report("certified");
    let export = SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
    report("prepared-export");
    assert!(
        export.manifest().state_bytes > 1024 * 1024,
        "replayed economic body must exercise file-backed staging"
    );
    let attestations = (0..7)
        .map(|seed| {
            export
                .attest(
                    &store,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut archive = Vec::new();
    export.write(&attestations, &mut archive).unwrap();
    report("written-archive");
    if measure {
        eprintln!(
            "snapshot resource probe state_bytes={} archive_bytes={}",
            export.manifest().state_bytes,
            archive.len()
        );
    }
    assert!(
        archive.len() > 31 * 16 * 1024,
        "fixture must exceed the former time limit at four seconds per chunk"
    );
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.snapshot");
    let output = directory.path().join("received.snapshot");
    std::fs::write(&source, &archive).unwrap();
    let mut server = Transfer::new(
        Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: Some(source),
            fetch: None,
        },
        base.clone(),
        directory.path(),
    )
    .unwrap();
    let mut client = Transfer::new(
        Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: None,
            fetch: Some(Fetch {
                epoch: Some(epoch.to_string()),
                minimum_height: 1,
                output: output.clone(),
            }),
        },
        base.clone(),
        directory.path(),
    )
    .unwrap();
    let started = Instant::now();
    let mut now = started;
    let mut request = client.poll(vec!["source".into()], now);
    let mut chunks = 0;
    while let Some(requested) = request {
        let peer = requested.peer;
        assert!(!output.exists(), "partial archive must not be published");
        let chunk = server
            .handle(
                Incoming {
                    peer: "receiver".into(),
                    message: requested.message,
                },
                now,
            )
            .unwrap()
            .unwrap();
        now += Duration::from_secs(4);
        request = client
            .handle(
                Incoming {
                    peer: peer.clone(),
                    message: chunk.message.clone(),
                },
                now,
            )
            .unwrap();
        chunks += 1;
        if request.is_some() {
            assert!(
                client
                    .handle(
                        Incoming {
                            peer: peer.clone(),
                            message: chunk.message
                        },
                        now
                    )
                    .unwrap()
                    .is_none(),
                "duplicate chunk must not advance the download"
            );
        }
        if chunks == 2 {
            now += Duration::from_secs(5);
            let resumed = client.poll(vec!["backup".into()], now).unwrap();
            assert_eq!(resumed.peer, "backup");
            match &resumed.message {
                crate::checkpoint_sync::CheckpointSyncMessage::SnapshotRequest {
                    offset,
                    digest,
                    ..
                } => {
                    assert_eq!(
                        *offset,
                        2 * 16 * 1024,
                        "peer loss must retain accepted chunks"
                    );
                    assert!(digest.is_some(), "resumed requests must pin the same bytes");
                }
                _ => panic!("expected resumed snapshot request"),
            }
            request = Some(resumed);
        }
    }
    assert!(chunks > 30);
    assert!(now.duration_since(started) > Duration::from_secs(120));
    assert_eq!(std::fs::read(&output).unwrap(), archive);
    report("transferred");
    let snapshot = AuthenticatedSnapshot::read_in(
        &base,
        &mut std::fs::File::open(&output).unwrap(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(snapshot.anchor().hash(), epoch);
    assert_eq!(&snapshot.certificate().manifest, export.manifest());
    report("restored");
    let minimal = Fixture {
        params: fixture.params.clone(),
        blocks: vec![fixture.blocks[0].clone()],
    };
    let (local_directory, local, _) = minimal.store_with_map_size(128 << 20);
    let needs = snapshot.installation_needs(&local).unwrap();
    assert!(needs.snapshot_heads.len() >= account_count as usize);
    let mut requested = std::collections::HashSet::new();
    let mut proofs = Vec::new();
    let hashes: Vec<_> = needs
        .snapshot_heads
        .iter()
        .chain(&needs.cemented_heads)
        .map(|head| head.hash)
        .collect();
    let mut cursors: std::collections::HashMap<_, _> = snapshot
        .committed_history_cursors(&hashes)
        .unwrap()
        .into_iter()
        .map(|cursor| (cursor.next().unwrap().hash, cursor))
        .collect();
    for head in needs.snapshot_heads.iter().chain(&needs.cemented_heads) {
        if !requested.insert(head.hash) {
            continue;
        }
        let mut cursor = cursors.remove(&head.hash).unwrap();
        proofs.extend(
            cursor
                .accept(&[store.block_store().get_block(&head.hash).unwrap()])
                .unwrap(),
        );
    }
    assert!(needs.confirmation_targets.is_empty());
    snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap()
        .commit_for_startup()
        .unwrap();
    local.force_sync().unwrap();
    drop(proofs);
    report("installed");
    drop(local);
    let reopened = LmdbStore::open(local_directory.path(), 32, 128 << 20).unwrap();
    let recovered = crate::confirmed_replay::snapshot::installed::load_base(
        &reopened,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .into_continuation();
    assert_eq!(recovered.anchor().hash(), epoch);
    assert!(recovered.state().accounts.len() >= account_count as usize);
    let plan = recovered.plan_cemented_tail(&reopened).unwrap();
    let witnesses =
        crate::checkpoint_sync::witness::read_plan(&store, &recovered, plan.witness_plan())
            .unwrap();
    let tail = plan.finish(witnesses).unwrap();
    recovered
        .verify_installed_metadata(&reopened, &tail)
        .unwrap();
    tail.verify_unchanged(&reopened.tx_begin_write().unwrap())
        .unwrap();
    report("reopened-startup-checked");
}
