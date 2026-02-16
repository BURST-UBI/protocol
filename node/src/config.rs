//! Node configuration.

use burst_types::{NetworkId, ProtocolParams};
use std::path::PathBuf;

/// Configuration for a BURST node.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// Which network to connect to.
    pub network: NetworkId,

    /// Data directory for ledger storage.
    pub data_dir: PathBuf,

    /// Protocol parameters (may be overridden by governance).
    pub params: ProtocolParams,

    /// Maximum number of peer connections.
    pub max_peers: usize,

    /// Port to listen on for P2P connections.
    pub port: u16,

    /// Whether this node opts in to be a verifier.
    pub enable_verification: bool,

    /// Whether to enable the RPC server.
    pub enable_rpc: bool,

    /// RPC port (if enabled).
    pub rpc_port: u16,

    /// Whether to enable the WebSocket server.
    pub enable_websocket: bool,

    /// WebSocket port (if enabled).
    pub websocket_port: u16,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            network: NetworkId::Dev,
            data_dir: PathBuf::from("./burst_data"),
            params: ProtocolParams::default(),
            max_peers: 50,
            port: NetworkId::Dev.default_port(),
            enable_verification: false,
            enable_rpc: true,
            rpc_port: 7077,
            enable_websocket: false,
            websocket_port: 7078,
        }
    }
}
