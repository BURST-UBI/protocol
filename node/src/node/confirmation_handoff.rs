use super::*;

impl BurstNode {
    /// Durable handoff must not wait behind the local signer or vote repair.
    pub(super) fn spawn_confirmation_handoff(
        &mut self,
        ready: Arc<tokio::sync::Notify>,
        cementation_ready: Arc<tokio::sync::Notify>,
        fork_install_tx: mpsc::Sender<BlockHash>,
    ) {
        let active_elections = self.active_elections.clone();
        let store = self.store.clone();
        let metrics = self.metrics.clone();
        let diagnostic_port = self.config.port;
        let mut shutdown = self.shutdown.subscribe();
        self.track_task("confirmation handoff", tokio::spawn(async move {
            let handoff_worker = crate::causal_cementation::handoff_worker::Worker::new(store.clone(), cementation_ready);
            let mut repair = repair_timer::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.recv() => break,
                    _ = async {
                        tokio::select! {
                            _ = repair.tick() => {},
                            _ = ready.notified() => {},
                        }
                    } => {
                        let roots = active_elections.write().await.next_handoff_roots(
                            crate::causal_cementation::MAX_HANDOFF_BATCH);
                        // Only final-quorum elections enter durable handoff.
                        let confirmed = {
                            let elections = active_elections.read().await;
                            elections.confirmed_for_roots(&roots)
                        };

                        let mut processed_winners = std::collections::HashSet::new();
                        let mut available_winners = Vec::new();
                        for status in &confirmed {
                            let winner = status.winner;
                            // A fork hash may have votes before its full state can
                            // be installed. Do not advertise or cement it until
                            // validation and persistence have actually succeeded.
                            if !store.block_store().get_block(&winner).ok()
                                .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok())
                                .is_some_and(|block| block.hash == winner && block.compute_hash() == winner)
                            {
                                if store.meta_store().fork_candidate(&winner).ok().flatten().is_some() {
                                    let _ = fork_install_tx.try_send(winner);
                                }
                                tracing::debug!(%winner, "election winner unavailable in ledger; confirmation deferred");
                                continue;
                            }
                            available_winners.push(winner);
                        }
                        for winners in available_winners.chunks(crate::causal_cementation::MAX_HANDOFF_BATCH) {
                            match handoff_worker.handoff(winners.to_vec()).await {
                                Ok(accepted) => processed_winners.extend(accepted),
                                Err(error) => tracing::error!(%error,
                                    "confirmation handoff worker failed; elections retained"),
                            }
                        }

                        for status in &confirmed {
                            if processed_winners.contains(&status.winner) {
                                tracing::debug!(node_port = diagnostic_port, hash = %status.winner,
                                    "confirmation handoff committed");
                                metrics.confirmation_latency_ms.observe(status.election_duration_ms as f64);
                            }
                        }

                        // Free election capacity only for winners whose durable handoff succeeded.
                        if !confirmed.is_empty() {
                            let mut elections = active_elections.write().await;
                            let cleaned = elections.cleanup_confirmed_roots(&roots, |winner| processed_winners.contains(winner));
                            if !cleaned.is_empty() {
                                tracing::debug!(
                                    count = cleaned.len(),
                                    "cleaned up confirmed elections"
                                );
                            }
                        }


                        if roots.len() == crate::causal_cementation::MAX_HANDOFF_BATCH
                            && !processed_winners.is_empty() {
                            ready.notify_one();
                        }
                    }
                }
            }
        }));
    }
}
