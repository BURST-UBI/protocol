//! Explicit credentials for isolated development-node fixtures.
use burst_node::services::{RpcEndpoint, ServiceTasks, WebSocketEndpoint};
use burst_node::ShutdownController;
use burst_wallet_core::NodeClient;
use std::{sync::Arc, time::Duration};

const TOKEN: &str = "b85c9f24c067186e8d9a2fdc493dfa810c244f33d491fd6e678d8a612913850a";

pub fn start_services(
    rpc: RpcEndpoint,
    websocket: WebSocketEndpoint,
    shutdown: Arc<ShutdownController>,
) -> ServiceTasks {
    let mut tasks = burst_daemon::start_public_services(None, websocket, shutdown.clone());
    if let Some((port, bind, state)) = rpc {
        assert_eq!(state.network, burst_types::NetworkId::Dev);
        assert_eq!(bind, "127.0.0.1");
        let server =
            burst_rpc::RpcServer::with_bind(port, bind, state).with_admin_token(TOKEN.to_owned());
        let mut stopped = shutdown.subscribe();
        tasks.push((
            "fixture admin RPC",
            tokio::spawn(async move {
                if let Err(error) = server
                    .start(async move {
                        let _ = stopped.recv().await;
                    })
                    .await
                {
                    shutdown.fail("fixture admin RPC", &error);
                }
            }),
        ));
    }
    tasks
}

#[allow(dead_code)]
pub async fn endorse(client: &NodeClient, account: &str) -> Result<serde_json::Value, String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let body = serde_json::json!({"action": "genesis_endorse", "account": account});
    for token in [None, Some("00".repeat(32))] {
        let mut request = http.post(client.node_url()).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let status = request
            .send()
            .await
            .map_err(|error| error.to_string())?
            .status();
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    }
    let response = http
        .post(client.node_url())
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let response = response
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let reply: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    if let Some(error) = reply.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_owned());
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| "missing RPC result".to_owned())
}
