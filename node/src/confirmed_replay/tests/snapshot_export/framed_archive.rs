use super::*;
use crate::checkpoint_sync::{
    snapshot::{Config, Fetch, Transfer},
    Incoming,
};
use burst_consensus::{
    checkpoint::HumanDelegation,
    checkpoint::HumanRoster,
    snapshot::{SnapshotAuthorityStep, SnapshotCertificate},
};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

#[test]
fn framed_archive_crosses_legacy_header_boundary_and_transfers_restored_state() {
    framed_archive_journey(None, 64);
}

#[test]
fn framed_archive_exceeds_old_total_limit_with_explicit_operator_budget() {
    framed_archive_journey(None, 320);
}

#[test]
#[ignore = "requires BURST_SNAPSHOT_DAEMON pointing to a freshly built executable"]
fn daemon_verifies_framed_archive_beyond_legacy_header_limit() {
    framed_archive_journey(
        Some(std::env::var_os("BURST_SNAPSHOT_DAEMON").expect("set BURST_SNAPSHOT_DAEMON")),
        64,
    );
}

fn framed_archive_journey(daemon: Option<std::ffi::OsString>, transitions: usize) {
    let fixture = Fixture::selection();
    let (_dir, store, heads) = fixture.store();
    let candidate = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        1_595_434_050,
        &heads,
        &address(20),
    )
    .unwrap();
    let record = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(candidate.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    let events =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let mut anchor =
        crate::checkpoint_store::chain::next_anchor(&events, NetworkId::Dev, &fixture.params)
            .unwrap();
    let original = anchor.roster().clone();
    let mut authority = vec![SnapshotAuthorityStep {
        certificate: record.certificate().clone(),
        roster: original.humans().to_vec(),
    }];
    let large = HumanRoster::new(
        (0..2048u32)
            .map(|index| {
                let mut seed = [73; 32];
                seed[..4].copy_from_slice(&index.to_be_bytes());
                HumanDelegation {
                    human: burst_crypto::derive_address(
                        &burst_crypto::keypair_from_seed(&seed).public,
                    ),
                    representative: address(0),
                }
            })
            .collect(),
    )
    .unwrap();
    // Synthetic quorum-valid authority transitions exercise archive framing.
    // This is not evidence of replaying these identity changes from blocks.
    for index in 0..=transitions {
        let roster = if index == transitions {
            original.clone()
        } else {
            large.clone()
        };
        let mut checkpoint = record.certificate().checkpoint.clone();
        checkpoint.parent = anchor.hash();
        checkpoint.height = anchor.height() + 1;
        checkpoint.cutoff = anchor.cutoff() + 1;
        checkpoint.next_roster = roster.commitment();
        let signers: &[u8] = if index == 0 {
            &[0, 1, 2, 3, 4, 5, 6, 7, 8]
        } else {
            &[0]
        };
        let step = SnapshotAuthorityStep {
            certificate: certified_votes::certify(&checkpoint, signers),
            roster: roster.humans().to_vec(),
        };
        anchor = step.authenticate(&anchor).unwrap();
        authority.push(step);
    }
    // Restore the original certified selection receipts before rebinding the
    // synthetic final authority context.
    let mut state = ConfirmedReplay::replay_with_events(
        &fixture.history(1_595_434_050),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    state.payment_epoch = anchor.hash();
    state.cutoff = Timestamp::new(anchor.cutoff());
    let prepared = state.prepare_snapshot(&anchor).unwrap();
    let certificate = SnapshotCertificate {
        manifest: prepared.manifest().clone(),
        signatures: (0..7)
            .map(|seed| {
                prepared
                    .sign(
                        &anchor,
                        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )
                    .unwrap()
            })
            .collect(),
    };
    let files = tempfile::tempdir().unwrap();
    let source = files.path().join("framed.snapshot");
    let output = files.path().join("received.snapshot");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    let mut compact = Vec::new();
    prepared
        .write_archive(&base, &authority, &certificate, &mut compact)
        .unwrap();
    assert_eq!(&compact[..16], b"BURST-SNAPSHOT\0\x04");
    assert!(
        compact.len() < 16 * 1024 * 1024,
        "unchanged electorate proofs stay compact"
    );
    assert_eq!(
        AuthenticatedSnapshot::read_in(&base, &mut compact.as_slice(), files.path())
            .unwrap()
            .into_state()
            .snapshot_bytes()
            .unwrap(),
        prepared.bytes()
    );
    // Keep exercising legacy v3 transfer budgets with the original large archive.
    prepared
        .write_legacy_archive(&base, &authority, &certificate, &[], &mut file)
        .unwrap();
    assert!(file.metadata().unwrap().len() > 16 * 1024 * 1024);
    let archive_bytes = file.metadata().unwrap().len();
    if transitions > 64 {
        assert!(archive_bytes > Config::default().max_archive_bytes);
    }
    assert!(Transfer::new(
        Config {
            max_archive_bytes: archive_bytes - 1,
            serve: Some(source.clone()),
            fetch: None,
        },
        base.clone(),
        files.path()
    )
    .is_err());
    file.rewind().unwrap();
    let mut prefix = [0; 24];
    file.read_exact(&mut prefix).unwrap();
    assert_eq!(&prefix[..16], b"BURST-SNAPSHOT\0\x03");
    let header = u64::from_be_bytes(prefix[16..24].try_into().unwrap());
    assert!(header > 16 * 1024 * 1024);
    file.rewind().unwrap();
    assert!(AuthenticatedSnapshot::read_file_with_budget(
        &base,
        &source,
        files.path(),
        archive_bytes - 1,
    )
    .is_err());
    let imported =
        AuthenticatedSnapshot::read_file_with_budget(&base, &source, files.path(), archive_bytes)
            .unwrap();
    assert_eq!(
        imported.into_state().snapshot_bytes().unwrap(),
        prepared.bytes()
    );
    if let Some(daemon) = daemon {
        let params = files.path().join("genesis.json");
        serde_json::to_writer(std::fs::File::create(&params).unwrap(), &fixture.params).unwrap();
        let ledger = files.path().join("must-not-be-created");
        let verify = |expected| {
            let result = std::process::Command::new(&daemon)
                .current_dir(files.path())
                .args(["--network", "dev", "--data-dir"])
                .arg(&ledger)
                .args(["snapshot", "verify", "--input"])
                .arg(&source)
                .arg("--genesis-params")
                .arg(&params)
                .arg("--expected-epoch")
                .arg(anchor.hash().to_string())
                .output()
                .unwrap();
            assert_eq!(
                result.status.success(),
                expected,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(!ledger.exists());
        };
        verify(true);
        file.seek(SeekFrom::Start(32)).unwrap();
        file.write_all(&u64::MAX.to_be_bytes()).unwrap();
        verify(false);
        return;
    }
    let mut server = Transfer::new(
        Config {
            max_archive_bytes: archive_bytes,
            serve: Some(source.clone()),
            fetch: None,
        },
        base.clone(),
        files.path(),
    )
    .unwrap();
    let mut client = Transfer::new(
        Config {
            max_archive_bytes: archive_bytes,
            serve: None,
            fetch: Some(Fetch {
                epoch: Some(anchor.hash().to_string()),
                minimum_height: anchor.height(),
                output: output.clone(),
            }),
        },
        base.clone(),
        files.path(),
    )
    .unwrap();
    let mut now = Instant::now();
    let mut request = client.poll(vec!["source".into()], now);
    while let Some(requested) = request {
        assert!(!output.exists());
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
        now += Duration::from_secs(1);
        request = client
            .handle(
                Incoming {
                    peer: "source".into(),
                    message: chunk.message,
                },
                now,
            )
            .unwrap();
    }
    let restored = AuthenticatedSnapshot::read_in(
        &base,
        &mut std::fs::File::open(&output).unwrap(),
        files.path(),
    )
    .unwrap();
    assert_eq!(
        restored.into_state().snapshot_bytes().unwrap(),
        prepared.bytes()
    );
    // A valid outer length cannot authorize an oversized individual record.
    file.seek(SeekFrom::Start(32)).unwrap();
    file.write_all(&(16u64 * 1024 * 1024 + 1).to_be_bytes())
        .unwrap();
    file.rewind().unwrap();
    assert!(AuthenticatedSnapshot::read_in(&base, &mut file, files.path()).is_err());
}
