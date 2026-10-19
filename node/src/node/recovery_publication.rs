use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredInstalledMetadata;

impl BurstNode {
    pub(crate) async fn publish_recovered(
        &mut self,
        recovered: RecoveredInstalledMetadata,
        cache_at: Timestamp,
    ) -> Result<(), NodeError> {
        if !self.certified_selection_enabled()
            || !self.connection_registry.read().await.is_recovering()
        {
            return Err(NodeError::Other(
                "compact publication requires gated startup".into(),
            ));
        }
        let mut governance = self.governance.lock().await;
        let mut delegation = self.delegation_engine.lock().await;
        let mut constitution = self.consti_engine.lock().await;
        let mut brn = self.brn_engine.lock().await;
        let mut trst = self.trst_engine.lock().await;
        let mut verification = self.verification_orchestrator.lock().await;
        let mut processor = self.block_processor.lock().await;
        let mut frontier = self.frontier.write().await;
        let mut weights = self.rep_weights.write().await;
        let mut scheduler = self.priority_scheduler.lock().await;
        let mut elections = self.active_elections.write().await;
        let mut backlog = self.backlog.lock().await;
        let mut broadcaster = self.local_broadcaster.lock().await;
        // No await while the LMDB writer exists. Every fallible preparation
        // precedes commit; publication after force-sync is replacement only.
        let staged = recovered
            .project_token_indexes_at(&self.store, cache_at)?
            .stage_undo()?;
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
        let mut next_elections = elections.clone();
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
        if let Some(probe) = self.checkpoint_publication_probe {
            probe("prepared");
        }
        let (runtime, recovery) = match staged.commit_runtime() {
            Ok(published) => published,
            Err(error) => {
                self.shutdown.fail("compact recovery commit", &error);
                return Err(error.into());
            }
        };
        if let Err(error) = self.store.force_sync() {
            self.shutdown.fail("compact recovery sync", &error);
            return Err(NodeError::Other(error.to_string()));
        }
        #[cfg(test)]
        if let Some(probe) = self.checkpoint_publication_probe {
            probe("committed");
        }
        *brn = runtime.brn;
        *trst = runtime.trst;
        *verification = runtime.verification;
        *governance = runtime.governance;
        *delegation = runtime.delegation;
        *constitution = runtime.constitution;
        #[cfg(test)]
        if let Some(probe) = self.checkpoint_publication_probe {
            probe("economics");
        }
        *weights = next_weights;
        *elections = next_elections;
        *frontier = next_frontier;
        processor.install_payment_epoch(runtime.anchor.hash(), runtime.anchor.cutoff());
        processor.install_params(&runtime.params);
        self.config.params = runtime.params;
        scheduler.clear();
        backlog.clear();
        broadcaster.clear();
        self.ledger_cache.reset(runtime.ledger_counts);
        // The installer rechecks the base commitment and accepted suffix before
        // reusing this history. Publish it only after commit and force-sync.
        self.recovered_checkpoint = Some(recovery);
        #[cfg(test)]
        if let Some(probe) = self.checkpoint_publication_probe {
            probe("complete");
        }
        Ok(())
    }
}
