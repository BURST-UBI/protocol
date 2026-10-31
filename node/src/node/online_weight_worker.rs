//! Sample online-weight telemetry every 20 seconds after refreshing authority.
//! Samples are written to storage, but startup does not reload this tracker
//! from them. Attendance never lowers the installed consensus quorum.
use super::{rebuild_rep_weights, unix_now_secs, BurstNode};
use burst_store::rep_weights::RepWeightStore;
use burst_types::Timestamp;
use std::{sync::Arc, time::Duration};

impl BurstNode {
    pub(super) fn spawn_online_weight_worker(&mut self) {
        let online_weight_tracker = Arc::clone(&self.online_weight_tracker);
        let online_weight_sampler = Arc::clone(&self.online_weight_sampler);
        let rep_weights = Arc::clone(&self.rep_weights);
        let active_elections = Arc::clone(&self.active_elections);
        let store = Arc::clone(&self.store);
        let mut shutdown_rx = self.shutdown.subscribe();

        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(20));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("online weight sampling task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now_secs = unix_now_secs();
                        // Refresh equal-person weights for verification and
                        // delegation changes. TRST expiry cannot change weight.
                        if let Err(error) = rebuild_rep_weights(&store, &rep_weights, &active_elections).await {
                            tracing::error!(%error, "cannot refresh confirmed voting authority");
                            continue;
                        }
                        let weights = rep_weights.read().await;
                        let weight_map = weights.all_weights();
                        drop(weights);
                        let sampler = online_weight_sampler.lock().await;
                        let total_online = sampler.online_weight(now_secs, &weight_map);
                        drop(sampler);

                        let mut tracker = online_weight_tracker.lock().await;
                        tracker.record_sample(total_online, Timestamp::new(now_secs));

                        let mut sampler = online_weight_sampler.lock().await;
                        sampler.update_trend(total_online);
                        let effective = sampler.effective_weight(now_secs, &weight_map);
                        drop(sampler);

                        tracing::trace!(
                            total_online,
                            effective,
                            trended = tracker.trended_weight(),
                            quorum_delta = tracker.quorum_delta(),
                            "online weight sample recorded"
                        );

                        let weight_store = store.rep_weight_store();
                        if let Err(e) = weight_store.put_online_weight_sample(now_secs, total_online) {
                            tracing::warn!(error = %e, "failed to persist online weight sample");
                        }
                    }
                }
            }
        });
        self.track_task("online weight", task);
    }
}
