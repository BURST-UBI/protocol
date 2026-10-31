use super::*;

const CAPACITY: usize = 256;
const BATCH_LIMIT: usize = 32;

pub(super) fn enqueue(queue: &mpsc::Sender<BlockHash>, hash: BlockHash) {
    if let Err(error) = queue.try_send(hash) {
        // The durable account backlog and periodic confirmation requests recover
        // unconfirmed candidates. This queue only accelerates their first vote.
        tracing::debug!(%hash, %error, "local vote hint deferred to confirmation repair");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_pressure_defers_signing_then_confirmation_repair_recovers() {
        use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
        let directory = tempfile::tempdir().unwrap();
        let mut config = NodeConfig {
            data_dir: directory.path().to_path_buf(), network: burst_types::NetworkId::Dev,
            params: burst_ledger::genesis::initial_params(&burst_types::NetworkId::Dev),
            port: 0, enable_rpc: false, enable_upnp: false, enable_representative: true,
            bootstrap_peers: Vec::new(), nts_sources: Vec::new(), ..NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        let mut node = BurstNode::new(config).await.unwrap();
        node.initialize_genesis().unwrap();
        node.verification_fixture_clock = Some(Arc::new(|| None));
        let store = node.store.clone();
        let signer = node.vote_generator.clone();
        let guard = signer.clone().lock_owned().await;
        let initial_sequence = guard.sequence();
        let queue = node.block_queue();
        let results = node.submission_results.clone();
        let shutdown = node.shutdown.clone();
        let creator = crate::genesis_key::genesis_address(burst_types::NetworkId::Dev);
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let mut block: StateBlock = bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
        let keys = burst_crypto::keypair_from_seed(&[17; 32]);
        block.account = burst_crypto::derive_address(&keys.public);
        block.representative = creator;
        block.payment_epoch = store.meta_store().payment_epoch().unwrap();
        block.timestamp = Timestamp::now();
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
        let payload = bincode::serialize(&block).unwrap();
        results.track(&payload);
        let task = tokio::spawn(async move {
            let started = node.start_with_services(burst_daemon::start_public_services).await;
            (node, started)
        });
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            assert!(queue.push(block.clone()).await);
            assert_eq!(tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap(),
                Some(burst_rpc::ProcessResult::Accepted));
            let capture_store = store.clone();
            let (ready, received) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let owner = std::thread::spawn(move || {
                let reader = capture_store.tx_begin_read().unwrap();
                for n in 0..8 {
                    capture_store.meta_store().put_meta("test/vote-pressure", &vec![n; 128 << 10]).unwrap();
                }
                let mut writer = capture_store.tx_begin_write().unwrap();
                let limit = writer.limit_file_extent(capture_store.env().real_disk_size().unwrap() as usize).unwrap();
                drop(writer);
                ready.send(limit.watch()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
                drop(reader);
                drop(limit);
            });
            let watch = received.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(guard);
            tokio::time::timeout(Duration::from_secs(2), async {
                while !watch.was_exceeded() { tokio::task::yield_now().await; }
            }).await.unwrap();
            // Span the ordinary one-second repair interval while capacity is held.
            tokio::time::sleep(Duration::from_millis(1100)).await;
            assert_eq!(signer.lock().await.sequence(), initial_sequence);
            assert_eq!(store.account_store().get_account(&block.account).unwrap().confirmation_height, 0);
            assert!(!watch.wait_released(Duration::ZERO));
            release.send(()).unwrap();
            owner.join().unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if store.account_store().get_account(&block.account).unwrap().confirmation_height == 1 { break; }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }).await.expect("ordinary vote repair and confirmation handoff must recover without resubmission");
            assert!(signer.lock().await.sequence() > initial_sequence);
            assert_eq!(store.account_store().get_account(&block.account).unwrap().head, block.hash);
        }).await;
        shutdown.shutdown();
        let (mut node, started) = tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
        started.unwrap();
        node.stop().await.unwrap();
        outcome.expect("vote pressure recovery timed out");
    }

    #[tokio::test]
    async fn held_signer_bounds_hints_without_blocking_enqueue_or_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let mut node = BurstNode::new(NodeConfig {
            data_dir: directory.path().to_path_buf(),
            network: burst_types::NetworkId::Dev,
            params: burst_ledger::genesis::initial_params(&burst_types::NetworkId::Dev),
            enable_representative: true,
            ..NodeConfig::default()
        }).await.unwrap();
        node.initialize_genesis().unwrap();
        let genesis = crate::genesis_state::block(node.config.network, &node.initial_params);
        let signer = node.vote_generator.clone().lock_owned().await;
        let queue = node.spawn_local_votes();
        enqueue(&queue, genesis.hash);
        tokio::time::timeout(Duration::from_secs(1), async {
            while queue.capacity() != CAPACITY {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        for _ in 0..CAPACITY {
            enqueue(&queue, genesis.hash);
        }
        assert_eq!(queue.capacity(), 0);
        // The production enqueue must return even when both the signer and
        // queue are saturated. No spawned task is created for this overflow.
        enqueue(&queue, genesis.hash);
        assert_eq!(queue.capacity(), 0);
        assert_eq!(signer.sequence(), 0);
        node.shutdown.shutdown();
        crate::shutdown::drain_tasks(
            std::mem::take(&mut node.task_handles), Duration::from_secs(1),
        ).await.unwrap();
        assert!(queue.is_closed());
        assert_eq!(signer.sequence(), 0);
        drop(signer);
        // Shutdown discarded only hints; their canonical durable block survives.
        assert!(node.store.block_store().get_block(&genesis.hash).is_ok());
    }
}

impl BurstNode {
    pub(super) fn spawn_local_votes(&mut self) -> mpsc::Sender<BlockHash> {
        let (queue, mut pending) = mpsc::channel::<BlockHash>(CAPACITY);
        let store = self.store.clone();
        let generator = self.vote_generator.clone();
        let spacing = self.vote_spacing.clone();
        let elections = self.active_elections.clone();
        let weights = self.rep_weights.clone();
        let sampler = self.online_weight_sampler.clone();
        let peers = self.peer_manager.clone();
        let broadcaster = self.broadcaster.clone();
        let mut shutdown = self.shutdown.subscribe();
        let diagnostic_port = self.config.port;
        self.track_task("local votes", tokio::spawn(async move {
            let stage_log = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
            loop {
                let hash = tokio::select! {
                    biased;
                    _ = shutdown.recv() => break,
                    hash = pending.recv() => match hash {
                        Some(hash) => hash,
                        None => break,
                    },
                };
                let voting = async {
                    let started = stage_log.then(std::time::Instant::now);
                    if stage_log { tracing::debug!(node_port = diagnostic_port, %hash, "local vote hint consumed"); }
                    let guard = generator.clone().lock_owned().await;
                    if let Some(started) = started {
                        tracing::debug!(node_port = diagnostic_port, %hash,
                            elapsed_us = started.elapsed().as_micros(), "local vote signer acquired");
                    }
                    if !guard.is_representative {
                        return;
                    }
                    let mut hashes = vec![hash];
                    while hashes.len() < BATCH_LIMIT {
                        let Ok(hash) = pending.try_recv() else { break; };
                        hashes.push(hash);
                    }
                    let mut candidates = Vec::with_capacity(hashes.len());
                    for hash in hashes {
                        let block = match store.block_store().get_block(&hash)
                            .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes)
                                .map_err(|error| burst_store::StoreError::Serialization(error.to_string()))) {
                            Ok(block) => block,
                            Err(error) => {
                                tracing::debug!(%hash, %error, "local vote candidate unavailable");
                                continue;
                            }
                        };
                        let root = crate::ledger_updater::election_root(&block);
                        let mut spacing = spacing.lock().await;
                        if !spacing.votable(&root, &hash) { continue; }
                        spacing.record(root, hash);
                        candidates.push((hash, root, block));
                    }
                    let hashes = candidates.iter().map(|(hash, _, _)| *hash).collect();
                    // The worker retains signer ownership through commit and sync,
                    // including cancellation. Fallback rechecks cached/final choices.
                    let votes = match crate::durable_votes::worker::responses(store.clone(), guard, hashes).await {
                        Ok(votes) => votes,
                        Err(error) => {
                            tracing::error!(%error, "refusing local votes without durable reservation");
                            return;
                        }
                    };
                    for ((hash, root, block), result) in candidates.into_iter().zip(votes) {
                        let vote = match result {
                            Ok(Some(vote)) => vote,
                            Ok(None) => continue,
                            Err(error) => {
                                tracing::error!(%hash, %error, "refusing local vote without durable reservation");
                                continue;
                            }
                        };
                        if let Some(started) = started {
                            tracing::debug!(node_port = diagnostic_port, %hash, voter = %vote.voter,
                                is_final = vote.is_final, elapsed_us = started.elapsed().as_micros(),
                                "local vote response prepared");
                        }
                        let now = Timestamp::new(unix_now_secs());
                        let weight = weights.read().await.weight(&vote.voter);
                        let mut active = elections.write().await;
                        if weight > 0 || active.has_authorized_voter(&vote.voter) {
                            match crate::backlog_scan::register_candidate(&store, &mut active, &block, now) {
                                Ok(true) => {},
                                Ok(false) => continue,
                                Err(error) => {
                                    tracing::debug!(%hash, %error, "local vote election deferred");
                                    continue;
                                }
                            }
                            let _ = active.process_signed_vote(&root, burst_consensus::VoteInfo::new(
                                vote.voter.clone(), hash, weight, vote.is_final,
                                Timestamp::new(vote.timestamp / 1000), vote.sequence,
                            ), now);
                            drop(active);
                            sampler.lock().await.record_vote(&vote.voter, now.as_secs());
                        } else {
                            drop(active);
                        }
                        let message = WireMessage::Vote(WireVote {
                            voter: vote.voter, block_hashes: vec![hash], is_final: vote.is_final,
                            timestamp: vote.timestamp, sequence: vote.sequence, signature: vote.signature,
                        });
                        if let Ok(bytes) = message.encode() {
                            let peers: Vec<_> = peers.read().await.iter_connected()
                                .map(|(_, peer)| peer.clone()).collect();
                            let _ = broadcaster.broadcast_with_fanout(&bytes, &peers, 4).await;
                        }
                    }
                };
                tokio::select! {
                    biased;
                    _ = shutdown.recv() => break,
                    _ = voting => {},
                }
            }
        }));
        queue
    }
}
