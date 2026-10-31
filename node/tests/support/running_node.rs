//! Shared real-node lifecycle for integration journeys.
use burst_node::{BurstNode, NodeConfig, ShutdownController};
use burst_store_lmdb::LmdbStore;
use burst_wallet_core::NodeClient;
use std::{sync::Arc, time::Duration};

#[path = "admin_rpc.rs"]
pub mod admin_rpc;

pub struct RunningNode {
    pub client: NodeClient,
    pub store: Arc<LmdbStore>,
    pub shutdown: Arc<ShutdownController>,
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl RunningNode {
    #[allow(dead_code)] // Shared by integration targets with different fixture entry points.
    pub async fn launch(config: NodeConfig) -> Self {
        Self::launch_with_services(config, burst_daemon::start_public_services).await
    }

    #[allow(dead_code)]
    pub async fn launch_with_admin(config: NodeConfig) -> Self {
        Self::launch_with_services(config, admin_rpc::start_services).await
    }

    async fn launch_with_services(
        config: NodeConfig,
        services: fn(
            burst_node::services::RpcEndpoint,
            burst_node::services::WebSocketEndpoint,
            Arc<ShutdownController>,
        ) -> burst_node::services::ServiceTasks,
    ) -> Self {
        let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
        let mut node = BurstNode::new(config).await.unwrap();
        let store = node.store.clone();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            let started = node.start_with_services(services).await;
            // Failed startup can leave workers to drain; preserve both errors.
            let stopped = node.stop().await;
            lifecycle_result(
                started.map_err(|e| e.to_string()),
                stopped.map_err(|e| e.to_string()),
            )
        });
        Self {
            client,
            store,
            shutdown,
            task: Some(task),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|task| task.is_finished())
    }

    pub async fn stop(mut self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), self.task.take().unwrap())
            .await
            .expect("node shutdown timed out")
            .expect("node task panicked")
            .unwrap_or_else(|error| panic!("{error}"));
    }
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        // Drop cannot await; callers use stop() to prove complete drainage.
        self.shutdown.shutdown();
    }
}

fn lifecycle_result(
    started: Result<(), String>,
    stopped: Result<(), String>,
) -> Result<(), String> {
    match (started, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(start), Ok(())) => Err(format!("node startup failed: {start}")),
        (Ok(()), Err(stop)) => Err(format!("node shutdown failed: {stop}")),
        (Err(start), Err(stop)) => Err(format!(
            "node startup failed: {start}; shutdown also failed: {stop}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        RunningNode,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            LmdbStore::open(
                directory.path(),
                burst_store_lmdb::REQUIRED_DATABASES,
                8 << 20,
            )
            .unwrap(),
        );
        let shutdown = Arc::new(ShutdownController::new());
        let mut stopped = shutdown.subscribe();
        let (finished, completion) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            stopped.recv().await.unwrap();
            finished.send(()).unwrap();
            Ok(())
        });
        let node = RunningNode {
            client: NodeClient::new("http://127.0.0.1:1").unwrap(),
            store,
            shutdown,
            task: Some(task),
        };
        (directory, node, completion)
    }

    #[tokio::test]
    async fn early_drop_requests_worker_shutdown() {
        let (_directory, node, completion) = fixture();
        let shutdown = node.shutdown.clone();
        drop(node);
        assert!(shutdown.is_requested());
        tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn explicit_stop_joins_worker_cleanup() {
        let (_directory, node, mut completion) = fixture();
        node.stop().await;
        assert_eq!(completion.try_recv(), Ok(()));
    }
    #[tokio::test]
    async fn startup_bind_failure_is_reported_after_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = NodeConfig {
            network: burst_types::NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            rpc_bind: "127.0.0.1".into(),
            rpc_port: occupied.local_addr().unwrap().port(),
            enable_upnp: false,
            enable_metrics: false,
            bootstrap_peers: Vec::new(),
            ..NodeConfig::default()
        };
        let mut node = RunningNode::launch(config).await;
        let result = tokio::time::timeout(Duration::from_secs(20), node.task.take().unwrap())
            .await
            .expect("failed startup did not drain")
            .expect("node task panicked");
        assert!(node.shutdown.is_requested());
        let error = result.expect_err("occupied RPC listener must fail startup");
        assert!(error.contains("RPC server"), "{error}");
    }
}
