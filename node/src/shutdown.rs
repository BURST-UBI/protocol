//! Node task draining and shared shutdown control.
pub use burst_utils::shutdown::ShutdownController;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

/// Await all workers within one shared deadline, then abort and await unfinished
/// tasks. Aborting is cooperative: the final waits have no timeout, and a running
/// blocking task cannot be stopped this way.
pub(crate) async fn drain_tasks(
    mut tasks: Vec<tokio::task::JoinHandle<()>>,
    timeout: std::time::Duration,
) -> Result<(), crate::NodeError> {
    let mut failure = None;
    let completed = tokio::time::timeout(timeout, async {
        for task in &mut tasks {
            if let Err(error) = task.await {
                tracing::error!(%error, "node background task failed");
                failure.get_or_insert_with(|| error.to_string());
            }
        }
    })
    .await
    .is_ok();
    if !completed {
        // Dropping JoinHandle detaches its task; it does not stop ledger writes.
        let unfinished: Vec<_> = tasks
            .into_iter()
            .filter(|task| !task.is_finished())
            .collect();
        for task in &unfinished {
            tracing::error!(task = %task.id(), timeout_ms = timeout.as_millis(),
                "node background task exceeded shutdown deadline");
            task.abort();
        }
        for task in unfinished {
            let _ = task.await;
        }
    }
    if !completed {
        return Err(crate::NodeError::ShutdownTimeout);
    }
    if let Some(error) = failure {
        return Err(crate::NodeError::Other(format!(
            "node background task failed: {error}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_deadline_aborts_and_reaps_unfinished_workers() {
        struct Dropped(std::sync::Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        let flag = dropped.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let stuck = tokio::spawn(async move {
            let _guard = Dropped(flag);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert!(matches!(
            drain_tasks(vec![stuck], std::time::Duration::from_millis(10)).await,
            Err(crate::NodeError::ShutdownTimeout)
        ));
        assert!(dropped.load(Ordering::Acquire));
        assert!(drain_tasks(
            vec![tokio::spawn(async {})],
            std::time::Duration::from_secs(1)
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn worker_panics_are_not_reported_as_clean_shutdowns() {
        let panic = tokio::spawn(async { panic!("test worker failure") });
        let result = drain_tasks(
            vec![panic, tokio::spawn(async {})],
            std::time::Duration::from_secs(1),
        )
        .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("test worker failure"));
    }

    #[tokio::test]
    async fn fatal_worker_cause_survives_cleanup_and_late_subscribers() {
        let controller = ShutdownController::new();
        controller.fail("beacon", "corrupt certified state");
        controller.fail("other", "secondary failure");
        controller.shutdown();
        assert_eq!(
            controller.failure().as_deref(),
            Some("beacon: corrupt certified state")
        );
        let mut late = controller.subscribe();
        assert!(late.try_recv().is_ok());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            controller.wait_for_signal(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn node_cleanup_returns_the_fatal_worker_error_to_its_caller() {
        let directory = tempfile::tempdir().unwrap();
        let config = crate::NodeConfig {
            network: burst_types::NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            nts_sources: vec![],
            ..crate::NodeConfig::default()
        };
        let mut node = crate::BurstNode::new(config).await.unwrap();
        node.shutdown
            .fail("checkpoint", "durable state cannot be recovered");
        let result = node.stop().await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "checkpoint: durable state cannot be recovered"
        );
    }

    #[tokio::test]
    async fn programmatic_shutdown_notifies_subscribers() {
        let controller = ShutdownController::new();
        let mut rx = controller.subscribe();
        controller.shutdown();
        assert!(rx.recv().await.is_ok());
    }

    #[tokio::test]
    async fn occupied_listeners_stop_the_node_and_report_failure() {
        for server in ["P2P listener", "RPC server", "WebSocket server"] {
            let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = occupied.local_addr().unwrap().port();
            let directory = tempfile::tempdir().unwrap();
            let config = crate::NodeConfig {
                network: burst_types::NetworkId::Dev,
                data_dir: directory.path().to_path_buf(),
                port: if server == "P2P listener" { port } else { 0 },
                enable_rpc: server == "RPC server",
                rpc_port: port,
                enable_websocket: server == "WebSocket server",
                websocket_port: port,
                enable_upnp: false,
                enable_representative: false,
                nts_sources: vec![],
                ..crate::NodeConfig::default()
            };
            let mut node = crate::BurstNode::new(config).await.unwrap();
            let started = tokio::time::timeout(std::time::Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
                .await
                .unwrap();
            if server == "P2P listener" {
                assert!(started
                    .unwrap_err()
                    .to_string()
                    .contains("cannot bind P2P listener"));
                assert!(node.connection_registry.read().await.is_empty());
                node.stop().await.unwrap();
                continue;
            }
            started.unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.stop())
                .await
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().starts_with(server), "{server}: {error}");
        }
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

    #[tokio::test]
    async fn programmatic_shutdown_releases_the_node_start_waiter() {
        let controller = ShutdownController::new();
        let wait = controller.wait_for_signal();
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut wait)
                .await
                .is_err()
        );
        controller.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(1), wait)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_requested_before_start_is_not_lost() {
        let controller = ShutdownController::new();
        controller.shutdown();
        let mut late = controller.subscribe();
        assert!(late.try_recv().is_ok());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            controller.wait_for_signal(),
        )
        .await
        .unwrap();
    }
}
