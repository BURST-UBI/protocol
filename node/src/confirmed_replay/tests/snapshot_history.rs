use super::*;
use crate::checkpoint_store::CertifiedCheckpoint;
use crate::confirmed_replay::snapshot::{
    archive::AuthenticatedSnapshot,
    export::SnapshotExport,
    history::{read_chunk, MAX_WITNESS_BLOCKS, MAX_WITNESS_BYTES},
    SelectionSource,
};

mod archive_selection;
pub(super) mod boundaries;
pub(super) mod cemented;
pub(super) mod compact;
mod compact_export;
mod deadline_signing;
pub(super) mod installed_recovery;
mod planning;
mod proof_equivalence;
mod reconciliation;
mod restoration;
pub(super) mod selection;
mod stored_recovery;
mod witness_transfer;

fn snapshot(fixture: &Fixture) -> (tempfile::TempDir, LmdbStore, AuthenticatedSnapshot) {
    let (dir, store, heads) = fixture.store();
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
    let snapshot = authenticate(&store, &fixture.params, epoch);
    (dir, store, snapshot)
}

pub(super) fn authenticate(
    store: &LmdbStore,
    params: &ProtocolParams,
    epoch: BlockHash,
) -> AuthenticatedSnapshot {
    let export = SnapshotExport::prepare(store, NetworkId::Dev, params, epoch).unwrap();
    let events = crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
    let index = events
        .iter()
        .position(|event| event.certificate().checkpoint.hash() == epoch)
        .unwrap();
    let anchor =
        crate::checkpoint_store::chain::next_anchor(&events[..=index], NetworkId::Dev, params)
            .unwrap();
    let signatures = (0..32)
        .filter(|seed| anchor.roster().weight(&address(*seed)) > 0)
        .map(|seed| {
            export
                .attest(store, &burst_crypto::keypair_from_seed(&[seed; 32]).private)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    export.write(&signatures, &mut bytes).unwrap();
    let genesis =
        crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, params)
            .unwrap();
    AuthenticatedSnapshot::read(&genesis, &mut bytes.as_slice()).unwrap()
}

#[test]
fn snapshot_history_recovers_all_account_prefixes_in_bounded_chunks() {
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let accounts: HashSet<_> = fixture
        .blocks
        .iter()
        .map(|block| block.account.clone())
        .collect();
    let mut total = 0;
    for account in accounts {
        let expected: Vec<_> = fixture
            .blocks
            .iter()
            .rev()
            .filter(|block| block.account == account)
            .collect();
        let mut cursor = snapshot.history_cursor(&account).unwrap().unwrap();
        let context = cursor.context();
        assert_eq!(context.network, NetworkId::Dev);
        assert_eq!(context.genesis, fixture.blocks[0].hash);
        assert_eq!(context.epoch, snapshot.anchor().hash());
        assert_eq!(
            context.state_hash,
            snapshot.certificate().manifest.state_hash
        );
        let mut consumed = 0;
        while let Some(next) = cursor.next() {
            let chunk = read_chunk(&store, next.hash, 2).unwrap();
            assert!(!chunk.is_empty() && chunk.len() <= 2);
            let witnesses = cursor.accept(&chunk).unwrap();
            for witness in witnesses {
                assert_eq!(witness.context(), context);
                assert_eq!(witness.height(), (expected.len() - consumed) as u64);
                assert_eq!(witness.block().hash, expected[consumed].hash);
                assert_eq!(
                    bincode::serialize(witness.block()).unwrap(),
                    bincode::serialize(expected[consumed]).unwrap()
                );
                consumed += 1;
            }
        }
        assert_eq!(consumed, expected.len());
        assert!(cursor
            .accept(&[bincode::serialize(expected[0]).unwrap()])
            .is_err());
        assert!(cursor.next().is_none());
        total += consumed;
    }
    assert_eq!(total, fixture.blocks.len());
    assert!(snapshot.history_cursor(&address(99)).unwrap().is_none());
}

#[test]
fn snapshot_history_rejects_forged_reordered_and_oversized_chunks_atomically() {
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let mut cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let initial = cursor.next().unwrap();
    let valid = read_chunk(&store, initial.hash, 3).unwrap();
    assert_eq!(valid.len(), 3);
    let mut cases = vec![
        vec![],
        vec![vec![0]; MAX_WITNESS_BLOCKS + 1],
        vec![vec![0; burst_types::MAX_P2P_MESSAGE_BYTES + 1]],
        vec![
            vec![0; burst_types::MAX_P2P_MESSAGE_BYTES];
            MAX_WITNESS_BYTES / burst_types::MAX_P2P_MESSAGE_BYTES + 1
        ],
    ];
    let mut reordered = valid.clone();
    reordered.swap(0, 1);
    cases.push(reordered);
    let mut trailing = valid.clone();
    trailing[2].push(0);
    cases.push(trailing);
    let mut truncated = valid.clone();
    truncated[2].pop();
    cases.push(truncated);
    for index in [0, 2] {
        let mut bad = valid.clone();
        let mut block: StateBlock = bincode::deserialize(&bad[index]).unwrap();
        block.signature = Signature([0; 64]);
        bad[index] = bincode::serialize(&block).unwrap();
        cases.push(bad);
    }
    let mut fork = valid.clone();
    let mut block: StateBlock = bincode::deserialize(&fork[0]).unwrap();
    block.representative = address(1);
    sign(0, &mut block); // A valid owner signature is not proof of membership.
    fork[0] = bincode::serialize(&block).unwrap();
    cases.push(fork);
    for bad in cases {
        assert!(cursor.accept(&bad).is_err());
        assert_eq!(cursor.next(), Some(initial.clone()));
    }
    assert_eq!(cursor.accept(&valid).unwrap().len(), 3);
    let progressed = cursor.next();
    assert!(cursor.accept(&valid).is_err());
    assert_eq!(cursor.next(), progressed);
    assert!(read_chunk(&store, BlockHash::new([99; 32]), 1).is_err());
    assert!(read_chunk(&store, initial.hash, MAX_WITNESS_BLOCKS + 1).is_err());
}

#[test]
fn snapshot_history_rejects_oversized_primary_and_archival_rows_before_decode() {
    let fixture = Fixture::selection();
    let (_dir, source, authenticated) = snapshot(&fixture);
    let head = authenticated.history_cursor(&address(0)).unwrap().unwrap().next().unwrap();
    let valid = read_chunk(&source, head.hash, 1).unwrap().remove(0);
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let key = format!("checkpoint_history/v1/{}", head.hash);
    let oversized = vec![0; burst_types::MAX_P2P_MESSAGE_BYTES + 1];
    store.meta_store().put_meta(&key, &oversized).unwrap();
    assert!(read_chunk(&store, head.hash, 1).unwrap_err().to_string().contains("metadata exceeds pinned read byte budget"));
    store.meta_store().put_meta(&key, &valid).unwrap();
    assert_eq!(read_chunk(&store, head.hash, 1).unwrap(), vec![valid.clone()]);
    store.block_store().put_block(&head.hash, &oversized).unwrap();
    assert!(read_chunk(&store, head.hash, 1).unwrap_err().to_string().contains("block exceeds pinned read byte budget"),
        "corrupt primary data must not fall back to an archival copy");
    store.block_store().put_block(&head.hash, &[]).unwrap();
    assert!(read_chunk(&store, head.hash, 1).unwrap_err().to_string().contains("wire limit"));
    store.block_store().put_block(&head.hash, &valid).unwrap();
    assert_eq!(read_chunk(&store, head.hash, 1).unwrap(), vec![valid]);
}

#[test]
fn snapshot_history_continues_across_partial_archival_sources() {
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let mut cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let first = cursor.next().unwrap();
    let bytes = read_chunk(&store, first.hash, 1).unwrap().pop().unwrap();
    let partial_dir = tempfile::tempdir().unwrap();
    let partial = LmdbStore::open(partial_dir.path(), 32, 32 << 20).unwrap();
    partial
        .meta_store()
        .put_meta(&format!("checkpoint_history/v1/{}", first.hash), &bytes)
        .unwrap();
    let prefix = read_chunk(&partial, first.hash, MAX_WITNESS_BLOCKS).unwrap();
    assert_eq!(prefix, vec![bytes]);
    cursor.accept(&prefix).unwrap();
    let next = cursor.next().unwrap();
    assert!(read_chunk(&partial, next.hash, 1).is_err());
    // Continue from verified progress on a different archival source, not from
    // a new peer-selected account head or a complete genesis history download.
    while let Some(next) = cursor.next() {
        let chunk = read_chunk(&store, next.hash, 2).unwrap();
        cursor.accept(&chunk).unwrap();
    }
    assert!(cursor.next().is_none());
}

#[test]
fn snapshot_history_membership_does_not_claim_to_authenticate_work_nonces() {
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let mut cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let next = cursor.next().unwrap();
    let mut block: StateBlock =
        bincode::deserialize(&read_chunk(&store, next.hash, 1).unwrap()[0]).unwrap();
    block.work = block.work.wrapping_add(1);
    assert_eq!(block.compute_hash(), next.hash);
    let accepted = cursor
        .accept(&[bincode::serialize(&block).unwrap()])
        .unwrap();
    assert_eq!(accepted[0].block().hash, next.hash);
    assert_eq!(
        accepted[0].context().state_hash,
        snapshot.certificate().manifest.state_hash
    );
}

pub(super) fn suffix_store(blocks: &[StateBlock]) -> (tempfile::TempDir, LmdbStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    for block in blocks {
        store
            .meta_store()
            .put_meta(
                &format!("checkpoint_history/v1/{}", block.hash),
                &bincode::serialize(block).unwrap(),
            )
            .unwrap();
    }
    (dir, store)
}

#[test]
fn snapshot_history_tail_reads_only_suffix_and_matches_full_history_commitment() {
    let mut fixture = Fixture::selection();
    let (_base_dir, base_store, snapshot) = snapshot(&fixture);
    let start = fixture.blocks.len();
    let cutoff = snapshot.anchor().cutoff() + 10;
    let burn = fixture.push(0, BlockType::Burn, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.link = destination(30);
        block.brn_spent += 1;
    });
    fixture.push(30, BlockType::Open, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.link = burn;
        block.trst_balance = 1;
    });
    fixture.push(1, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.representative = address(2);
    });
    let full = fixture.history(cutoff);
    let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
    let proofs = || compact::retained(&snapshot, &fixture, &base_store, &[]);
    let tail = snapshot
        .read_tail(&tail_store, cutoff, full.heads(), proofs())
        .unwrap();
    assert_eq!(tail.block_count(), 3);
    assert_eq!(tail.commitment(), full.commitment());
    assert_eq!(tail.heads(), full.heads());
    assert_eq!(tail.cutoff(), cutoff);
    assert_eq!(tail.context().epoch, snapshot.anchor().hash());
    assert_eq!(
        tail.context().state_hash,
        snapshot.certificate().manifest.state_hash
    );
    assert_eq!(tail.retained().len(), proofs().len());
    let order: Vec<_> = tail.block_hashes().collect();
    assert!(
        order.iter().position(|hash| *hash == burn).unwrap()
            < order
                .iter()
                .position(|hash| *hash == fixture.blocks[start + 1].hash)
                .unwrap()
    );
    let mut reversed = full.heads().to_vec();
    reversed.reverse();
    let reordered = snapshot
        .read_tail(&tail_store, cutoff, &reversed, proofs())
        .unwrap();
    assert_eq!(reordered.commitment(), tail.commitment());
    assert_eq!(
        bincode::serialize(&reordered.collected_blocks()).unwrap(),
        bincode::serialize(&tail.collected_blocks()).unwrap()
    );
}

#[test]
fn snapshot_history_tail_rejects_rollback_omission_and_bad_suffix_before_replay() {
    let mut fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let original = fixture.history(snapshot.anchor().cutoff());
    let cutoff = snapshot.anchor().cutoff() + 10;
    fixture.push(0, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.representative = address(1);
    });
    let full = fixture.history(cutoff);
    let valid = fixture.blocks.last().unwrap().clone();
    let (_tail_dir, tail_store) = suffix_store(std::slice::from_ref(&valid));
    let proofs = || compact::retained(&snapshot, &fixture, &store, &[]);
    for case in 0..6 {
        let mut heads = full.heads().to_vec();
        match case {
            0 => {
                heads.pop();
            }
            1 => heads.push(heads[0].clone()),
            2 => heads[0].height = 0,
            3 => heads[0].height = u64::MAX,
            4 => heads[0].hash = BlockHash::new([99; 32]),
            _ => {
                let head = heads
                    .iter_mut()
                    .find(|head| head.account == address(0))
                    .unwrap();
                head.height -= 2;
            }
        }
        assert!(
            snapshot
                .read_tail(&tail_store, cutoff, &heads, proofs())
                .is_err(),
            "manifest case {case}"
        );
    }
    assert!(snapshot
        .read_tail(
            &tail_store,
            snapshot.anchor().cutoff() - 1,
            original.heads(),
            proofs()
        )
        .is_err());
    let empty = snapshot
        .read_tail(
            &tail_store,
            snapshot.anchor().cutoff(),
            original.heads(),
            proofs(),
        )
        .unwrap();
    assert_eq!(empty.block_count(), 0);
    assert_eq!(empty.commitment(), original.commitment());
    for case in 0..6 {
        let mut bad = valid.clone();
        match case {
            0 => bad.payment_epoch = fixture.blocks[0].hash,
            1 => bad.timestamp = Timestamp::new(snapshot.anchor().cutoff() - 1),
            2 => bad.timestamp = Timestamp::new(cutoff + 1),
            3 => bad.previous = fixture.blocks[0].hash,
            4 => bad.account = address(99),
            _ => {}
        }
        sign(0, &mut bad);
        if case == 5 {
            bad.signature = Signature([0; 64]);
        }
        let mut heads = full.heads().to_vec();
        heads
            .iter_mut()
            .find(|head| head.account == address(0))
            .unwrap()
            .hash = bad.hash;
        let (_bad_dir, bad_store) = suffix_store(&[bad]);
        assert!(
            snapshot
                .read_tail(&bad_store, cutoff, &heads, proofs())
                .is_err(),
            "suffix case {case}"
        );
    }
    assert_eq!(
        snapshot
            .read_tail(&tail_store, cutoff, full.heads(), proofs())
            .unwrap()
            .block_count(),
        1
    );
}

#[test]
fn snapshot_history_tail_requires_snapshot_bound_witness_for_old_cross_account_source() {
    let mut fixture = Fixture::selection();
    let burn = fixture.push(0, BlockType::Burn, 1_595_434_040, |block| {
        block.link = destination(30);
        block.brn_spent += 1;
    });
    fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_dir, store, snapshot) = snapshot(&fixture);
    let cutoff = snapshot.anchor().cutoff() + 10;
    fixture.push(30, BlockType::Open, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.link = burn;
        block.trst_balance = 1;
    });
    let full = fixture.history(cutoff);
    let (_tail_dir, tail_store) =
        suffix_store(std::slice::from_ref(fixture.blocks.last().unwrap()));
    let proofs = || compact::retained(&snapshot, &fixture, &store, &[burn]);
    let mut missing = proofs();
    missing.retain(|proof| proof.block().hash != burn);
    assert!(snapshot
        .read_tail(&tail_store, cutoff, full.heads(), missing)
        .is_err());
    let recover = || {
        let mut cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
        cursor
            .accept(&read_chunk(&store, cursor.next().unwrap().hash, 2).unwrap())
            .unwrap()
            .into_iter()
            .find(|witness| witness.block().hash == burn)
            .unwrap()
    };
    let tail = snapshot
        .read_tail(&tail_store, cutoff, full.heads(), proofs())
        .unwrap();
    assert_eq!(tail.block_count(), 1);
    assert_eq!(tail.retained().len(), proofs().len());
    assert_eq!(tail.commitment(), full.commitment());
    let mut duplicate = proofs();
    duplicate.push(recover());
    assert!(snapshot
        .read_tail(&tail_store, cutoff, full.heads(), duplicate)
        .is_err());
    let other_fixture = Fixture::selection();
    let (_other_dir, other_store, other) = self::snapshot(&other_fixture);
    let mut cursor = other.history_cursor(&address(0)).unwrap().unwrap();
    let mut wrong = cursor
        .accept(&read_chunk(&other_store, cursor.next().unwrap().hash, 1).unwrap())
        .unwrap();
    wrong.extend(proofs());
    assert!(snapshot
        .read_tail(&tail_store, cutoff, full.heads(), wrong)
        .is_err());
}

#[test]
fn durable_history_inputs_reauthenticate_reopened_progress_under_combined_budget() {
    use crate::confirmed_replay::snapshot::history::DurableHistoryInputs;
    use crate::confirmed_replay::scratch_file::{ScratchFile, ScratchLimit};
    use std::io::Write;
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let initial = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let head = initial.next().unwrap();
    assert!(head.height >= 2);
    let floor = head.height - 2;
    let cache = tempfile::tempdir().unwrap();
    let reservation = DurableHistoryInputs::reservation(1 << 20).unwrap();
    let owner = ScratchLimit::acquire_reserved(cache.path(), 4 << 20, reservation).unwrap();
    let foreign = tempfile::tempdir().unwrap();
    assert!(DurableHistoryInputs::open_with_limit(foreign.path(), initial.clone(), floor, 1 << 20, owner.clone()).is_err());
    assert!(DurableHistoryInputs::open_with_limit(cache.path(), initial.clone(), floor, 2 << 20, owner.clone()).is_err());
    assert!(!cache.path().join("recovery-inputs-v2").exists());
    let mut durable = DurableHistoryInputs::open_with_limit(cache.path(), initial.clone(), floor, 1 << 20, owner.clone()).unwrap();
    drop(owner);
    assert!(ScratchLimit::acquire(cache.path(), 4 << 20).is_err());
    let first = read_chunk(&store, head.hash, 1).unwrap();
    let proof = durable.append(&first).unwrap();
    assert_eq!(proof[0].block().hash, head.hash);
    assert!(durable.seal().is_err());
    let mut scratch = ScratchFile::new_in(cache.path()).unwrap();
    let remaining = (4 << 20) - (1 << 20) - (64 << 10);
    scratch.write_all(&vec![0;remaining]).unwrap();
    assert!(scratch.write_all(&[0]).is_err());
    drop(scratch);
    drop(durable);
    // Even without an active cache handle, its retained files count against
    // ordinary scratch policy. The data environment alone exceeds one byte.
    assert!(ScratchLimit::acquire(cache.path(), 1).is_err());
    let mut durable = DurableHistoryInputs::open(cache.path(), initial.clone(), floor, 4 << 20, 1 << 20).unwrap();
    assert_eq!(durable.cursor().next().unwrap().hash, head.hash);
    assert!(durable.append(&first).is_err(), "saved bytes must be reauthenticated first");
    let replayed = durable.replay_page().unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].context(), initial.context());
    assert_eq!(replayed[0].block().hash, head.hash);
    assert!(durable.replay_page().unwrap().is_empty());
    let second_hash = durable.cursor().next().unwrap().hash;
    durable.append(&read_chunk(&store, second_hash, 1).unwrap()).unwrap();
    durable.seal().unwrap();
    drop(durable);
    let mut durable = DurableHistoryInputs::open(cache.path(), initial, floor, 4 << 20, 1 << 20).unwrap();
    assert!(durable.seal().is_err(), "a saved completion marker is not authenticated progress");
    let proofs = durable.replay_page().unwrap();
    assert_eq!(proofs.iter().map(|proof| proof.block().hash).collect::<Vec<_>>(), vec![head.hash, second_hash]);
    durable.seal().unwrap();
}

#[test]
fn durable_history_rejects_validly_hashed_raw_forgery_without_advancing_cursor() {
    use crate::confirmed_replay::snapshot::history::DurableHistoryInputs;
    use crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY;
    use burst_store_lmdb::recovery_inputs::{RecoveryInputs, Limits};
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let initial = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let head = initial.next().unwrap();
    assert!(head.height > 1);
    let floor = head.height - 1;
    let bytes = read_chunk(&store, head.hash, 1).unwrap();
    let mut forged: StateBlock = bincode::deserialize(&bytes[0]).unwrap();
    forged.signature = burst_types::Signature([0;64]);
    let forged = bincode::serialize(&forged).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let context = DurableHistoryInputs::input_context(&initial, floor).unwrap();
    let raw = RecoveryInputs::open(&directory.path().join(PERSISTENT_INPUTS_DIRECTORY), context,
        Limits { chunks: 131_072, bytes: 1 << 20, map_bytes: 1 << 20 }).unwrap();
    raw.append(0, &forged).unwrap();
    raw.seal(1).unwrap();
    drop(raw);
    let mut durable = DurableHistoryInputs::open(directory.path(), initial, floor, 4 << 20, 1 << 20).unwrap();
    assert!(durable.replay_page().is_err());
    assert_eq!(durable.cursor().next().unwrap().hash, head.hash);
    assert!(durable.seal().is_err());
}

#[test]
fn durable_history_streams_reopen_independent_accounts_under_one_budget() {
    use crate::confirmed_replay::snapshot::history::DurableHistoryInputs;
    use crate::confirmed_replay::scratch_file::{ScratchFile, ScratchLimit};
    use std::io::Write;
    let fixture = Fixture::selection();
    let (_dir, store, authenticated) = snapshot(&fixture);
    let first = authenticated.history_cursor(&address(0)).unwrap().unwrap();
    let second = authenticated.history_cursor(&address(1)).unwrap().unwrap();
    let first_head = first.next().unwrap();
    let second_head = second.next().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut root = DurableHistoryInputs::open(cache.path(), first.clone(), first_head.height - 1, 4 << 20, 1 << 20).unwrap();
    let mut other = root.select_stream(second.clone(), second_head.height - 1).unwrap();
    root.append(&read_chunk(&store, first_head.hash, 1).unwrap()).unwrap();
    root.seal().unwrap();
    other.append(&read_chunk(&store, second_head.hash, 1).unwrap()).unwrap();
    other.seal().unwrap();
    drop(root);
    assert!(ScratchLimit::acquire(cache.path(), 4 << 20).is_err());
    let mut scratch = ScratchFile::new_in(cache.path()).unwrap();
    scratch.write_all(&vec![0; (4 << 20) - (1 << 20) - (64 << 10)]).unwrap();
    assert!(scratch.write_all(&[0]).is_err());
    drop(scratch); drop(other);
    let mut root = DurableHistoryInputs::open(cache.path(), first.clone(), first_head.height - 1, 4 << 20, 1 << 20).unwrap();
    let mut other = root.select_stream(second, second_head.height - 1).unwrap();
    assert!(root.seal().is_err());
    assert!(other.seal().is_err());
    assert_eq!(root.replay_page().unwrap()[0].block().hash, first_head.hash);
    assert_eq!(other.replay_page().unwrap()[0].block().hash, second_head.hash);
    root.seal().unwrap(); other.seal().unwrap();
    let mut changed = Fixture::selection();
    changed.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_changed_dir, _changed_store, changed_snapshot) = snapshot(&changed);
    let foreign = changed_snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let floor = foreign.next().unwrap().height - 1;
    assert!(root.select_stream(foreign, floor).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn durable_history_session_uses_epoch_identity_and_resets_obsolete_inputs() {
    use crate::confirmed_replay::snapshot::history::{DurableHistoryInputs, DurableHistorySession};
    use crate::confirmed_replay::scratch_file::{ScratchLimit, PERSISTENT_INPUTS_DIRECTORY};
    use burst_store_lmdb::recovery_inputs::{Limits, RecoveryInputs};
    let fixture = Fixture::selection();
    let (_dir, store, authenticated) = snapshot(&fixture);
    let first = authenticated.history_cursor(&address(0)).unwrap().unwrap();
    let context = first.context();
    let cache = tempfile::tempdir().unwrap();
    let limit = ScratchLimit::acquire_reserved(cache.path(), 4 << 20, DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
    let shutdown = crate::shutdown::ShutdownController::new();
    let session = DurableHistorySession::open(cache.path().to_path_buf(), context, 1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
    let mut stream = session.select(first.clone()).unwrap();
    stream.append(&read_chunk(&store, first.next().unwrap().hash, 1).unwrap()).unwrap();
    drop(stream); drop(session);
    // A different first account still selects the same epoch-bound session.
    let second = authenticated.history_cursor(&address(1)).unwrap().unwrap();
    let session = DurableHistorySession::open(cache.path().to_path_buf(), second.context(), 1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
    let mut stream = session.select(first.clone()).unwrap();
    assert_eq!(stream.replay_page().unwrap().len(), 1);
    drop(stream); drop(session);
    let raw = RecoveryInputs::open_retained(&cache.path().join(PERSISTENT_INPUTS_DIRECTORY), [0;32],
        Limits { chunks: 131_072, bytes: 1 << 20, map_bytes: 1 << 20 }).unwrap();
    raw.begin_reset([7;32]).unwrap();
    drop(raw);
    // Finish a previously recorded reset target, then establish the currently
    // authenticated context; the recorded target supplies no authority.
    let session = DurableHistorySession::open(cache.path().to_path_buf(), context, 1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
    assert!(session.select(first.clone()).unwrap().replay_page().unwrap().is_empty());
    drop(session);
    let mut changed = Fixture::selection();
    changed.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_changed_dir, _changed_store, changed_snapshot) = snapshot(&changed);
    let foreign = changed_snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let session = DurableHistorySession::open(cache.path().to_path_buf(), foreign.context(), 1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
    assert!(session.select(first).is_err());
    assert!(session.select(foreign).unwrap().replay_page().unwrap().is_empty());
    drop(session);
    shutdown.shutdown();
    assert!(DurableHistorySession::open(cache.path().to_path_buf(), context, 1 << 20, limit, &shutdown).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn signed_history_capacity_reclaims_inventory_and_preserves_other_accounts() {
    use crate::confirmed_replay::snapshot::history::{DurableHistoryInputs, DurableHistorySession};
    use crate::confirmed_replay::scratch_file::ScratchLimit;
    use burst_store_lmdb::recovery_inputs::InputWriteError;
    let mut fixture = Fixture::selection();
    for _ in 0..64 {
        fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    }
    let (_dir, store, authenticated) = snapshot(&fixture);
    let first = authenticated.history_cursor(&address(0)).unwrap().unwrap();
    let second = authenticated.history_cursor(&address(1)).unwrap().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let limit = ScratchLimit::acquire_reserved(cache.path(), 4 << 20,
        DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
    let shutdown = crate::shutdown::ShutdownController::new();
    let session = DurableHistorySession::open(cache.path().to_path_buf(), first.context(),
        1 << 20, limit.clone(), &shutdown).await.unwrap().unwrap();
    let mut history = session.select(first.clone()).unwrap();
    let mut other = session.select(second.clone()).unwrap();
    let other_bytes = read_chunk(&store, second.next().unwrap().hash, 1).unwrap();
    other.append(&other_bytes).unwrap();
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
    let chunks = read_chunk(&store, first.next().unwrap().hash, 64).unwrap();
    assert_eq!(chunks.len(), 64);
    assert!(history.append(&chunks).is_err(), "fixture did not reach history pressure");
    assert_eq!(history.cursor().next(), first.next());
    let (mut history, proofs) = history.append_with_capacity(chunks.clone(), &shutdown).await.unwrap();
    let proofs = proofs.unwrap();
    assert_eq!(proofs.len(), 64);
    for (proof, bytes) in proofs.iter().zip(&chunks) {
        assert_eq!(bincode::serialize(proof.block()).unwrap(), *bytes);
        assert_eq!(proof.context(), first.context());
    }
    assert!(inventory.raw.progress().is_err(), "reclaimed inventory owner remained live");
    assert_eq!(session.select_inventory().unwrap().raw.progress().unwrap().positions, 0);
    let mut other = session.select(second).unwrap();
    let replayed = other.replay_page().unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(bincode::serialize(replayed[0].block()).unwrap(), other_bytes[0]);
    while let Some(head) = history.cursor().next() {
        let chunks = read_chunk(&store, head.hash, 128).unwrap();
        let (next, proofs) = history.append_with_capacity(chunks, &shutdown).await.unwrap();
        history = next;
        assert!(!proofs.unwrap().is_empty());
    }
    history.seal().unwrap();
    drop(history); drop(other); drop(inventory); drop(session); drop(limit);
    let limit = ScratchLimit::acquire_reserved(cache.path(), 4 << 20,
        DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
    let session = DurableHistorySession::open(cache.path().to_path_buf(), first.context(),
        1 << 20, limit, &shutdown).await.unwrap().unwrap();
    let mut history = session.select(first.clone()).unwrap();
    assert_eq!(history.cursor().next(), first.next());
    assert!(history.seal().is_err(), "saved completion is not authentication");
    let mut count = 0;
    while history.cursor().next().is_some() { count += history.replay_page().unwrap().len(); }
    assert_eq!(count as u64, first.next().unwrap().height);
    history.seal().unwrap();
}
