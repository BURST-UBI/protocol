//! Axum-based RPC server.

use crate::error::RpcError;

pub struct RpcServer {
    pub port: u16,
}

impl RpcServer {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// Start the RPC server.
    pub async fn start(&self) -> Result<(), RpcError> {
        todo!("create axum router with all handlers, bind to port, serve")
    }
}
