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
    let (_base_dir, _base_store, snapshot) = snapshot(&fixture);
    let start = fixture.blocks.len();
    let cutoff = snapshot.anchor().cutoff() + 10;
    let burn = fixture.push(0, BlockType::Burn, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.link = destination(30);
        block.brn_balance += 1;
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
    let tail = snapshot
        .read_tail(&tail_store, cutoff, full.heads(), vec![])
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
    assert!(tail.retained().is_empty());
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
        .read_tail(&tail_store, cutoff, &reversed, vec![])
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
    let (_dir, _store, snapshot) = snapshot(&fixture);
    let original = fixture.history(snapshot.anchor().cutoff());
    let cutoff = snapshot.anchor().cutoff() + 10;
    fixture.push(0, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.representative = address(1);
    });
    let full = fixture.history(cutoff);
    let valid = fixture.blocks.last().unwrap().clone();
    let (_tail_dir, tail_store) = suffix_store(std::slice::from_ref(&valid));
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
                .read_tail(&tail_store, cutoff, &heads, vec![])
                .is_err(),
            "manifest case {case}"
        );
    }
    assert!(snapshot
        .read_tail(
            &tail_store,
            snapshot.anchor().cutoff() - 1,
            original.heads(),
            vec![]
        )
        .is_err());
    let empty = snapshot
        .read_tail(
            &tail_store,
            snapshot.anchor().cutoff(),
            original.heads(),
            vec![],
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
                .read_tail(&bad_store, cutoff, &heads, vec![])
                .is_err(),
            "suffix case {case}"
        );
    }
    assert_eq!(
        snapshot
            .read_tail(&tail_store, cutoff, full.heads(), vec![])
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
        block.brn_balance += 1;
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
    assert!(snapshot
        .read_tail(&tail_store, cutoff, full.heads(), vec![])
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
        .read_tail(&tail_store, cutoff, full.heads(), vec![recover()])
        .unwrap();
    assert_eq!(tail.block_count(), 1);
    assert_eq!(tail.retained().len(), 1);
    assert_eq!(tail.commitment(), full.commitment());
    assert!(snapshot
        .read_tail(
            &tail_store,
            cutoff,
            full.heads(),
            vec![recover(), recover()]
        )
        .is_err());
    let other_fixture = Fixture::selection();
    let (_other_dir, other_store, other) = self::snapshot(&other_fixture);
    let mut cursor = other.history_cursor(&address(0)).unwrap().unwrap();
    let wrong = cursor
        .accept(&read_chunk(&other_store, cursor.next().unwrap().hash, 1).unwrap())
        .unwrap();
    assert!(snapshot
        .read_tail(&tail_store, cutoff, full.heads(), wrong)
        .is_err());
}
