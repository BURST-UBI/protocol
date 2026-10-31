use super::*;
use crate::checkpoint_sync::witness::{self, Roots};
use crate::confirmed_replay::snapshot::{archive::AuthenticatedSnapshot, installed};

impl BurstNode {
    pub(super) async fn install_bootstrap_snapshot(&self) -> Result<(), NodeError> {
        let Some(path) = self.config.snapshot_bootstrap.clone() else {
            return Ok(());
        };
        if !self.certified_selection_enabled()
            || !self.connection_registry.read().await.is_recovering()
        {
            return Err(NodeError::Other(
                "snapshot installation requires gated startup".into(),
            ));
        }
        let store = self.store.clone();
        let network = self.config.network;
        let params = self.initial_params.clone();
        let max_archive_bytes = self.config.snapshot_transfer.max_archive_bytes;
        if let Some(fetch) = self
            .config
            .snapshot_transfer
            .fetch
            .as_ref()
            .filter(|fetch| fetch.output == path)
        {
            let genesis =
                crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, &params)?;
            crate::checkpoint_sync::snapshot::fetch_before_admission(
                fetch.clone(),
                max_archive_bytes,
                genesis,
                self.connection_registry.clone(),
                self.shutdown.clone(),
                #[cfg(test)]
                self.checkpoint_publication_probe,
            )
            .await?;
        }
        let prepared = tokio::task::spawn_blocking(move || {
            let genesis =
                crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, &params)?;
            let snapshot = AuthenticatedSnapshot::read_file_with_budget(
                &genesis,
                &path,
                store.env().path(),
                max_archive_bytes,
            )
            .map_err(|error| NodeError::Other(error.to_string()))?;
            if let Some(base) = installed::load_base(&store, network, &params)? {
                if base.anchor().hash() == snapshot.anchor().hash()
                    && base.certificate().manifest.state_hash
                        == snapshot.certificate().manifest.state_hash
                {
                    // The caller's ordinary installed-state recovery still checks
                    // the exact epoch, retained history, journals and economics.
                    return Ok(None);
                }
            }
            let needs = snapshot.installation_needs(&store)?;
            let mut wanted: Vec<_> = needs
                .snapshot_heads
                .iter()
                .chain(&needs.cemented_heads)
                .map(|head| head.hash)
                .chain(needs.confirmation_targets)
                .collect();
            wanted.sort_by_key(|hash| *hash.as_bytes());
            wanted.dedup();
            let roots = needs
                .snapshot_heads
                .iter()
                .map(|head| {
                    snapshot
                        .history_cursor(&head.account)
                        .map_err(|error| NodeError::Other(error.to_string()))?
                        .ok_or_else(|| {
                            NodeError::Other("snapshot account has no authenticated head".into())
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let committed = snapshot
                .committed_history_cursors(&wanted)
                .map_err(|error| NodeError::Other(error.to_string()))?;
            Ok::<_, NodeError>(Some((snapshot, Roots::Plan(roots), wanted, committed)))
        })
        .await
        .map_err(|error| NodeError::Other(error.to_string()))??;
        let Some((snapshot, roots, wanted, committed)) = prepared else {
            return Ok(());
        };
        let proofs =
            witness::request_with_committed(&self.connection_registry, roots, wanted, committed)
                .await?;
        let store = self.store.clone();
        let params = self.initial_params.clone();
        #[cfg(test)]
        let probe = self.checkpoint_publication_probe;
        tokio::task::spawn_blocking(move || {
            let prepared = snapshot
                .reconcile_installation(store.tx_begin_write()?, network, &params, &proofs)?
                .project_ledger(&[])?
                .project_economics()?;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("bootstrap_prepared");
            }
            prepared.commit_for_startup()?;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("bootstrap_committed");
            }
            store
                .force_sync()
                .map_err(|error| NodeError::Other(error.to_string()))?;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe("bootstrap_synced");
            }
            Ok::<_, NodeError>(())
        })
        .await
        .map_err(|error| NodeError::Other(error.to_string()))??;
        Ok(())
    }
}
