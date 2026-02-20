//! Graceful shutdown controller for the BURST node.
//!
//! Listens for SIGINT/SIGTERM and broadcasts a shutdown signal to all
//! subsystems via a `tokio::sync::broadcast` channel.

use tokio::signal;
use tokio::sync::broadcast;

/// Coordinates graceful shutdown across all node subsystems.
///
/// Subsystems call [`subscribe`] to get a receiver, then `select!` on it
/// alongside their main loop. When shutdown is triggered (either by OS signal
/// or programmatically), every receiver is notified.
pub struct ShutdownController {
    tx: broadcast::Sender<()>,
}

impl ShutdownController {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self { tx }
    }

    /// Get a receiver that will be notified on shutdown.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }

    /// Trigger shutdown programmatically.
    pub fn shutdown(&self) {
        let _ = self.tx.send(());
    }

    /// Wait for SIGTERM or SIGINT, then trigger shutdown.
    pub async fn wait_for_signal(&self) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn programmatic_shutdown_notifies_subscribers() {
        let controller = ShutdownController::new();
        let mut rx = controller.subscribe();
        controller.shutdown();
        assert!(rx.recv().await.is_ok());
    }

    #[tokio::test]
    async fn multiple_subscribers_all_notified() {
        let controller = ShutdownController::new();
        let mut rx1 = controller.subscribe();
        let mut rx2 = controller.subscribe();
        controller.shutdown();
        assert!(rx1.recv().await.is_ok());
        assert!(rx2.recv().await.is_ok());
    }
}
