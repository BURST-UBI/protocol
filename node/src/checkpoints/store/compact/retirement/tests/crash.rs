use super::*;
use std::{io::Write, os::unix::process::ExitStatusExt, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

const CHILD: &str =
    "checkpoints::store::compact::retirement::tests::crash::retirement_storage_crash_child";
const READY: &str = "BURST_RETIREMENT_STORAGE_READY";

fn pause(stage: &str) {
    if stage != std::env::var("BURST_RETIREMENT_STORAGE_STAGE").unwrap() {
        return;
    }
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
#[ignore = "subprocess helper for retirement transaction crashes"]
fn retirement_storage_crash_child() {
    let directory = std::env::var("BURST_RETIREMENT_STORAGE_DIRECTORY").unwrap();
    let store = LmdbStore::open(std::path::Path::new(&directory), 32, 256 * MIB).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    make_room(&mut batch, &anchor(), 13, 16 * MIB).unwrap();
    pause("retired");
    stage_successor(&mut batch);
    pause("staged");
    batch.commit().unwrap();
    store.force_sync().unwrap();
    pause("committed");
    panic!("retirement crash boundary was not reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn byte_pressure_retirement_recovers_old_or_new_state_after_process_kill() {
    for stage in ["retired", "staged", "committed"] {
        let directory = tempfile::tempdir().unwrap();
        seed(directory.path());
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("BURST_RETIREMENT_STORAGE_DIRECTORY", directory.path())
            .env("BURST_RETIREMENT_STORAGE_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let reached = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.contains(READY) {
                    return true;
                }
            }
            false
        })
        .await;
        if !matches!(reached, Ok(true)) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            panic!("retirement did not reach {stage}: {reached:?}");
        }
        child.kill().await.unwrap();
        assert_eq!(child.wait().await.unwrap().signal(), Some(9));
        let store = LmdbStore::open(directory.path(), 32, 256 * MIB).unwrap();
        verify(&store, stage == "committed");
    }
}
