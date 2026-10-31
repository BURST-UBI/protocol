use super::*;
use crate::confirmed_replay::{
    scratch_file::ScratchLimit,
    snapshot::history::{DurableHistoryInputs, DurableHistorySession},
};
use burst_store_lmdb::recovery_inputs::InputWriteError;

fn fixture() -> Fixture {
    let mut fixture = Fixture::selection();
    for _ in 0..64 {
        fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    }
    fixture
}

pub(super) async fn child(directory: &Path) -> ! {
    let fixture = fixture();
    let (_dir, store, authenticated) = super::super::super::snapshot(&fixture);
    let first = authenticated.history_cursor(&address(0)).unwrap().unwrap();
    let second = authenticated.history_cursor(&address(1)).unwrap().unwrap();
    let limit = ScratchLimit::acquire_reserved(directory, 4 << 20,
        DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
    let shutdown = crate::shutdown::ShutdownController::new();
    let session = DurableHistorySession::open(directory.to_path_buf(), first.context(),
        1 << 20, limit, &shutdown).await.unwrap().unwrap();
    let mut history = session.select(first.clone()).unwrap();
    let mut other = session.select(second.clone()).unwrap();
    other.append(&super::super::super::read_chunk(&store, second.next().unwrap().hash, 1).unwrap()).unwrap();
    drop(other);
    let inventory = session.select_inventory().unwrap();
    let mut positions = 0u64;
    loop {
        let mut page = vec![4; 16 << 10];
        page[..8].copy_from_slice(&positions.to_be_bytes());
        match inventory.raw.append_checked(positions, &page) {
            Ok(_) => positions += 1,
            Err(InputWriteError::Capacity) => break,
            Err(error) => panic!("unexpected cache fill error: {error}"),
        }
        assert!(positions < 128);
    }
    assert!(positions > 0);
    let chunks = super::super::super::read_chunk(&store, first.next().unwrap().hash, 64).unwrap();
    assert_eq!(chunks.len(), 64);
    assert!(history.append(&chunks).is_err(), "fixture did not reach history pressure");
    assert_eq!(history.cursor().next(), first.next());
    let (history, proofs) = history.append_with_capacity(chunks, &shutdown).await.unwrap();
    assert_eq!(proofs.unwrap().len(), 64);
    assert_eq!(std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap(), "pressure-appended");
    assert!(history.cursor().next().is_some());
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    // Keep the session, history, inventory and their budget alive until SIGKILL.
    loop { std::thread::park(); }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn signed_history_pressure_cleanup_recovers_after_process_kill() {
    let fixture = fixture();
    let (_dir, store, authenticated) = super::super::super::snapshot(&fixture);
    let first = authenticated.history_cursor(&address(0)).unwrap().unwrap();
    let second = authenticated.history_cursor(&address(1)).unwrap().unwrap();
    let chunks = super::super::super::read_chunk(&store, first.next().unwrap().hash, 64).unwrap();
    let other_bytes = super::super::super::read_chunk(&store, second.next().unwrap().hash, 1).unwrap();
    for stage in ["pressure-reset-start", "pressure-reset-page", "pressure-reset-done", "pressure-appended"] {
        let directory = tempfile::tempdir().unwrap();
        kill_at_kind(directory.path(), stage, "pressure-inputs").await;
        assert!(ScratchLimit::acquire(directory.path(), 1).is_err());
        let limit = ScratchLimit::acquire_reserved(directory.path(), 4 << 20,
            DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
        let shutdown = crate::shutdown::ShutdownController::new();
        let session = DurableHistorySession::open(directory.path().to_path_buf(), first.context(),
            1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
        let inventory = session.select_inventory().unwrap();
        assert_eq!(inventory.raw.stream_reset_pending().unwrap(),
            matches!(stage, "pressure-reset-start" | "pressure-reset-page"), "{stage}");
        // Resume the same bounded reset-page API used by inventory preparation.
        // Saved progress is unavailable while its reset marker is pending.
        if inventory.raw.stream_reset_pending().unwrap() {
            assert!(inventory.raw.progress().is_err());
            let mut complete = false;
            for _ in 0..128 {
                if inventory.raw.reset_stream_page(128, (1 << 20) + 64).unwrap().complete {
                    complete = true;
                    break;
                }
            }
            assert!(complete);
        }
        drop(inventory);
        assert_eq!(session.select_inventory().unwrap().raw.progress().unwrap().positions, 0);
        let mut other = session.select(second.clone()).unwrap();
        let proofs = other.replay_page().unwrap();
        assert_eq!(proofs.len(), 1);
        assert_eq!(bincode::serialize(proofs[0].block()).unwrap(), other_bytes[0]);
        assert_eq!(proofs[0].context(), second.context());
        drop(other);
        let mut history = session.select(first.clone()).unwrap();
        assert_eq!(history.cursor().next(), first.next());
        assert!(history.seal().is_err(), "saved progress became authentication at {stage}");
        let proofs = history.replay_page().unwrap();
        assert_eq!(proofs.len(), if stage == "pressure-appended" { 64 } else { 0 });
        for (proof, bytes) in proofs.iter().zip(&chunks) {
            assert_eq!(bincode::serialize(proof.block()).unwrap(), *bytes);
            assert_eq!(proof.context(), first.context());
        }
        while let Some(head) = history.cursor().next() {
            let page = super::super::super::read_chunk(&store, head.hash, 128).unwrap();
            let (next, proofs) = history.append_with_capacity(page, &shutdown).await.unwrap();
            history = next;
            assert!(!proofs.unwrap().is_empty());
        }
        history.seal().unwrap();
        drop(history); drop(session);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        eprintln!("{stage}: SIGKILL, selective reset resume, preserved other account and authenticated history pass");
    }
}
