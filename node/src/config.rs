//! Node configuration with TOML file support.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use burst_types::{NetworkId, ProtocolParams};

use crate::NodeError;

/// Configuration for a BURST node.
///
/// Can be loaded from a TOML file via [`NodeConfig::from_toml_file`] or
/// built programmatically (e.g. for tests).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Which network to connect to.
    #[serde(default = "default_network")]
    pub network: NetworkId,

    /// Data directory for ledger storage.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Protocol parameters (loaded from genesis/governance, not TOML config).
    #[serde(skip)]
    pub params: ProtocolParams,

    /// Maximum number of peer connections.
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,

    /// Port to listen on for P2P connections.
    #[serde(default = "default_p2p_port")]
    pub port: u16,

    /// Whether this node opts in to be a verifier.
    #[serde(default)]
    pub enable_verification: bool,

    /// Whether to enable the RPC server.
    #[serde(default = "default_true")]
    pub enable_rpc: bool,

    /// RPC port (if enabled).
    #[serde(default = "default_rpc_port")]
    pub rpc_port: u16,

    /// Whether to enable the WebSocket server.
    #[serde(default)]
    pub enable_websocket: bool,

    /// WebSocket port (if enabled).
    #[serde(default = "default_ws_port")]
    pub websocket_port: u16,

    /// Bootstrap peer addresses to connect to on startup.
    #[serde(default)]
    pub bootstrap_peers: Vec<String>,

    /// Log format: "human" or "json".
    #[serde(default = "default_log_format")]
    pub log_format: String,

    /// Log level filter: "trace", "debug", "info", "warn", "error".
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Number of work threads for PoW validation.
    #[serde(default = "default_work_threads")]
    pub work_threads: usize,

    /// Whether to enable Prometheus metrics endpoint.
    #[serde(default)]
    pub enable_metrics: bool,

    /// Whether to enable the testnet faucet endpoint.
    #[serde(default)]
    pub enable_faucet: bool,
}

// ── Serde default helpers ──────────────────────────────────────────────

fn default_network() -> NetworkId {
    NetworkId::Dev
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./burst_data")
}

fn default_max_peers() -> usize {
    50
}

fn default_p2p_port() -> u16 {
    NetworkId::Dev.default_port()
}

fn default_true() -> bool {
    true
}

fn default_rpc_port() -> u16 {
    7077
}

fn default_ws_port() -> u16 {
    7078
}

fn default_log_format() -> String {
    "human".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_work_threads() -> usize {
    1
}

// ── Impl ───────────────────────────────────────────────────────────────

impl NodeConfig {
    /// Load configuration from a TOML file.
    pub fn from_toml_file(path: &str) -> Result<Self, NodeError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| NodeError::Config(e.to_string()))?;
        Self::from_toml_str(&content)
    }

    /// Parse configuration from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, NodeError> {
        toml::from_str(s).map_err(|e| NodeError::Config(e.to_string()))
    }

    /// Serialize the configuration to a TOML string.
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).expect("NodeConfig is always serializable to TOML")
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            network: default_network(),
            data_dir: default_data_dir(),
            params: ProtocolParams::default(),
            max_peers: default_max_peers(),
            port: default_p2p_port(),
            enable_verification: false,
            enable_rpc: default_true(),
            rpc_port: default_rpc_port(),
            enable_websocket: false,
            websocket_port: default_ws_port(),
            bootstrap_peers: Vec::new(),
            log_format: default_log_format(),
            log_level: default_log_level(),
            work_threads: default_work_threads(),
            enable_metrics: false,
            enable_faucet: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = NodeConfig::default();
        let toml_str = config.to_toml_string();
        let parsed = NodeConfig::from_toml_str(&toml_str).expect("should parse");
        assert_eq!(parsed.rpc_port, config.rpc_port);
        assert_eq!(parsed.max_peers, config.max_peers);
    }

    #[test]
    fn minimal_toml_uses_defaults() {
        let config = NodeConfig::from_toml_str("").expect("empty toml should use defaults");
        assert_eq!(config.rpc_port, 7077);
        assert_eq!(config.max_peers, 50);
        assert_eq!(config.log_format, "human");
    }

    #[test]
    fn partial_toml_overrides() {
        let toml = r#"
            rpc_port = 9999
            max_peers = 100
        "#;
        let config = NodeConfig::from_toml_str(toml).expect("should parse");
        assert_eq!(config.rpc_port, 9999);
        assert_eq!(config.max_peers, 100);
        assert_eq!(config.log_format, "human"); // default
    }

    #[test]
    fn missing_file_returns_config_error() {
        let result = NodeConfig::from_toml_file("/nonexistent/burst.toml");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, NodeError::Config(_)));
    }
}
