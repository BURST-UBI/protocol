use super::{handoff_batch, BlockHash, LmdbStore, StoreError, MAX_HANDOFF_BATCH};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// Serialize blocking handoffs per Worker, retaining the guard through caller
/// cancellation. Committed handoffs remain queued even if their caller leaves.
pub(crate) struct Worker {
    store: Arc<LmdbStore>,
    gate: Arc<Mutex<()>>,
    ready: Arc<Notify>,
}

impl Worker {
    pub(crate) fn new(store: Arc<LmdbStore>, ready: Arc<Notify>) -> Self {
        Self {
            store,
            gate: Arc::new(Mutex::new(())),
            ready,
        }
    }

    /// Try one bounded batch, then retry targets individually if the batch fails.
    /// Returns only accepted hashes; individual failures are logged, so an empty
    /// or partial successful result is possible. Waiting callers are not capped.
    pub(crate) async fn handoff(
        &self,
        targets: Vec<BlockHash>,
    ) -> Result<Vec<BlockHash>, StoreError> {
        if targets.len() > MAX_HANDOFF_BATCH {
            return Err(StoreError::Serialization(
                "confirmation handoff batch exceeds bound".into(),
            ));
        }
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let guard = self.gate.clone().lock_owned().await;
        let store = self.store.clone();
        let ready = self.ready.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let accepted = match handoff_batch(&store, &targets) {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!(%error, "confirmation handoff batch aborted; isolating targets");
                    let mut accepted = Vec::new();
                    for winner in targets {
                        match handoff_batch(&store, &[winner]) {
                            Ok(committed) => accepted.extend(committed),
                            Err(error) => tracing::error!(%winner, %error,
                                "confirmation handoff failed; election retained"),
                        }
                    }
                    accepted
                }
            };
            // Wake only after durable publication, even if the async caller was cancelled.
            if !accepted.is_empty() {
                ready.notify_one();
            }
            accepted
        })
        .await
        .map_err(|error| {
            StoreError::Backend(format!("confirmation handoff worker failed: {error}"))
        })
    }
}
