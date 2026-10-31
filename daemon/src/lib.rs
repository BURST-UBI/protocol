//! Application-level composition of public servers around node services.
use burst_utils::shutdown::ShutdownController;
use std::sync::Arc;

pub fn start_public_services(
    rpc: Option<(u16, String, Arc<burst_rpc::RpcState>)>,
    websocket: Option<(u16, String, Arc<burst_websocket::WsState>)>,
    shutdown: Arc<ShutdownController>,
) -> Vec<(&'static str, tokio::task::JoinHandle<()>)> {
    let mut tasks = Vec::new();
    if let Some((port, bind, state)) = rpc {
        let server = burst_rpc::RpcServer::with_bind(port, bind, state);
        let mut stopped = shutdown.subscribe();
        let failure = shutdown.clone();
        tasks.push((
            "RPC server",
            tokio::spawn(async move {
                match server
                    .start(async move {
                        let _ = stopped.recv().await;
                    })
                    .await
                {
                    Ok(()) => tracing::info!("RPC server exited"),
                    Err(error) => {
                        tracing::error!(%error, "RPC server error");
                        failure.fail("RPC server", &error);
                    }
                }
            }),
        ));
    }
    if let Some((port, bind, state)) = websocket {
        let server = burst_websocket::WebSocketServer::with_bind(port, bind, state);
        let mut stopped = shutdown.subscribe();
        let failure = shutdown.clone();
        tasks.push((
            "WebSocket server",
            tokio::spawn(async move {
                match server
                    .start(async move {
                        let _ = stopped.recv().await;
                    })
                    .await
                {
                    Ok(()) => tracing::info!("WebSocket server exited"),
                    Err(error) => {
                        tracing::error!(%error, "WebSocket server error");
                        failure.fail("WebSocket server", &error);
                    }
                }
            }),
        ));
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn listener_bind_failure_requests_shutdown_and_preserves_cause() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let shutdown = Arc::new(ShutdownController::new());
        let tasks = start_public_services(
            None,
            Some((
                port,
                "127.0.0.1".into(),
                Arc::new(burst_websocket::WsState::new(8)),
            )),
            shutdown.clone(),
        );
        assert_eq!(tasks.len(), 1);
        for (_, task) in tasks {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
        }
        assert!(shutdown.is_requested());
        assert!(shutdown.failure().unwrap().starts_with("WebSocket server:"));
    }

    #[tokio::test]
    async fn shutdown_before_listener_start_is_not_lost() {
        let shutdown = Arc::new(ShutdownController::new());
        shutdown.shutdown();
        let tasks = start_public_services(
            None,
            Some((
                0,
                "127.0.0.1".into(),
                Arc::new(burst_websocket::WsState::new(8)),
            )),
            shutdown.clone(),
        );
        for (_, task) in tasks {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
        }
        assert!(shutdown.failure().is_none());
    }
}
