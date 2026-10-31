//! Inputs handed to the application once core node startup is ready.
use std::sync::Arc;

pub type RpcEndpoint = Option<(u16, String, Arc<burst_rpc::RpcState>)>;
pub type WebSocketEndpoint = Option<(u16, String, Arc<burst_websocket::WsState>)>;
pub type ServiceTasks = Vec<(&'static str, tokio::task::JoinHandle<()>)>;
