use super::*;
use crate::checkpoint_sync::{
    witness::{Command, Roots, Transfer},
    CheckpointSyncMessage as Message, Incoming,
};
use crate::wire_message::WireMessage;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

fn wire(peer: &str, message: Message) -> Incoming {
    let bytes = WireMessage::CheckpointSync(message).encode().unwrap();
    assert!(bytes.len() <= burst_types::MAX_P2P_MESSAGE_BYTES);
    let WireMessage::CheckpointSync(message) = WireMessage::decode(&bytes).unwrap() else {
        panic!("wrong wire message")
    };
    Incoming {
        peer: peer.into(),
        message,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn consumer_history_lookup_proves_membership_without_installing_blocks() {
    use burst_store::meta::MetaStore;
    use std::sync::Arc;
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let export = SnapshotExport::prepare(
        &source, NetworkId::Dev, &fixture.params, snapshot.anchor().hash(),
    ).unwrap();
    let signatures = (0..32)
        .filter(|seed| snapshot.anchor().roster().weight(&address(*seed)) > 0)
        .map(|seed| export.attest(&source, &burst_crypto::keypair_from_seed(&[seed; 32]).private).unwrap())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    export.write(&signatures, &mut bytes).unwrap();
    let (_local_dir, local) = suffix_store(&[]);
    local.meta_store().put_meta(
        crate::confirmed_replay::snapshot::installed::BASE_KEY, &bytes,
    ).unwrap();
    let before = local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
    let local = Arc::new(local);
    let service = super::installed_recovery::Service::start(Arc::new(source), local.clone(), 0).await;
    let mut reader = crate::historical_lookup::Reader {
        store: local.clone(), registry: service.registry.clone(), network: NetworkId::Dev,
        initial_params: fixture.params.clone(), active: Arc::new(tokio::sync::Mutex::new(())),
        admitted: Arc::new(tokio::sync::Semaphore::new(3)),
    };
    let expected = &fixture.blocks[1];
    let recovered = reader.read(expected.hash).await.unwrap().unwrap();
    assert_eq!(bincode::serialize(&recovered).unwrap(), bincode::serialize(expected).unwrap());
    let head = snapshot.history_cursor(&expected.account).unwrap().unwrap().next().unwrap();
    let expected_chain: Vec<_> = fixture.blocks.iter()
        .filter(|block| block.account == expected.account)
        .take(head.height as usize).collect();
    assert_eq!(expected_chain.len() as u64, head.height);
    for (offset, block) in expected_chain.iter().enumerate() {
        let page = reader.account_page(expected.account.clone(), offset as u64 + 1, 1).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, offset as u64 + 1);
        assert_eq!(bincode::serialize(&page[0].1).unwrap(), bincode::serialize(block).unwrap());
    }
    let page = reader.account_page(expected.account.clone(), 1, 1000).await.unwrap();
    assert_eq!(page.len(), expected_chain.len());
    assert!(page.windows(2).all(|pair| pair[0].0 + 1 == pair[1].0 && pair[1].1.previous == pair[0].1.hash));
    assert!(reader.account_page(expected.account.clone(), head.height + 1, 1).await.unwrap().is_empty());
    assert!(reader.account_page(expected.account.clone(), 0, 1).await.is_err());
    assert!(reader.account_page(expected.account.clone(), 1, 1001).await.is_err());
    reader.network = NetworkId::Test;
    assert!(reader.read(expected.hash).await.is_err());
    assert!(reader.account_page(expected.account.clone(), 1, 1).await.is_err());
    reader.network = NetworkId::Dev;
    let (recovered, page) = tokio::join!(
        reader.read(expected.hash), reader.account_page(expected.account.clone(), 1, 1),
    );
    assert_eq!(recovered.unwrap().unwrap().hash, expected.hash);
    assert_eq!(page.unwrap()[0].1.hash, expected_chain[0].hash);
    // Cancelled waiters must return their admission slot without starting work.
    let active = reader.active.lock().await;
    assert!(tokio::time::timeout(Duration::from_millis(20), reader.read(expected.hash)).await.is_err());
    assert_eq!(reader.admitted.available_permits(), 3);
    drop(active);
    let guard = reader.admitted.acquire_many(3).await.unwrap();
    assert!(reader.read(expected.hash).await.unwrap_err().contains("busy"));
    assert!(reader.account_page(expected.account.clone(), 1, 1).await.err().unwrap().contains("busy"));
    drop(guard);
    local.meta_store().put_meta(
        crate::confirmed_replay::snapshot::installed::BASE_KEY, b"corrupt",
    ).unwrap();
    assert!(reader.read(expected.hash).await.is_err());
    assert!(reader.account_page(expected.account.clone(), 1, 1).await.is_err());
    local.meta_store().put_meta(
        crate::confirmed_replay::snapshot::installed::BASE_KEY, &bytes,
    ).unwrap();
    assert_eq!(local.block_store().block_count().unwrap(), 0);
    assert_eq!(local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), before);
    drop(reader);
    assert!(service.stop().await > 0);
}

#[test]
fn exact_witness_batch_recovers_independent_heads_without_installing_them() {
    let mut fixture = Fixture::selection();
    for seed in 40..104 {
        fixture.push(seed, BlockType::Open, 1_595_434_000, |_| {});
    }
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let heads = snapshot
        .confirmed_heads()
        .into_iter()
        .take(64)
        .collect::<Vec<_>>();
    assert_eq!(heads.len(), 64);
    let cursors: Vec<_> = heads
        .iter()
        .map(|head| snapshot.history_cursor(&head.account).unwrap().unwrap())
        .collect();
    let wanted: Vec<_> = heads.iter().map(|head| head.hash).collect();
    let context = cursors[0].context();
    let proofs = exchange(Roots::Exact(cursors), wanted.clone(), &source, &local).unwrap();
    assert_eq!(proofs.len(), heads.len());
    let expected: HashMap<_, _> = heads.iter().map(|head| (head.hash, head)).collect();
    for proof in proofs {
        let head = expected[&proof.block().hash];
        assert_eq!(proof.height(), head.height);
        assert_eq!(proof.block().account, head.account);
        assert_eq!(proof.context(), context);
        assert_eq!(
            bincode::serialize(proof.block()).unwrap(),
            source.block_store().get_block(&head.hash).unwrap()
        );
    }
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn exact_witness_batch_splits_committed_inventory_into_bounded_requests() {
    let mut fixture = Fixture::selection();
    for seed in 40..200 {
        fixture.push(seed, BlockType::Open, 1_595_434_000, |_| {});
    }
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let heads: Vec<_> = snapshot.confirmed_heads().into_iter().take(129).collect();
    assert_eq!(heads.len(), 129);
    let cursors: Vec<_> = heads
        .iter()
        .map(|head| snapshot.history_cursor(&head.account).unwrap().unwrap())
        .collect();
    let wanted: Vec<_> = heads.iter().map(|head| head.hash).collect();
    let local = std::sync::Arc::new(local);
    let service =
        super::installed_recovery::Service::start(std::sync::Arc::new(source), local.clone(), 0)
            .await;
    let proofs = crate::checkpoint_sync::witness::request_with_committed(
        &service.registry,
        Roots::Plan(cursors.clone()),
        wanted.clone(),
        cursors,
    )
    .await
    .unwrap();
    assert_eq!(service.stop().await, 3);
    assert_eq!(proofs.len(), wanted.len());
    assert_eq!(
        proofs
            .iter()
            .map(|proof| proof.block().hash)
            .collect::<HashSet<_>>(),
        wanted.into_iter().collect()
    );
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn exact_witness_batch_resumes_a_prefix_and_distinct_positions_of_one_account() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let head = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let mut older = head.clone();
    older
        .accept(&read_chunk(&source, head.next().unwrap().hash, 1).unwrap())
        .unwrap();
    let cursors = vec![head, older];
    let mut wanted: Vec<_> = cursors.iter().map(|c| c.next().unwrap().hash).collect();
    wanted.sort_by_key(|hash| *hash.as_bytes());
    let now = Instant::now();
    let (result, mut received) = oneshot::channel();
    let mut client = Transfer::new(now);
    let mut server = Transfer::new(now);
    client.start(
        Command {
            roots: Roots::Exact(cursors),
            wanted: wanted.clone(),
            result,
        },
        now,
    );
    let request = client.poll(vec!["source".into()], now).unwrap();
    let mut response = server
        .handle(&source, wire("client", request.message), now)
        .unwrap()
        .unwrap();
    let Message::WitnessChunk {
        data,
        total,
        digest,
        ..
    } = &mut response.message
    else {
        panic!("expected exact witness bytes");
    };
    assert_eq!(data.len(), *total as usize);
    assert_eq!(u32::from_be_bytes(data[..4].try_into().unwrap()), 2);
    let first_length = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    data.truncate(8 + first_length);
    data[..4].copy_from_slice(&1u32.to_be_bytes());
    *total = data.len() as u32;
    *digest = BlockHash::new(burst_crypto::blake2b_256(data));
    let next = client
        .handle(&local, wire("source", response.message), now)
        .unwrap()
        .unwrap();
    assert!(
        matches!(&next.message, Message::WitnessHeadsRequest { starts, offset: 0, digest: None } if starts == &wanted[1..])
    );
    assert!(matches!(
        received.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let response = server
        .handle(&source, wire("client", next.message), now)
        .unwrap()
        .unwrap();
    assert!(client
        .handle(&local, wire("source", response.message), now)
        .unwrap()
        .is_none());
    let proofs = received.try_recv().unwrap().unwrap();
    assert_eq!(
        proofs.iter().map(|p| p.block().hash).collect::<Vec<_>>(),
        wanted
    );
    assert_ne!(proofs[0].height(), proofs[1].height());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn exact_witness_batch_rejects_a_late_bad_signature_before_returning_any_proofs() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let heads: Vec<_> = snapshot.confirmed_heads().into_iter().take(4).collect();
    let cursors = heads
        .iter()
        .map(|head| snapshot.history_cursor(&head.account).unwrap().unwrap())
        .collect();
    let now = Instant::now();
    let (result, mut received) = oneshot::channel();
    let mut client = Transfer::new(now);
    let mut server = Transfer::new(now);
    client.start(
        Command {
            roots: Roots::Exact(cursors),
            wanted: heads.iter().map(|head| head.hash).collect(),
            result,
        },
        now,
    );
    let request = client.poll(vec!["source".into()], now).unwrap();
    assert!(matches!(
        request.message,
        Message::WitnessHeadsRequest { .. }
    ));
    let response = server
        .handle(&source, wire("client", request.message), now)
        .unwrap()
        .unwrap();
    let mut bad = response.message.clone();
    let Message::WitnessChunk {
        data,
        total,
        digest,
        ..
    } = &mut bad
    else {
        panic!("expected exact witness bytes");
    };
    assert_eq!(data.len(), *total as usize);
    let count = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
    let mut offset = 4;
    for index in 0..count {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if index + 1 == count {
            let mut block: StateBlock =
                bincode::deserialize(&data[offset..offset + length]).unwrap();
            block.signature.0[0] ^= 1;
            let bytes = bincode::serialize(&block).unwrap();
            assert_eq!(bytes.len(), length);
            data[offset..offset + length].copy_from_slice(&bytes);
        }
        offset += length;
    }
    *digest = BlockHash::new(burst_crypto::blake2b_256(data));
    assert!(client.handle(&local, wire("source", bad), now).is_err());
    assert!(matches!(
        received.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let retry = client.poll(vec!["source".into()], now).unwrap();
    let good = server
        .handle(&source, wire("client", retry.message), now)
        .unwrap()
        .unwrap();
    assert!(client
        .handle(&local, wire("source", good.message), now)
        .unwrap()
        .is_none());
    assert_eq!(received.try_recv().unwrap().unwrap().len(), count);
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn suffix_resume_requires_exact_authenticated_positions_atomically() {
    let fixture = Fixture::selection();
    let (_directory, store, snapshot) = snapshot(&fixture);
    let mut cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let initial = cursor.next().unwrap();
    let mut reader = cursor.clone();
    let proofs = reader
        .accept(&read_chunk(&store, initial.hash, 3).unwrap())
        .unwrap();
    assert_eq!(proofs.len(), 3);
    let mut reversed = proofs.clone();
    reversed.swap(1, 2);
    let mut duplicate = proofs.clone();
    duplicate[2] = duplicate[1].clone();
    let mut other_account = snapshot.history_cursor(&address(1)).unwrap().unwrap();
    let other_head = other_account.next().unwrap();
    let other_proofs = other_account
        .accept(&read_chunk(&store, other_head.hash, 1).unwrap())
        .unwrap();
    let (_other_directory, other_store, other_snapshot) = snapshot_with_extra_head();
    let mut other_context = other_snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let other_head = other_context.next().unwrap();
    let other_context_proofs = other_context
        .accept(&read_chunk(&other_store, other_head.hash, 1).unwrap())
        .unwrap();
    for invalid in [
        vec![],
        reversed,
        duplicate,
        other_proofs,
        other_context_proofs,
        vec![proofs[0].clone(); MAX_WITNESS_BLOCKS + 1],
    ] {
        assert!(cursor.resume_suffix(invalid).is_err());
        assert_eq!(cursor.next(), Some(initial.clone()));
    }
    let expected: Vec<_> = proofs
        .iter()
        .map(|proof| bincode::serialize(proof.block()).unwrap())
        .collect();
    let blocks = cursor.resume_suffix(proofs.clone()).unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| bincode::serialize(block).unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(cursor.next(), reader.next());
    let resumed = cursor.next();
    assert!(cursor.resume_suffix(proofs).is_err());
    assert_eq!(cursor.next(), resumed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn suffix_fetch_resumes_bounded_batches_without_installing_blocks() {
    let mut fixture = Fixture::selection();
    for _ in 0..8_200 {
        fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    }
    let (_directory, source, mut heads) = fixture.store_with_map_size(128 << 20);
    heads.sort_by(|left, right| left.account.as_str().cmp(right.account.as_str()));
    let candidate = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        1_595_434_050,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
    let parent = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let mut cursor = crate::confirmed_replay::snapshot::history::HistoryCursor::certified_cut(
        &parent,
        &certificate,
        &heads,
    )
    .unwrap()
    .into_iter()
    .find(|cursor| cursor.next().unwrap().account == address(0))
    .unwrap();
    let (_local_directory, local) = suffix_store(&[]);
    let local = std::sync::Arc::new(local);
    let service =
        super::installed_recovery::Service::start(std::sync::Arc::new(source), local.clone(), 0)
            .await;
    let head_cursors = crate::confirmed_replay::snapshot::history::HistoryCursor::certified_cut(
        &parent,
        &certificate,
        &heads,
    )
    .unwrap();
    let expected_heads: std::collections::HashSet<_> = head_cursors
        .iter()
        .map(|cursor| cursor.next().unwrap().hash)
        .collect();
    assert!(expected_heads.len() > 1);
    let proofs = crate::checkpoint_sync::witness::fetch_exact_heads(
        &service.registry,
        head_cursors.clone(),
    )
    .await
    .unwrap();
    assert_eq!(proofs.len(), expected_heads.len());
    assert_eq!(
        proofs.iter().map(|proof| proof.block().hash).collect::<std::collections::HashSet<_>>(),
        expected_heads,
    );
    assert!(crate::checkpoint_sync::witness::fetch_exact_heads(
        &service.registry,
        vec![head_cursors[0].clone(), head_cursors[0].clone()],
    )
    .await
    .is_err());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
    let initial = cursor.next().unwrap();
    assert!(initial.height > 8_192, "cross the former suffix ceiling");
    assert!(crate::checkpoint_sync::witness::fetch_suffix(
        &service.registry,
        &mut cursor,
        initial.height
    )
    .await
    .is_err());
    assert_eq!(cursor.next(), Some(initial));
    let mut actual = Vec::new();
    let mut batches = 0;
    while cursor.next().is_some_and(|head| head.height > 1) {
        let remaining = cursor.next().unwrap().height - 1;
        let batch =
            crate::checkpoint_sync::witness::fetch_suffix(&service.registry, &mut cursor, 1)
                .await
                .unwrap();
        assert_eq!(batch.len(), (remaining as usize).min(MAX_WITNESS_BLOCKS));
        actual.extend(batch.into_iter().map(|block| block.hash));
        batches += 1;
    }
    assert!(batches > 8_192 / MAX_WITNESS_BLOCKS);
    let expected: Vec<_> = fixture
        .blocks
        .iter()
        .rev()
        .filter(|block| block.account == address(0) && block.hash != fixture.blocks[0].hash)
        .map(|block| block.hash)
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(cursor.next().unwrap().hash, fixture.blocks[0].hash);
    assert_eq!(local.block_store().block_count().unwrap(), 0);
    service.stop().await;
}

#[test]
fn witness_suffix_stops_at_parent_height_and_does_not_reuse_partial_cache() {
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let cursor = || snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let head = cursor().next().unwrap();
    assert!(head.height > 2);
    let now = Instant::now();
    let mut transfer = Transfer::new(now);
    // Prime the ordinary proof cache with only the head, not its ancestors.
    for floor in [None, Some(0), Some(1), Some(head.height - 1)] {
        let (result, mut received) = oneshot::channel();
        transfer.start(
            Command {
                roots: match floor {
                    Some(floor) => Roots::Suffix {
                        cursor: cursor(),
                        floor,
                    },
                    None => Roots::Account(cursor()),
                },
                wanted: vec![head.hash],
                result,
            },
            now,
        );
        let mut completed = false;
        for _ in 0..100 {
            transfer.local_progress(&source, now);
            if let Ok(proofs) = received.try_recv() {
                let proofs = proofs.unwrap();
                let expected = floor.map_or(1, |floor| head.height - floor);
                assert_eq!(proofs.len() as u64, expected);
                for (offset, proof) in proofs.iter().enumerate() {
                    assert_eq!(proof.height(), head.height - offset as u64);
                }
                completed = true;
                break;
            }
        }
        assert!(completed);
    }
    for floor in [head.height, head.height + 1] {
        let (result, mut received) = oneshot::channel();
        transfer.start(
            Command {
                roots: Roots::Suffix {
                    cursor: cursor(),
                    floor,
                },
                wanted: vec![head.hash],
                result,
            },
            now,
        );
        assert!(received.try_recv().unwrap().is_err());
    }
}

#[test]
fn discovered_head_is_authenticated_without_a_duplicate_download() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let head = cursor.next().unwrap();
    let now = Instant::now();
    for corrupt in [false, true] {
        let mut client = Transfer::new(now);
        let mut server = Transfer::new(now);
        let (result, mut received) = oneshot::channel();
        client.start(Command {
            roots: Roots::Plan(vec![cursor.clone()]),
            wanted: vec![head.hash],
            result,
        }, now);
        let request = client.poll(vec!["source".into()], now).unwrap();
        let mut response = server.handle(&source, wire("client", request.message), now)
            .unwrap().unwrap();
        if corrupt {
            let Message::WitnessChunk { data, digest, total, .. } = &mut response.message else {
                panic!("expected witness chunk");
            };
            assert_eq!(data.len(), *total as usize);
            let length = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
            let mut block: StateBlock = bincode::deserialize(&data[8..8 + length]).unwrap();
            block.signature.0[0] ^= 1;
            let bytes = bincode::serialize(&block).unwrap();
            assert_eq!(bytes.len(), length);
            data[8..8 + length].copy_from_slice(&bytes);
            *digest = BlockHash::new(burst_crypto::blake2b_256(data));
            *total = data.len() as u32;
        }
        let outcome = client.handle(&local, wire("source", response.message), now);
        if corrupt {
            assert!(outcome.is_err(), "matching hash must not bypass signature validation");
            assert!(received.try_recv().is_err());
            let retry = client.poll(vec!["source".into(), "z-fallback".into()], now).unwrap();
            assert_eq!(retry.peer, "z-fallback", "invalid proof must not earn peer preference");
        } else {
            assert!(outcome.unwrap().is_none(), "head must not be requested twice");
            let proofs = received.try_recv().unwrap().unwrap();
            assert_eq!(proofs.len(), 1);
            assert_eq!(proofs[0].block().hash, head.hash);
            assert_eq!(proofs[0].context(), cursor.context());
            let other = snapshot.confirmed_heads().into_iter()
                .find(|other| other.account != head.account)
                .expect("another authenticated account");
            let other_cursor = snapshot.history_cursor(&other.account).unwrap().unwrap();
            let (result, _other_received) = oneshot::channel();
            client.start(Command {
                roots: Roots::Account(other_cursor),
                wanted: vec![other.hash],
                result,
            }, now);
            let next = client.poll(vec!["source".into(), "z-fallback".into()], now).unwrap();
            assert_eq!(next.peer, "source", "reuse a provider only after cursor validation");
            let retry = client.poll(
                vec!["source".into(), "z-fallback".into()], now + Duration::from_secs(5),
            ).unwrap();
            assert_eq!(retry.peer, "z-fallback", "a preferred provider cannot stall recovery");
        }
        assert_eq!(local.block_store().block_count().unwrap(), 0);
    }
}

#[test]
fn witness_plan_recovers_only_requested_heads_without_older_bodies() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let heads = snapshot.confirmed_heads();
    let wanted: Vec<_> = heads.iter().map(|head| head.hash).collect();
    let mut missing_ancestor = false;
    for head in &heads {
        let bytes = read_chunk(&source, head.hash, 1).unwrap().remove(0);
        let block: StateBlock = bincode::deserialize(&bytes).unwrap();
        missing_ancestor |= !block.previous.is_zero() && !wanted.contains(&block.previous);
        local
            .meta_store()
            .put_meta(&format!("checkpoint_history/v1/{}", head.hash), &bytes)
            .unwrap();
    }
    assert!(missing_ancestor);
    let roots = Roots::Plan(
        heads
            .iter()
            .map(|head| snapshot.history_cursor(&head.account).unwrap().unwrap())
            .collect(),
    );
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots,
            wanted: wanted.clone(),
            result,
        },
        now,
    );
    for _ in 0..100 {
        assert!(client.local_progress(&local, now));
        if let Ok(proofs) = received.try_recv() {
            let proofs = proofs.unwrap();
            assert_eq!(proofs.len(), wanted.len());
            for proof in proofs {
                assert!(wanted.contains(&proof.block().hash));
                snapshot
                    .history_cursor(&proof.block().account)
                    .unwrap()
                    .unwrap()
                    .accept(&[bincode::serialize(proof.block()).unwrap()])
                    .unwrap();
            }
            return;
        }
    }
    panic!("head-only witness recovery did not finish");
}

#[tokio::test]
async fn local_witness_service_progress_does_not_wait_for_peer_retry_ticks() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let heads = snapshot.confirmed_heads();
    assert!(heads.len() >= 6);
    let roots = Roots::Plan(
        heads
            .iter()
            .map(|head| snapshot.history_cursor(&head.account).unwrap().unwrap())
            .collect(),
    );
    let wanted: Vec<_> = heads.iter().map(|head| head.hash).collect();
    let store = Arc::new(store);
    let registry = Arc::new(RwLock::new(crate::ConnectionRegistry::new()));
    let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
    crate::checkpoint_sync::witness::start(store.clone(), registry.clone(), shutdown.clone()).await;
    let (result, received) = oneshot::channel();
    registry
        .read()
        .await
        .witness_requests()
        .unwrap()
        .send(Command {
            roots,
            wanted: wanted.clone(),
            result,
        })
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), received).await;
    shutdown.shutdown();
    for task in registry.write().await.take_tasks() {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
    let proofs = result
        .expect("local proofs waited for peer retry ticks")
        .unwrap()
        .unwrap();
    assert_eq!(proofs.len(), wanted.len());
    for proof in proofs {
        assert!(wanted.contains(&proof.block().hash));
        assert_eq!(
            proof.context(),
            snapshot
                .history_cursor(&proof.block().account)
                .unwrap()
                .unwrap()
                .context()
        );
    }
    assert!(registry.read().await.peer_ids().is_empty());
    assert!(shutdown.failure().is_none());
}

#[test]
fn local_witness_validation_preserves_peer_fallback_and_does_not_write() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    for case in 0..3 {
        let (_dir, local) = suffix_store(&[]);
        let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
        let hash = cursor.next().unwrap().hash;
        if case > 0 {
            for bytes in read_chunk(&source, hash, MAX_WITNESS_BLOCKS).unwrap() {
                let mut block: StateBlock = bincode::deserialize(&bytes).unwrap();
                if case == 2 && block.hash == hash {
                    block.signature.0[0] ^= 1;
                }
                local
                    .meta_store()
                    .put_meta(
                        &format!("checkpoint_history/v1/{}", block.hash),
                        &bincode::serialize(&block).unwrap(),
                    )
                    .unwrap();
            }
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let now = Instant::now();
        let mut client = Transfer::new(now);
        let (result, mut received) = oneshot::channel();
        client.start(
            Command {
                roots: Roots::Account(cursor),
                wanted: vec![hash],
                result,
            },
            now,
        );
        assert_eq!(client.local_progress(&local, now), case == 1);
        if case != 1 {
            assert!(matches!(
                received.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            let mut server = Transfer::new(now);
            let mut request = client.poll(vec!["source".into()], now).unwrap();
            assert!(
                matches!(request.message, Message::WitnessRequest { start, .. } if start == hash)
            );
            for _ in 0..110 {
                let response = server
                    .handle(&source, wire("client", request.message), now)
                    .unwrap()
                    .unwrap();
                match client
                    .handle(&local, wire("source", response.message), now)
                    .unwrap()
                {
                    Some(next) => request = next,
                    None => break,
                }
            }
        }
        let witnesses = received.try_recv().unwrap().unwrap();
        assert_eq!(witnesses.len(), 1);
        assert_eq!(witnesses[0].block().hash, hash);
        let mut authenticated = snapshot.history_cursor(&address(0)).unwrap().unwrap();
        authenticated
            .accept(&[bincode::serialize(witnesses[0].block()).unwrap()])
            .unwrap();
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

pub(super) fn exchange(
    roots: Roots,
    wanted: Vec<BlockHash>,
    source: &LmdbStore,
    local: &LmdbStore,
) -> Result<Vec<crate::confirmed_replay::snapshot::history::HistoricalBlock>, burst_store::StoreError>
{
    exchange_with(
        &mut Transfer::new(Instant::now()),
        roots,
        wanted,
        source,
        local,
    )
}

fn exchange_with(
    client: &mut Transfer,
    roots: Roots,
    wanted: Vec<BlockHash>,
    source: &LmdbStore,
    local: &LmdbStore,
) -> Result<Vec<crate::confirmed_replay::snapshot::history::HistoricalBlock>, burst_store::StoreError>
{
    let now = Instant::now();
    let (result, mut received) = oneshot::channel();
    let mut server = Transfer::new(now);
    client.start(
        Command {
            roots,
            wanted,
            result,
        },
        now,
    );
    let mut request = client.poll(vec!["source".into()], now).unwrap();
    for index in 0..110 {
        let at = now + Duration::from_secs(index);
        let response = server
            .handle(source, wire("client", request.message), at)
            .unwrap()
            .unwrap();
        match client
            .handle(local, wire("source", response.message), at)
            .unwrap()
        {
            Some(next) => request = next,
            None => return received.try_recv().unwrap(),
        }
    }
    panic!("witness exchange did not finish")
}

#[test]
fn cached_ancestry_bodies_are_reauthenticated_for_a_new_checkpoint_context() {
    let fixture = Fixture::selection();
    let (_source_dir, source, base) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let cursor = base.history_cursor(&address(0)).unwrap().unwrap();
    let head = cursor.next().unwrap();
    let chain = read_chunk(&source, head.hash, MAX_WITNESS_BLOCKS).unwrap();
    assert!(chain.len() > 1);
    let oldest: StateBlock = bincode::deserialize(chain.last().unwrap()).unwrap();
    let now = Instant::now();
    let mut client = Transfer::new(now);
    exchange_with(&mut client, Roots::Account(cursor.clone()), vec![oldest.hash], &source, &local)
        .unwrap();

    let mut changed = Fixture::selection();
    changed.push(1, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_changed_dir, _, changed) = snapshot(&changed);
    let next = changed.history_cursor(&address(0)).unwrap().unwrap();
    assert_ne!(next.context(), cursor.context());
    assert_eq!(next.next().unwrap().hash, head.hash);
    let (result, mut received) = oneshot::channel();
    client.start(Command { roots: Roots::Account(next.clone()), wanted: vec![oldest.hash], result }, now);
    assert!(received.try_recv().is_err(), "old-context proofs cannot be returned directly");
    assert!(client.local_progress(&local, now));
    let proofs = received.try_recv().unwrap().unwrap();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs[0].block().hash, oldest.hash);
    assert_eq!(proofs[0].context(), next.context());
    assert_eq!(proofs[0].height(), 1);

    let (_fork_dir, _, fork) = snapshot_with_extra_head();
    let (result, mut received) = oneshot::channel();
    client.start(Command { roots: Roots::Account(fork.history_cursor(&address(0)).unwrap().unwrap()),
        wanted: vec![oldest.hash], result }, now);
    assert!(!client.local_progress(&local, now), "a different head needs its own ancestry");
    assert!(received.try_recv().is_err());
    assert!(client.poll(vec!["source".into()], now).is_some());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn local_witness_reads_do_not_evict_peer_recovery_proofs() {
    let mut fixture = Fixture::selection();
    for seed in 40..170 {
        fixture.push(seed, BlockType::Open, 1_595_434_000, |_| {});
    }
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let hash = cursor.next().unwrap().hash;
    exchange_with(&mut client, Roots::Account(cursor.clone()), vec![hash], &source, &local)
        .unwrap();

    // Disk-resident heads must not displace proofs that required a peer walk.
    for seeds in (40..170).collect::<Vec<_>>().chunks(64) {
        let cursors: Vec<_> = seeds.iter().map(|seed|
            snapshot.history_cursor(&address(*seed)).unwrap().unwrap()
        ).collect();
        let wanted: Vec<_> = cursors.iter().map(|cursor| cursor.next().unwrap().hash).collect();
        let (result, mut received) = oneshot::channel();
        client.start(Command { roots: Roots::Exact(cursors), wanted: wanted.clone(), result }, now);
        assert!(client.local_progress(&source, now));
        let proofs = received.try_recv().unwrap().unwrap();
        assert_eq!(proofs.len(), wanted.len());
        assert!(proofs.iter().all(|proof| wanted.contains(&proof.block().hash)));
    }
    let (result, mut received) = oneshot::channel();
    client.start(Command { roots: Roots::Account(cursor.clone()), wanted: vec![hash], result }, now);
    let proof = received.try_recv().unwrap().unwrap().remove(0);
    assert_eq!(proof.block().hash, hash);
    assert_eq!(proof.context(), cursor.context());
    assert!(client.poll(Vec::new(), now).is_none());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn witness_cache_reuses_only_matching_authenticated_prefixes_without_peers() {
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let (_dir, local) = suffix_store(&[]);
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let hash = cursor.next().unwrap().hash;
    exchange_with(
        &mut client,
        Roots::Account(cursor),
        vec![hash],
        &source,
        &local,
    )
    .unwrap();
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(snapshot.history_cursor(&address(0)).unwrap().unwrap()),
            wanted: vec![hash],
            result,
        },
        now,
    );
    assert_eq!(received.try_recv().unwrap().unwrap()[0].block().hash, hash);
    assert!(client.poll(Vec::new(), now).is_none());
    for wanted in [vec![], vec![hash, hash], vec![BlockHash::ZERO]] {
        let (result, mut received) = oneshot::channel();
        client.start(
            Command {
                roots: Roots::Account(snapshot.history_cursor(&address(0)).unwrap().unwrap()),
                wanted,
                result,
            },
            now,
        );
        assert!(received.try_recv().unwrap().is_err());
    }
    let (_other_dir, _, other) = snapshot_with_extra_head();
    for cursor in [
        other.history_cursor(&address(0)).unwrap().unwrap(),
        snapshot.history_cursor(&address(1)).unwrap().unwrap(),
    ] {
        let (result, mut received) = oneshot::channel();
        client.start(
            Command {
                roots: Roots::Account(cursor),
                wanted: vec![hash],
                result,
            },
            now,
        );
        assert!(received.try_recv().is_err());
        assert!(client.poll(vec!["source".into()], now).is_some());
        drop(received);
        assert!(client.poll(Vec::new(), now).is_none());
    }
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn mixed_local_and_cached_witnesses_do_not_refetch_peer_bodies() {
    let fixture = Fixture::selection();
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let cached = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let local_cursor = snapshot.history_cursor(&address(1)).unwrap().unwrap();
    let cached_hash = cached.next().unwrap().hash;
    let local_hash = local_cursor.next().unwrap().hash;
    let local_body: StateBlock = bincode::deserialize(
        &read_chunk(&source, local_hash, 1).unwrap()[0],
    ).unwrap();
    let (_local_dir, local) = suffix_store(&[local_body]);
    let before = local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
    for exact in [false, true] {
        let now = Instant::now();
        let mut client = Transfer::new(now);
        exchange_with(&mut client, Roots::Account(cached.clone()), vec![cached_hash], &source, &local)
            .unwrap();
        for _ in 0..2 {
            let cursors = vec![cached.clone(), local_cursor.clone()];
            let roots = if exact { Roots::Exact(cursors) } else { Roots::Plan(cursors) };
            let (result, mut received) = oneshot::channel();
            client.start(Command { roots, wanted: vec![cached_hash, local_hash], result }, now);
            assert!(received.try_recv().is_err(), "the local proof is still required");
            let mut completed = None;
            for _ in 0..4 {
                assert!(client.local_progress(&local, now), "cached peer proof must not be refetched");
                if let Ok(proofs) = received.try_recv() {
                    completed = Some(proofs.unwrap());
                    break;
                }
            }
            let proofs = completed.expect("mixed proof request must finish without a peer");
            assert_eq!(proofs.len(), 2);
            assert_eq!(proofs.iter().map(|proof| proof.block().hash).collect::<HashSet<_>>(),
                HashSet::from([cached_hash, local_hash]));
            assert!(proofs.iter().all(|proof| proof.context() == cached.context()));
            assert!(client.poll(Vec::new(), now).is_none());
        }
    }
    assert_eq!(local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), before);
}

#[test]
fn completed_witness_plan_keeps_requested_peer_proofs_after_larger_ancestry_walk() {
    let mut fixture = Fixture::selection();
    let other = fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    for _ in 0..140 {
        fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    }
    let mut wanted = Vec::new();
    for seed in 40..72 {
        wanted.push(fixture.push(seed, BlockType::Open, 1_595_434_000, |_| {}));
        for _ in 0..5 {
            fixture.push(seed, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
        }
    }
    assert!(wanted.len() * 6 > MAX_WITNESS_BLOCKS);
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let (_local_dir, local) = suffix_store(&[]);
    let cursors: Vec<_> = (40..72)
        .map(|seed| snapshot.history_cursor(&address(seed)).unwrap().unwrap())
        .collect();
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let proofs = exchange_with(
        &mut client, Roots::Plan(cursors.clone()), wanted.clone(), &source, &local,
    ).unwrap();
    assert_eq!(proofs.len(), wanted.len());
    let intervening = exchange_with(
        &mut client,
        Roots::Account(snapshot.history_cursor(&address(0)).unwrap().unwrap()),
        vec![other], &source, &local,
    ).unwrap();
    assert_eq!(intervening[0].block().hash, other);
    let (result, mut received) = oneshot::channel();
    client.start(Command { roots: Roots::Plan(cursors), wanted: wanted.clone(), result }, now);
    let repeated = received.try_recv().expect("requested proofs must survive ancestor eviction").unwrap();
    assert_eq!(repeated.iter().map(|proof| proof.block().hash).collect::<HashSet<_>>(),
        wanted.into_iter().collect());
    assert!(client.poll(Vec::new(), now).is_none());
    assert_eq!(local.block_store().block_count().unwrap(), 0);
}

#[test]
fn witness_cache_evicts_old_entries_at_the_block_bound() {
    let mut fixture = Fixture::selection();
    let mut wanted = Vec::new();
    for _ in 0..140 {
        wanted.push(fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {}));
    }
    let (_dir, source, snapshot) = snapshot(&fixture);
    let (_dir, local) = suffix_store(&[]);
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let proofs = exchange_with(
        &mut client,
        Roots::Account(snapshot.history_cursor(&address(0)).unwrap().unwrap()),
        wanted,
        &source,
        &local,
    )
    .unwrap();
    assert_eq!(proofs.len(), 140);
    for (proof, cached) in [(proofs.last().unwrap(), true), (&proofs[0], false)] {
        let (result, mut received) = oneshot::channel();
        client.start(
            Command {
                roots: Roots::Account(snapshot.history_cursor(&address(0)).unwrap().unwrap()),
                wanted: vec![proof.block().hash],
                result,
            },
            now,
        );
        if cached {
            assert_eq!(
                received.try_recv().unwrap().unwrap()[0].block().hash,
                proof.block().hash
            );
        } else {
            assert!(received.try_recv().is_err());
            assert!(client.poll(vec!["source".into()], now).is_some());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn witness_plan_discovers_accounts_and_authenticates_every_requested_body() {
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = suffix_store(&[]);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let full = fixture.history(base.anchor().cutoff());
    let plan = base
        .plan_tail(&local, base.anchor().cutoff(), full.heads())
        .unwrap();
    let expected: HashSet<_> = plan.witness_hashes().iter().copied().collect();
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let proofs = exchange_with(
        &mut client,
        Roots::for_plan(&base, &plan).unwrap(),
        plan.witness_hashes().to_vec(),
        &source,
        &local,
    )
    .unwrap();
    assert_eq!(
        proofs
            .iter()
            .map(|proof| proof.block().hash)
            .collect::<HashSet<_>>(),
        expected
    );
    assert!(
        proofs
            .iter()
            .map(|proof| &proof.block().account)
            .collect::<HashSet<_>>()
            .len()
            > 1
    );
    assert!(proofs
        .iter()
        .all(|proof| proof.context() == base.history_context()));
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::for_plan(&base, &plan).unwrap(),
            wanted: plan.witness_hashes().to_vec(),
            result,
        },
        now,
    );
    let cached = received.try_recv().unwrap().unwrap();
    assert_eq!(
        cached
            .iter()
            .map(|proof| proof.block().hash)
            .collect::<HashSet<_>>(),
        expected
    );
    assert!(client.poll(Vec::new(), now).is_none());
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    // There is no witness service: complete reuse must not request any peers.
    let mut available = proofs.clone();
    available.extend(proofs.clone());
    let reused = crate::checkpoint_sync::witness::fetch_plan_reusing(
        &registry, &base, &plan, available,
    ).await.unwrap();
    assert_eq!(reused.len(), expected.len());
    assert_eq!(reused.iter().map(|proof| proof.block().hash).collect::<HashSet<_>>(), expected);
    // Partial evidence cannot silently satisfy the remaining dependencies.
    assert!(crate::checkpoint_sync::witness::fetch_plan_reusing(
        &registry, &base, &plan, proofs.iter().take(1).cloned().collect(),
    ).await.is_err());
    let mut changed = Fixture::selection();
    changed.push(1, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_changed_dir, changed_source, changed) = super::snapshot(&changed);
    let accounts: HashSet<_> = proofs.iter().map(|proof| proof.block().account.clone()).collect();
    let cursors = accounts.iter().map(|account|
        changed.history_cursor(account).unwrap().unwrap()
    ).collect();
    let wrong_context = exchange(
        Roots::Plan(cursors), plan.witness_hashes().to_vec(), &changed_source, &local,
    ).unwrap();
    assert!(wrong_context.iter().all(|proof| proof.context() != plan.context()));
    assert!(crate::checkpoint_sync::witness::fetch_plan_reusing(
        &registry, &base, &plan, wrong_context,
    ).await.is_err());
    let local = std::sync::Arc::new(local);
    let service = super::installed_recovery::Service::start(
        std::sync::Arc::new(source), local.clone(), 0,
    ).await;
    let combined = crate::checkpoint_sync::witness::fetch_plan_reusing(
        &service.registry, &base, &plan, proofs.iter().take(1).cloned().collect(),
    ).await.unwrap();
    assert_eq!(combined.len(), expected.len());
    assert_eq!(combined.iter().map(|proof| proof.block().hash).collect::<HashSet<_>>(), expected);
    assert!(service.stop().await > 0);
    let tail = plan.finish(proofs).unwrap();
    assert_eq!(tail.block_count(), 0);
    assert_eq!(tail.commitment(), full.commitment());
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
fn witness_plan_rejects_hash_matching_bodies_outside_authenticated_prefixes() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let base = snapshot.into_continuation();
    let (_dir, local) = suffix_store(&[]);
    let heads = fixture.history(base.anchor().cutoff()).heads().to_vec();
    let plan = base
        .plan_tail(&local, base.anchor().cutoff(), &heads)
        .unwrap();
    // A genuine signed block above the authenticated head is only a hint,
    // even when the serving peer supplies its exact requested hash.
    let hash = fixture.push(
        0,
        BlockType::ChangeRepresentative,
        base.anchor().cutoff() + 1,
        |block| {
            block.payment_epoch = base.anchor().hash();
        },
    );
    source
        .meta_store()
        .put_meta(
            &format!("checkpoint_history/v1/{hash}"),
            &bincode::serialize(fixture.blocks.last().unwrap()).unwrap(),
        )
        .unwrap();
    let error = exchange(
        Roots::for_plan(&base, &plan).unwrap(),
        vec![hash],
        &source,
        &local,
    )
    .err()
    .expect("unproven body must fail");
    assert!(error
        .to_string()
        .contains("outside the authenticated account prefixes"));
    assert_eq!(local.block_store().block_count().unwrap(), 0);

    let (_dir, _, other) = snapshot_with_extra_head();
    assert!(Roots::for_plan(&other.into_continuation(), &plan).is_err());
}

fn snapshot_with_extra_head() -> (tempfile::TempDir, LmdbStore, AuthenticatedSnapshot) {
    let mut fixture = Fixture::selection();
    fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    snapshot(&fixture)
}

#[test]
fn witness_transfer_authenticates_multiple_batches_and_never_installs_peer_blocks() {
    let mut fixture = Fixture::selection();
    for _ in 0..140 {
        fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    }
    let (_dir, source, snapshot) = snapshot(&fixture);
    let (_dir, local) = suffix_store(&[]);
    let local_before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let before = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let now = Instant::now();
    let mut server = Transfer::new(now);
    let mut client = Transfer::new(now);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let context = cursor.context();
    let wanted = vec![fixture.blocks[0].hash, fixture.blocks.last().unwrap().hash];
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(cursor),
            wanted: wanted.clone(),
            result,
        },
        now,
    );
    let mut request = client.poll(vec!["source".into()], now).unwrap();
    let mut chunks = 0;
    let mut batches = HashSet::new();
    loop {
        let Message::WitnessRequest { start, .. } = &request.message else {
            panic!("wrong request")
        };
        batches.insert(*start);
        let response = server
            .handle(&source, wire("client", request.message), now)
            .unwrap()
            .unwrap();
        chunks += 1;
        assert!(chunks < 30);
        let next = client
            .handle(&local, wire("source", response.message), now)
            .unwrap();
        if let Some(next) = next {
            request = next;
        } else {
            break;
        }
    }
    let recovered = received.try_recv().unwrap().unwrap();
    assert!(chunks > 2);
    assert_eq!(batches.len(), 2);
    assert_eq!(recovered.len(), wanted.len());
    assert_eq!(
        recovered
            .iter()
            .map(|proof| proof.block().hash)
            .collect::<HashSet<_>>(),
        wanted.into_iter().collect()
    );
    assert!(recovered.iter().all(|proof| proof.context() == context));
    assert_eq!(local.block_store().block_count().unwrap(), 0);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        local_before
    );
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[test]
fn witness_transfer_rejects_forged_membership_rotates_peers_and_honors_cancellation() {
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let (_dir, local) = suffix_store(&[]);
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let mut server = Transfer::new(now);
    let make = || snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let start = make().next().unwrap().hash;
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(make()),
            wanted: vec![start],
            result,
        },
        now,
    );
    let request = client.poll(vec!["a".into(), "b".into()], now).unwrap();
    assert_eq!(request.peer, "a");
    let response = server
        .handle(&source, wire("client", request.message), now)
        .unwrap()
        .unwrap();
    assert!(client
        .handle(
            &local,
            wire("unrequested-peer", response.message.clone()),
            now
        )
        .unwrap()
        .is_none());
    assert!(received.try_recv().is_err());
    let mut forged = response.message;
    let Message::WitnessChunk {
        data,
        total,
        digest,
        ..
    } = &mut forged
    else {
        panic!("wrong response")
    };
    assert_eq!(data.len(), *total as usize);
    data[12] ^= 1;
    *digest = BlockHash::new(burst_crypto::blake2b_256(data));
    assert!(client.handle(&local, wire("a", forged), now).is_err());
    let request = client.poll(vec!["a".into(), "b".into()], now).unwrap();
    assert_eq!(request.peer, "b");
    let response = server
        .handle(&source, wire("client", request.message), now)
        .unwrap()
        .unwrap();
    assert!(client
        .handle(&local, wire("b", response.message), now)
        .unwrap()
        .is_none());
    assert_eq!(received.try_recv().unwrap().unwrap()[0].block().hash, start);

    // Cancellation and timeout must exercise an uncached transfer.
    let mut client = Transfer::new(now);
    let (result, received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(make()),
            wanted: vec![start],
            result,
        },
        now,
    );
    assert!(client.poll(vec!["a".into()], now).is_some());
    drop(received);
    assert!(client.poll(vec!["a".into()], now).is_none());
    let (result, mut received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(make()),
            wanted: vec![start],
            result,
        },
        now,
    );
    assert!(client
        .poll(vec!["a".into()], now + Duration::from_secs(121))
        .is_none());
    assert!(received.try_recv().unwrap().is_err());
}

#[test]
fn witness_transfer_times_out_a_peer_and_rejects_busy_or_invalid_local_requests() {
    let fixture = Fixture::selection();
    let (_dir, _source, snapshot) = snapshot(&fixture);
    let now = Instant::now();
    let mut client = Transfer::new(now);
    let make = || snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let start = make().next().unwrap().hash;
    for wanted in [vec![], vec![BlockHash::ZERO], vec![start, start]] {
        let (result, mut received) = oneshot::channel();
        client.start(
            Command {
                roots: Roots::Account(make()),
                wanted,
                result,
            },
            now,
        );
        assert!(received.try_recv().unwrap().is_err());
    }
    let (result, _received) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(make()),
            wanted: vec![start],
            result,
        },
        now,
    );
    let (result, mut busy) = oneshot::channel();
    client.start(
        Command {
            roots: Roots::Account(make()),
            wanted: vec![start],
            result,
        },
        now,
    );
    assert!(busy.try_recv().unwrap().is_err());
    let peers = vec!["a".into(), "b".into()];
    assert_eq!(client.poll(peers.clone(), now).unwrap().peer, "a");
    assert!(client
        .poll(peers.clone(), now + Duration::from_millis(500))
        .is_none());
    assert_eq!(
        client
            .poll(peers.clone(), now + Duration::from_secs(1))
            .unwrap()
            .peer,
        "a"
    );
    assert_eq!(
        client
            .poll(peers, now + Duration::from_secs(5))
            .unwrap()
            .peer,
        "b"
    );
}
