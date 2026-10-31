use super::*;
use burst_store::peer::PeerStore;

impl BurstNode {
    pub(super) async fn start_recovery_peers(
        &mut self,
        feedback: &mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    ) {
        let mut addresses = self.peer_manager.read().await.bootstrap_peers().to_vec();
        match self.store.peer_store().all_peers() {
            Ok(cached) => {
                for (address, _) in cached {
                    if !addresses.contains(&address) {
                        addresses.push(address);
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "recovery peer cache unavailable"),
        }
        if addresses.is_empty() {
            return;
        }
        let context = crate::peer_connector::PeerConnectorContext {
            peer_manager: self.peer_manager.clone(),
            connection_registry: self.connection_registry.clone(),
            block_queue: self.block_queue.clone(),
            metrics: self.metrics.clone(),
            active_elections: self.active_elections.clone(),
            rep_weights: self.rep_weights.clone(),
            vote_generator: self.vote_generator.clone(),
            message_dedup: self.message_dedup.clone(),
            online_weight_sampler: self.online_weight_sampler.clone(),
            store: self.store.clone(),
            node_private_key: burst_types::PrivateKey(self.node_private_key.0),
            node_address: self.node_address.clone(),
            params_hash: self.config.params.params_hash(),
            network: self.config.network,
            peering_port: self.config.port,
            bootstrap_feedback: feedback.clone(),
        };
        let mut stop = self.shutdown.subscribe();
        let max_peers = self.config.max_peers;
        self.track_task("recovery peer connections", tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(2));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut next = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = stop.recv() => break,
                    _ = timer.tick() => {}
                }
                {
                    let registry = context.connection_registry.read().await;
                    if !registry.is_recovering() {
                        break;
                    }
                    if registry.len() >= max_peers {
                        continue;
                    }
                }
                let address = &addresses[next];
                next = (next + 1) % addresses.len();
                // The connector owns the shared dial claim and releases it on
                // return. Shutdown closes the registry and clears all claims.
                tokio::select! {
                    biased;
                    _ = stop.recv() => break,
                    result = crate::peer_connector::connect_to_peer(address, &context) => {
                        if let Err(error) = result {
                            tracing::debug!(peer = %address, %error, "recovery peer dial failed");
                        }
                    }
                }
            }
        }));
    }
}
