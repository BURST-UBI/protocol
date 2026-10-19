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

    /// Address the RPC server binds to. Defaults to `127.0.0.1` (localhost
    /// only) because the RPC surface includes powerful operations such as
    /// `genesis_endorse`; exposing it on all interfaces (`0.0.0.0`) requires an
    /// explicit opt-in and an external firewall.
    #[serde(default = "default_rpc_bind")]
    pub rpc_bind: String,

    /// Whether to enable the WebSocket server.
    #[serde(default)]
    pub enable_websocket: bool,

    /// WebSocket port (if enabled).
    #[serde(default = "default_ws_port")]
    pub websocket_port: u16,

    /// Event subscriptions are local by default; remote access is opt-in.
    #[serde(default = "default_rpc_bind")]
    pub websocket_bind: String,

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

    /// Whether to run the reachout loop that actively dials peers discovered
    /// via keepalive gossip (self-forming mesh). Now safe by default: the
    /// connection registry resolves simultaneous connects deterministically
    /// (the connection from the lower-addressed node survives), so mutual
    /// dials dedup to one stable connection instead of churning.
    #[serde(default = "default_true")]
    pub enable_peer_reachout: bool,

    /// Whether to enable UPnP port mapping for NAT traversal.
    /// Automatically requests the router to forward the P2P port.
    /// Disabled on dev networks; enabled by default on live/test.
    #[serde(default = "default_true")]
    pub enable_upnp: bool,

    /// Override address advertised to peers in keepalive (for nodes without UPnP).
    /// Use when running on cloud VPSes where UPnP fails. Format: "IP" or "IP:port".
    /// If only IP is given, the P2P port is used. UPnP overrides this when active.
    #[serde(default)]
    pub advertise_address: Option<String>,

    /// Whether this node participates in consensus as a voting representative.
    /// When enabled, the node generates and broadcasts votes for validated blocks.
    /// Defaults to true so testnet nodes vote out of the box.
    #[serde(default = "default_true")]
    pub enable_representative: bool,

    /// File containing this representative's 32-byte Ed25519 seed as hex.
    /// Use one durable node/data directory per signing key.
    #[serde(default)]
    pub representative_key_file: Option<PathBuf>,

    /// Independent certificate-authenticated time services for checkpoint signing.
    /// An empty list disables fresh checkpoint signatures, never weakens time checks.
    #[serde(default = "burst_network::trusted_clock::NtsClock::public_sources")]
    pub nts_sources: Vec<burst_network::trusted_clock::NtsSource>,

    /// Mirrors for the fixed mainnet drand chain, never alternate randomness.
    /// At most four HTTPS endpoints (loopback HTTP is allowed for local relays).
    #[serde(default = "default_drand_relays")]
    pub drand_relays: Vec<String>,

    /// Optional archive serving/staging. Fetch alone never installs a ledger.
    #[serde(default)]
    pub snapshot_transfer: crate::checkpoint_sync::snapshot::Config,
    /// Explicit startup installation; a matching fetch output downloads first.
    /// Installation remains release-gated.
    #[serde(default)]
    pub snapshot_bootstrap: Option<PathBuf>,
}

// ── Serde default helpers ──────────────────────────────────────────────

fn default_network() -> NetworkId {
    NetworkId::Dev
}

fn default_drand_relays() -> Vec<String> {
    vec![burst_vrf::drand::DRAND_MAINNET_URL.into()]
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

fn default_rpc_bind() -> String {
    "127.0.0.1".to_string()
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
    pub(crate) fn representative_key(&self) -> Result<Option<burst_types::KeyPair>, NodeError> {
        use std::io::Read;
        let Some(path) = &self.representative_key_file else {
            return Ok(None);
        };
        let file = std::fs::File::open(path)
            .map_err(|e| NodeError::Config(format!("cannot open representative key file: {e}")))?;
        let metadata = file
            .metadata()
            .map_err(|e| NodeError::Config(e.to_string()))?;
        if !metadata.is_file() {
            return Err(NodeError::Config(
                "representative key must be a regular file".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(NodeError::Config("representative key file must not be accessible to group or others (use mode 600)".into()));
            }
        }
        let mut encoded = String::new();
        file.take(129)
            .read_to_string(&mut encoded)
            .map_err(|_| NodeError::Config("cannot read representative key encoding".into()))?;
        let mut seed = [0; 32];
        if encoded.trim().len() != 64 || hex::decode_to_slice(encoded.trim(), &mut seed).is_err() {
            return Err(NodeError::Config(
                "representative key must contain exactly 32 hex-encoded bytes".into(),
            ));
        }
        Ok(Some(burst_crypto::keypair_from_seed(&seed)))
    }

    /// Load configuration from a TOML file.
    pub fn from_toml_file(path: &str) -> Result<Self, NodeError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| NodeError::Config(e.to_string()))?;
        Self::from_toml_str(&content)
    }

    /// Parse configuration from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, NodeError> {
        let config: Self = toml::from_str(s).map_err(|e| NodeError::Config(e.to_string()))?;
        config.validate_drand_relays()?;
        config
            .snapshot_transfer
            .validate()
            .map_err(|error| NodeError::Config(error.to_string()))?;
        Ok(config)
    }

    pub(crate) fn validate_drand_relays(&self) -> Result<(), NodeError> {
        burst_vrf::DrandClient::validate_relays(&self.drand_relays)
            .map(|_| ())
            .map_err(|error| NodeError::Config(error.to_string()))
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
            rpc_bind: default_rpc_bind(),
            enable_websocket: false,
            websocket_port: default_ws_port(),
            websocket_bind: default_rpc_bind(),
            bootstrap_peers: Vec::new(),
            log_format: default_log_format(),
            log_level: default_log_level(),
            work_threads: default_work_threads(),
            enable_metrics: false,
            enable_faucet: false,
            enable_peer_reachout: true,
            enable_upnp: true,
            advertise_address: None,
            enable_representative: true,
            representative_key_file: None,
            nts_sources: burst_network::trusted_clock::NtsClock::public_sources(),
            drand_relays: default_drand_relays(),
            snapshot_transfer: Default::default(),
            snapshot_bootstrap: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_identity_is_explicit_and_malformed_keys_fail_closed() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let seed = [7; 32];
        writeln!(file, "{}", hex::encode(seed)).unwrap();
        let config = NodeConfig {
            representative_key_file: Some(file.path().into()),
            ..NodeConfig::default()
        };
        assert_eq!(
            config.representative_key().unwrap().unwrap().public,
            burst_crypto::keypair_from_seed(&seed).public
        );
        assert!(!config.to_toml_string().contains(&hex::encode(seed)));
        file.as_file_mut().set_len(0).unwrap();
        assert!(config.representative_key().is_err());
        assert!(NodeConfig::default()
            .representative_key()
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn representative_key_rejects_public_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let config = NodeConfig {
            representative_key_file: Some(file.path().into()),
            ..NodeConfig::default()
        };
        assert!(config
            .representative_key()
            .err()
            .unwrap()
            .to_string()
            .contains("mode 600"));
    }

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
        assert_eq!(config.websocket_bind, "127.0.0.1");
        assert_eq!(config.max_peers, 50);
        assert_eq!(config.log_format, "human");
        assert_eq!(
            config.drand_relays,
            vec![burst_vrf::drand::DRAND_MAINNET_URL]
        );
    }

    #[test]
    fn drand_mirrors_round_trip_and_invalid_configuration_fails_before_startup() {
        let config = NodeConfig::from_toml_str(
            "drand_relays = ['https://relay.example/a', 'http://127.0.0.1:9090']",
        )
        .unwrap();
        assert_eq!(
            NodeConfig::from_toml_str(&config.to_toml_string())
                .unwrap()
                .drand_relays,
            config.drand_relays
        );
        for toml in [
            "drand_relays = []",
            "drand_relays = ['http://remote.example']",
            "drand_relays = ['https://user:secret@remote.example']",
        ] {
            assert!(NodeConfig::from_toml_str(toml).is_err());
        }
        let config = NodeConfig {
            drand_relays: vec![],
            ..NodeConfig::default()
        };
        assert!(config.validate_drand_relays().is_err());
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
