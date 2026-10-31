use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredInstalledMetadata;

impl BurstNode {
    pub(crate) async fn publish_recovered(
        &mut self,
        recovered: RecoveredInstalledMetadata,
    ) -> Result<(), NodeError> {
        if !self.certified_selection_enabled()
            || !self.connection_registry.read().await.is_recovering()
        {
            return Err(NodeError::Other(
                "compact publication requires gated startup".into(),
            ));
        }
        let cache_at = Timestamp::new(recovered.recovered().continuation.anchor().cutoff());
        let mut governance = self.governance.clone().lock_owned().await;
        let mut delegation = self.delegation_engine.clone().lock_owned().await;
        let mut constitution = self.consti_engine.clone().lock_owned().await;
        let mut brn = self.brn_engine.clone().lock_owned().await;
        let mut trst = self.trst_engine.clone().lock_owned().await;
        let mut verification = self.verification_orchestrator.clone().lock_owned().await;
        let mut processor = self.block_processor.clone().lock_owned().await;
        let mut frontier = self.frontier.clone().write_owned().await;
        let mut weights = self.rep_weights.clone().write_owned().await;
        let mut scheduler = self.priority_scheduler.clone().lock_owned().await;
        let mut elections = self.active_elections.clone().write_owned().await;
        let mut backlog = self.backlog.clone().lock_owned().await;
        let mut broadcaster = self.local_broadcaster.clone().lock_owned().await;
        // Hold publication locks across the worker: admission remains gated and
        // no runtime engine can observe a partially installed durable state.
        let store = self.store.clone();
        let shutdown = self.shutdown.clone();
        let ledger_cache = self.ledger_cache.clone();
        let mut next_elections = elections.clone();
        #[cfg(test)]
        let probe = self.checkpoint_publication_probe;
        let prepared = tokio::task::spawn_blocking(move || {
            // Keep the writer entirely inside this worker. Preparation precedes
            // commit; runtime replacement follows commit and force-sync.
            let started = std::time::Instant::now();
            let projection =
                recovered.project_token_indexes_with_stable_holdings(&store, cache_at)?;
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "recovery publication derived indexes"
            );
            let undo_started = std::time::Instant::now();
            let staged = projection.stage_undo()?;
            tracing::debug!(
                elapsed_ms = undo_started.elapsed().as_millis(),
                "recovery publication staged journals"
            );
            let runtime = staged.runtime();
            let mut next_weights = RepWeightCache::new();
            next_weights.rebuild_from_accounts(
                runtime
                    .anchor
                    .roster()
                    .humans()
                    .iter()
                    .map(|human| (true, human.representative.clone())),
            );

            next_elections
                .install_authority(
                    runtime.anchor.hash(),
                    next_weights.all_weights(),
                    runtime.anchor.quorum_bps(),
                )
                .map_err(|error| NodeError::Other(error.to_string()))?;
            let mut next_frontier = DagFrontier::new();
            for (address, head) in &runtime.frontiers {
                next_frontier.update(address.clone(), *head);
            }
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("prepared");
            }
            let (runtime, recovery) = match staged.commit_runtime() {
                Ok(published) => published,
                Err(error) => {
                    shutdown.fail("compact recovery commit", &error);
                    return Err(error.into());
                }
            };
            if let Err(error) = store.force_sync() {
                shutdown.fail("compact recovery sync", &error);
                return Err(NodeError::Other(error.to_string()));
            }
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("committed");
            }
            *brn = runtime.brn;
            *trst = runtime.trst;
            *verification = runtime.verification;
            *governance = runtime.governance;
            *delegation = runtime.delegation;
            *constitution = runtime.constitution;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("economics");
            }
            *weights = next_weights;
            *elections = next_elections;
            *frontier = next_frontier;
            processor.install_payment_epoch(runtime.anchor.hash(), runtime.anchor.cutoff());
            processor.install_params(&runtime.params);
            scheduler.clear();
            backlog.clear();
            broadcaster.clear();
            ledger_cache.reset(runtime.ledger_counts);
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("complete");
            }
            Ok::<_, NodeError>((runtime.params, recovery))
        })
        .await;
        let (params, recovery) = prepared.map_err(|error| {
            self.shutdown
                .fail("compact recovery publication worker", &error);
            NodeError::Other(format!(
                "compact recovery publication worker failed: {error}"
            ))
        })??;
        self.config.params = params;
        // Publish retained history only after the worker has committed, synced
        // and replaced all runtime engines.
        self.recovered_checkpoint = Some(recovery);
        Ok(())
    }
}
