use super::*;
use heed::types::Bytes;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

mod publication;

const CHILD: &str =
    "confirmed_replay::tests::certified_votes::installation::process_crash::crash_child";
const READY: &str = "BURST_INSTALLATION_CRASH_READY";
type Contents = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

fn contents(store: &LmdbStore) -> Contents {
    let env = store.env();
    let txn = env.read_txn().unwrap();
    let directory = env
        .open_database::<Bytes, Bytes>(&txn, None)
        .unwrap()
        .unwrap();
    directory
        .iter(&txn)
        .unwrap()
        .map(|entry| {
            let (name, _) = entry.unwrap();
            let name = std::str::from_utf8(name).unwrap();
            let db = env
                .open_database::<Bytes, Bytes>(&txn, Some(name))
                .unwrap()
                .unwrap();
            let entries = db
                .iter(&txn)
                .unwrap()
                .map(|entry| {
                    let (key, value) = entry.unwrap();
                    (key.to_vec(), logical_value(name, key, value))
                })
                .collect();
            (name.to_owned(), entries)
        })
        .collect()
}

fn logical_value(table: &str, key: &[u8], bytes: &[u8]) -> Vec<u8> {
    if table == "meta" && key == b"consti_engine" {
        let engine: burst_consti::ConstiEngine = bincode::deserialize(bytes).unwrap();
        return serde_json::to_vec(&serde_json::to_value(engine).unwrap()).unwrap();
    }
    if table != "meta" || key != crate::endorsement_commit::SNAPSHOT_KEY.as_bytes() {
        return bytes.to_vec();
    }
    // Verification snapshots encode HashMaps and these two HashSets. Compare
    // their contents, not a process's randomized iteration order. Ordered vote
    // and endorsement lists must retain their order.
    let snapshot = burst_verification::OrchestratorSnapshot::decode(bytes).unwrap();
    let mut value = serde_json::to_value(snapshot).unwrap();
    for state in value["states"].as_object_mut().unwrap().values_mut() {
        for field in ["endorser_set", "excluded_verifiers"] {
            state[field]
                .as_array_mut()
                .unwrap()
                .sort_by(|a, b| a.as_str().unwrap().cmp(b.as_str().unwrap()));
        }
    }
    serde_json::to_vec(&value).unwrap()
}

fn open(directory: &Path) -> LmdbStore {
    LmdbStore::open(directory, 32, 32 << 20).unwrap()
}

fn assert_contents(actual: &Contents, expected: &Contents) {
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (name, entries) in expected {
        let observed = &actual[name];
        assert_eq!(observed.len(), entries.len(), "table {name} length");
        for ((key, value), (expected_key, expected_value)) in observed.iter().zip(entries) {
            assert_eq!(key, expected_key, "table {name} key");
            if name == "brn_meta" && key == b"governance_engine" {
                let observed: burst_governance::GovernanceEngine =
                    bincode::deserialize(value).unwrap();
                let expected: burst_governance::GovernanceEngine =
                    bincode::deserialize(expected_value).unwrap();
                assert_eq!(observed, expected, "governance state differs after crash");
                continue;
            }
            assert!(
                value == expected_value,
                "table {name}, key {}: values differ ({} / {} bytes)",
                String::from_utf8_lossy(key),
                value.len(),
                expected_value.len()
            );
        }
    }
}

fn signal_ready() {
    println!("{READY}");
    std::io::stdout().flush().unwrap();
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

#[test]
#[ignore = "subprocess helper for installation crash recovery"]
fn crash_child() {
    let directory = std::env::var("BURST_INSTALLATION_CRASH_DIRECTORY").unwrap();
    let stage = std::env::var("BURST_INSTALLATION_CRASH_STAGE").unwrap();
    let store = open(Path::new(&directory));
    let fixture = Fixture::selection();
    let installation = prepare(&fixture, &store);
    match stage.as_str() {
        "before" => {
            signal_ready();
            park();
        }
        "race" => signal_ready(),
        "after" => (),
        _ => panic!("unknown crash stage"),
    }
    installation.commit(&store).unwrap();
    if stage == "after" {
        signal_ready();
    }
    park();
}

#[tokio::test]
async fn abrupt_installation_exit_recovers_one_complete_projection() {
    let mut fixture = Fixture::selection();
    let (baseline, store, heads) = fixture.store();
    accepted(&fixture, &store, heads);
    let excluded = tail(&mut fixture, &store, BlockType::Burn);
    let block = fixture.blocks.last().unwrap();
    store
        .meta_store()
        .reserve_consensus_vote(
            &fixture.blocks[0].hash,
            &address(0),
            &address(0),
            &block.previous,
            &excluded,
            true,
            0,
        )
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("application-preference", b"keep").unwrap();
    batch
        .put_meta("verification_stake/v2/stale", b"stale")
        .unwrap();
    batch.put_brn_wallet(&address(99), b"stale").unwrap();
    let pending = burst_store::pending::PendingInfo {
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
            &bincode::serialize(&pending).unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    store.env().force_sync().unwrap();
    let before = contents(&store);
    drop(store);

    let expected = tempfile::tempdir().unwrap();
    std::fs::copy(
        baseline.path().join("data.mdb"),
        expected.path().join("data.mdb"),
    )
    .unwrap();
    let store = open(expected.path());
    let installed = prepare(&fixture, &store).commit(&store).unwrap();
    assert_eq!(installed.retired_blocks, vec![excluded]);
    assert_eq!(installed.minority_promises, vec![excluded]);
    assert_eq!(installed.anchor.roster().total_humans(), 9);
    let after = contents(&store);
    assert_ne!(before, after);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    drop(store);

    for (stage, delay_ms) in [
        ("before", 0),
        ("race", 0),
        ("race", 1),
        ("race", 3),
        ("race", 10),
        ("after", 0),
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(
            baseline.path().join("data.mdb"),
            directory.path().join("data.mdb"),
        )
        .unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("BURST_INSTALLATION_CRASH_DIRECTORY", directory.path())
            .env("BURST_INSTALLATION_CRASH_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(line) = output.next_line().await.unwrap() {
                if line.contains(READY) {
                    return;
                }
            }
            panic!("installation child exited before its crash boundary");
        })
        .await
        .expect("installation child did not reach its crash boundary");
        if delay_ms != 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9));

        let store = open(directory.path());
        let recovered = contents(&store);
        let committed = recovered == after;
        assert!(
            recovered == before || committed,
            "mixed projection after {stage}/{delay_ms} ms"
        );
        if stage == "before" {
            assert!(!committed);
        } else if stage == "after" {
            assert!(committed);
        }
        if !committed {
            prepare(&fixture, &store).commit(&store).unwrap();
        }
        assert_contents(&contents(&store), &after);
        assert!(
            PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .is_none()
        );
        eprintln!(
            "{stage}/{delay_ms} ms: recovered {} across {} tables",
            if committed {
                "new projection"
            } else {
                "old projection; replay completed"
            },
            after.len()
        );
    }
}
