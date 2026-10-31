//! Shutdown requests and the first fatal failure shared by node workers.
//!
//! The controller notifies subscribers. Worker cleanup, task joining and storage
//! flushing belong to the caller; requesting shutdown does not complete them.

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::signal;
use tokio::sync::broadcast;

/// Persistent shutdown flag with broadcast notifications.
/// Workers subscribe through [`Self::subscribe`] and wait alongside their work.
/// Repeated requests can coalesce or produce lagged notifications; workers should
/// treat the request as a stop condition rather than count individual messages.
pub struct ShutdownController {
    tx: broadcast::Sender<()>,
    requested: AtomicBool,
    failure: std::sync::Mutex<Option<String>>,
}

impl ShutdownController {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            tx,
            requested: AtomicBool::new(false),
            failure: std::sync::Mutex::new(None),
        }
    }

    /// Subscribe to shutdown requests. If already requested, send another
    /// notification so this receiver also observes the stop condition.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        let receiver = self.tx.subscribe();
        if self.requested.load(Ordering::Acquire) {
            let _ = self.tx.send(());
        }
        receiver
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// Set the shutdown flag and notify subscribers. Does not wait for workers.
    pub fn shutdown(&self) {
        self.requested.store(true, Ordering::Release);
        let _ = self.tx.send(());
    }

    /// Fatal local worker/storage failure, not a rejected peer message or a
    /// transient relay outage. Preserve the first cause through normal cleanup.
    pub fn fail(&self, worker: &str, error: impl std::fmt::Display) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.is_none() {
            *failure = Some(format!("{worker}: {error}"));
        }
        drop(failure);
        self.shutdown();
    }

    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Wait for Ctrl+C, Unix SIGTERM, or an existing programmatic request.
    /// Installs signal listeners while this future is polled; constructing the
    /// controller alone does not listen for OS signals.
    pub async fn wait_for_signal(&self) {
        let mut requested = self.subscribe();
        if self.requested.load(Ordering::Acquire) {
            return;
        }
        let ctrl_c = signal::ctrl_c();

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = requested.recv() => { return; }
            _ = ctrl_c => { tracing::info!("received SIGINT, shutting down"); }
            _ = terminate => { tracing::info!("received SIGTERM, shutting down"); }
        }

        self.shutdown();
    }
}

impl Default for ShutdownController {
    fn default() -> Self {
        Self::new()
    }
}
