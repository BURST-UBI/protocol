use super::*;
use burst_governance::{content::EncodedContent, GovernableParam};
use burst_store::meta::MetaStore;
use burst_types::{Timestamp, TxHash};

#[tokio::test]
async fn recovery_content_service_fetches_archived_bytes_without_admission() {
    recovery_content_source(false).await;
}

#[tokio::test]
async fn recovery_content_service_fetches_cached_fork_without_admission() {
    recovery_content_source(true).await;
}

async fn recovery_content_source(cached: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (block, content) = proposal(1, 10, true);
    let directory = tempfile::tempdir().unwrap();
    let source = LmdbStore::open(&directory.path().join("source"), 32, 32 << 20).unwrap();
    let local = Arc::new(LmdbStore::open(&directory.path().join("local"), 32, 32 << 20).unwrap());
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    if cached {
        source
            .meta_store()
            .put_fork_candidate_with_content(
                &block.hash,
                &bincode::serialize(&block).unwrap(),
                Some(content.bytes()),
            )
            .unwrap();
    } else {
        let mut batch = source.tx_begin_write().unwrap();
        batch
            .put_meta(
                &format!("checkpoint_history/v1/{}", block.hash),
                &bincode::serialize(&block).unwrap(),
            )
            .unwrap();
        batch
            .put_meta(
                &crate::confirmed_history::proposal_archive_key(block.hash),
                content.bytes(),
            )
            .unwrap();
        batch.commit().unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (connection, _) = listener.accept().await.unwrap();
    let (_reader, writer) = connection.into_split();
    let registry = Arc::new(RwLock::new(ConnectionRegistry::for_recovery()));
    let recovery = Arc::new(Transfers::default());
    let live = Arc::new(Transfers::default());
    let queue = Arc::new(BlockPriorityQueue::new(4));
    let shutdown = Arc::new(ShutdownController::new());
    let (sender, inbox) = mpsc::channel(INBOX_CAPACITY);
    {
        let mut registry = registry.write().await;
        registry.insert("peer".into(), writer);
        registry.set_governance_inbox(sender.clone());
        registry.set_recovery_content(recovery.clone());
    }
    let server = tokio::spawn(async move {
        while let Ok(length) = peer.read_u32().await {
            assert!(length as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let mut bytes = vec![0; length as usize];
            peer.read_exact(&mut bytes).await.unwrap();
            let WireMessage::GovernanceContent(Packet::Request { proposal, offset }) =
                WireMessage::decode(&bytes).unwrap()
            else {
                panic!("unexpected request")
            };
            let bytes =
                WireMessage::GovernanceContent(response(&source, proposal, offset).unwrap())
                    .encode()
                    .unwrap();
            let WireMessage::GovernanceContent(packet) = WireMessage::decode(&bytes).unwrap()
            else {
                unreachable!()
            };
            if sender
                .send(Incoming {
                    peer: "peer".into(),
                    packet,
                })
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = peer.shutdown().await;
    });
    let worker = tokio::spawn(run(
        live.clone(),
        recovery.clone(),
        local.clone(),
        queue.clone(),
        registry.clone(),
        inbox,
        shutdown.clone(),
    ));
    let received = tokio::time::timeout(Duration::from_secs(10), recovery.fetch(&block)).await;
    shutdown.shutdown();
    worker.await.unwrap();
    registry.write().await.close();
    server.await.unwrap();
    assert_eq!(received.unwrap().unwrap().as_slice(), content.bytes());
    assert!(queue.is_empty().await);
    assert!(live.entries.lock().unwrap().is_empty());
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
fn archived_proposals_remain_servable_without_live_ledger_entries() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let (block, content) = proposal(1, 10, true);
    let archive = crate::confirmed_history::proposal_archive_key(block.hash);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta(
            &format!("checkpoint_history/v1/{}", block.hash),
            &bincode::serialize(&block).unwrap(),
        )
        .unwrap();
    batch.put_meta(&archive, content.bytes()).unwrap();
    batch.commit().unwrap();
    assert!(!store.block_store().exists(&block.hash).unwrap());
    let mut received = ContentReceiver::new(content.manifest()).unwrap();
    let mut completed = None;
    for chunk in content.chunks() {
        let Packet::Chunk { proposal, chunk } = response(&store, block.hash, chunk.offset).unwrap()
        else {
            panic!("expected chunk")
        };
        assert_eq!(proposal, block.hash);
        if let Some(value) = received.push(&chunk).unwrap() {
            completed = Some(value);
        }
    }
    assert_eq!(
        completed.unwrap().encode_content().unwrap(),
        content.bytes()
    );
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        load(&batch, &block).unwrap().encode_content().unwrap(),
        content.bytes()
    );
    drop(batch);
    assert!(response(&store, block.hash, 1).is_err());
    assert!(response(&store, block.hash, content.manifest().length).is_err());
    store
        .meta_store()
        .put_meta(&key(block.hash), b"corrupt live body")
        .unwrap();
    assert!(response(&store, block.hash, 0).is_err());
    let batch = store.tx_begin_write().unwrap();
    assert!(load(&batch, &block).is_err());
}

fn proposal(owner: u8, at: u64, large: bool) -> (StateBlock, EncodedContent) {
    let keys = burst_crypto::keypair_from_seed(&[owner; 32]);
    let content = if large {
        ProposalContent::ConstitutionalAmendment {
            title: "test".into(),
            text: "🌍".repeat(50_000),
        }
    } else {
        ProposalContent::ParameterChange {
            param: GovernableParam::BrnRate,
            new_value: 5,
        }
    };
    let state = burst_wallet_core::transaction_builder::AccountState { signing_grant: burst_types::BlockHash::ZERO,
        head: BlockHash::new([1; 32]),
        payment_epoch: BlockHash::new([2; 32]),
        block_count: 1,
        representative: burst_crypto::derive_address(&keys.public),
        brn_balance: 0,
        trst_balance: 0,
        release_sources: vec![],
    };
    burst_wallet_core::governance_content::build_signed_proposal(
        &state,
        &keys,
        &content,
        BlockHash::new([3; 32]),
        Timestamp::new(at),
    )
    .unwrap()
}

#[test]
fn staging_enforces_global_owner_and_lifetime_budgets() {
    let transfers = Transfers::default();
    for index in 0..MAX_TRANSFERS {
        let (block, content) = proposal(index as u8, 10, false);
        transfers.offer(&block, content.bytes()).unwrap();
    }
    let (block, content) = proposal(40, 10, false);
    assert!(transfers.offer(&block, content.bytes()).is_err());
    transfers.tick(&[], Instant::now() + TTL).unwrap();
    transfers.offer(&block, content.bytes()).unwrap();
    let (second, content) = proposal(40, 11, false);
    transfers.offer(&second, content.bytes()).unwrap();
    let (third, content) = proposal(40, 12, false);
    assert!(transfers.offer(&third, content.bytes()).is_err());
    transfers.remove(block.hash);
    transfers.offer(&third, content.bytes()).unwrap();
    let original = transfers.entries.lock().unwrap()[&third.hash].created;
    transfers.offer(&third, content.bytes()).unwrap();
    assert_eq!(
        transfers.entries.lock().unwrap()[&third.hash].created,
        original
    );
}

#[test]
fn correlated_fetch_rotates_after_timeout_or_bad_data_and_requeues_verified_content() {
    let transfers = Transfers::default();
    let (block, content) = proposal(1, 10, true);
    let chunks: Vec<_> = content.chunks().collect();
    let peers = vec!["bad".into(), "good".into()];
    transfers.defer(&block).unwrap();
    let now = Instant::now();
    let (requests, _) = transfers.tick(&peers, now).unwrap();
    assert_eq!(requests[0].0, "bad");
    transfers.receive("good", block.hash, &chunks[0]).unwrap();
    assert!(transfers.ready(block.hash).unwrap().is_none());
    assert!(transfers.tick(&peers, now).unwrap().0.is_empty());
    let (requests, _) = transfers.tick(&peers, now + RETRY).unwrap();
    assert_eq!(requests[0].0, "good");
    let mut corrupt = chunks[0].clone();
    corrupt.manifest.hash = BlockHash::new([9; 32]);
    transfers.receive("good", block.hash, &corrupt).unwrap();
    for chunk in &chunks {
        let (requests, _) = transfers.tick(&peers, Instant::now()).unwrap();
        let (peer, Packet::Request { proposal, offset }) = &requests[0] else {
            panic!("expected request");
        };
        assert_eq!(*proposal, block.hash);
        assert_eq!(*offset, chunk.offset);
        let packet = Packet::Chunk {
            proposal: block.hash,
            chunk: chunk.clone(),
        };
        let bytes = WireMessage::GovernanceContent(packet).encode().unwrap();
        assert!(bytes.len() < burst_types::MAX_P2P_MESSAGE_BYTES);
        assert!(WireMessage::decode(&bytes).is_ok());
        transfers.receive(peer, block.hash, chunk).unwrap();
    }
    assert_eq!(
        transfers.ready(block.hash).unwrap().unwrap().as_slice(),
        content.bytes()
    );
    let (requests, ready) = transfers.tick(&peers, Instant::now()).unwrap();
    assert!(requests.is_empty());
    assert_eq!(ready[0].hash, block.hash);
    transfers
        .receive("unknown", BlockHash::new([9; 32]), &chunks[0])
        .unwrap();
    assert_eq!(transfers.entries.lock().unwrap().len(), 1);
}

#[test]
fn staging_rejects_unsigned_or_mismatched_bundles_without_allocating_a_slot() {
    let transfers = Transfers::default();
    let (mut block, content) = proposal(1, 10, false);
    block.origin = TxHash::ZERO;
    assert!(transfers.offer(&block, content.bytes()).is_err());
    assert!(transfers.defer(&block).is_err());
    let (block, content) = proposal(1, 10, false);
    assert!(transfers.offer(&block, &content.bytes()[..4]).is_err());
    assert!(transfers.entries.lock().unwrap().is_empty());
}
