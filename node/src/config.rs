//! Node configuration with TOML file support.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use burst_types::{NetworkId, ProtocolParams};

use crate::NodeError;

/// Local physical-history retention policy. Economic rights and signer records
/// are never selected by this policy. Age is measured at the certified base cut.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BodyRetirementConfig {
    pub enabled: bool,
    pub retain_seconds: u64,
    /// Delay before the first scan, between completed scans, and after a failed
    /// or blocked page. Productive pages yield and continue with the same limits.
    pub interval_secs: u64,
    /// Aggregate temporary and durable recovery extent budget. Retirement
    /// reserves one quarter (up to 16 MiB), plus 64 KiB lock allowance, for its cache.
    /// Must be explicitly configured to at least 4 MiB before enabling retirement.
    pub max_scratch_bytes: Option<u64>,
    /// Cooperative initial-snapshot limits; synchronous work and in-flight writes
    /// may overshoot. Headroom uses file high-water extent, including reusable pages.
    pub max_capture_secs: u64,
    pub max_capture_growth_bytes: u64,
    pub min_capture_headroom_bytes: u64,
    pub max_examined: usize,
    pub max_bodies: usize,
    pub max_bytes: u64,
}

impl Default for BodyRetirementConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            retain_seconds: 30 * 86400,
            interval_secs: 3600,
            max_scratch_bytes: None,
            max_capture_secs: 120,
            max_capture_growth_bytes: 64 << 20,
            min_capture_headroom_bytes: 4 << 20,
            max_examined: 4096,
            max_bodies: 256,
            max_bytes: 4 << 20,
        }
    }
}

impl BodyRetirementConfig {
    pub(crate) fn capture_limits(&self) -> crate::body_retirement::capture_budget::CaptureLimits {
        crate::body_retirement::capture_budget::CaptureLimits {
            duration: std::time::Duration::from_secs(self.max_capture_secs),
            growth_bytes: self.max_capture_growth_bytes,
            headroom_bytes: self.min_capture_headroom_bytes,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.enabled && self.max_scratch_bytes.is_none_or(|bytes| bytes < 4 << 20) {
            return Err(
                "physical body retirement requires an explicit scratch byte budget of at least4MiB"
                    .into(),
            );
        }
        if self.max_capture_secs == 0
            || self.max_capture_secs > 120
            || self.max_capture_growth_bytes == 0
            || self.max_capture_growth_bytes > 64 << 20
            || self.min_capture_headroom_bytes < 4 << 20
        {
            return Err("invalid retirement capture lifetime/growth/headroom budget".into());
        }
        if self.interval_secs == 0
            || self.interval_secs > 31_536_000
            || self.max_examined == 0
            || self.max_examined > 131_072
            || self.max_bodies == 0
            || self.max_bodies > 131_072
            || self.max_bytes == 0
            || self.max_bytes > 64 << 20
        {
            return Err("invalid physical body retirement budget".into());
        }
        Ok(())
    }
}

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

    /// LMDB address-space limit in bytes, configured before opening the ledger.
    /// Whole MiB only; defaults to 1 GiB. This is not a resident-memory budget.
    #[serde(default = "default_database_map_size_bytes")]
    pub database_map_size_bytes: u64,

    /// Protocol parameters (loaded from genesis/governance, not TOML config).
    #[serde(skip)]
    pub params: ProtocolParams,

    /// Maximum number of peer connections.
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,

    /// Live defaults restrict address concentration; dev/test allow local clusters.
    #[serde(default)]
    pub peer_limits: Option<burst_network::admission::PeerLimits>,

    /// Shared outbound budget; bulk traffic leaves capacity for control messages.
    #[serde(default)]
    pub bandwidth_limits: burst_network::bandwidth::BandwidthLimits,

    /// Port to listen on for P2P connections.
    #[serde(default = "default_p2p_port")]
    pub port: u16,

    /// Stored preference; the node does not read it or submit verifier opt-in.
    /// Verifier enrollment requires a signed ledger operation.
    #[serde(default)]
    pub enable_verification: bool,

    /// Whether to enable the RPC server.
    #[serde(default = "default_true")]
    pub enable_rpc: bool,

    /// RPC port (if enabled).
    #[serde(default = "default_rpc_port")]
    pub rpc_port: u16,

    /// RPC bind address, defaulting to numeric loopback. When the node holds
    /// the genesis seed, server startup rejects non-loopback bindings.
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

    /// Stored format preference; daemon tracing does not read it.
    #[serde(default = "default_log_format")]
    pub log_format: String,

    /// Stored filter preference; daemon tracing reads environment variables instead.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Stored worker-count preference; the running node does not size a pool from it.
    #[serde(default = "default_work_threads")]
    pub work_threads: usize,

    /// Expose Prometheus metrics at /metrics on the RPC listener; requires RPC.
    #[serde(default)]
    pub enable_metrics: bool,

    /// Enable faucet and server-signing helpers on Dev/Test; never enables Live.
    #[serde(default)]
    pub enable_faucet: bool,

    /// Dial peers discovered through keepalive gossip. Simultaneous connections
    /// keep the connection initiated by the lower node ID.
    #[serde(default = "default_true")]
    pub enable_peer_reachout: bool,

    /// Request router forwarding for the P2P port. Defaults to true, but startup
    /// skips mapping on Dev and whenever advertise_address is set.
    #[serde(default = "default_true")]
    pub enable_upnp: bool,

    /// IPv4 address advertised in keepalive, optionally followed by :port.
    /// Defaults to the P2P port when omitted. Setting this disables UPnP and
    /// automatic address detection, including when the value fails to parse.
    #[serde(default)]
    pub advertise_address: Option<String>,

    /// Enable representative vote generation. Defaults to true; votes count only
    /// with delegated weight in the applicable electorate. A stable signing
    /// identity requires the representative key file outside test fixtures.
    #[serde(default = "default_true")]
    pub enable_representative: bool,

    /// File containing this representative's 32-byte Ed25519 seed as hex.
    /// On Unix it must deny group/other access (for example, mode 600).
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
    /// Aggregate private recovery file extents, including the durable input
    /// reservation. Optional; when set it must be at least 4 MiB. An enabled
    /// retirement policy supplies this budget if omitted and must agree if set.
    #[serde(default)]
    pub recovery_scratch_bytes: Option<u64>,
    /// Disabled by default and release-gated until retirement qualification passes.
    #[serde(default)]
    pub body_retirement: BodyRetirementConfig,
}

fn default_network() -> NetworkId {
    NetworkId::Dev
}

fn default_drand_relays() -> Vec<String> {
    vec![burst_vrf::drand::DRAND_MAINNET_URL.into()]
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./burst_data")
}

fn default_database_map_size_bytes() -> u64 {
    crate::node::DEFAULT_MAP_SIZE as u64
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

impl NodeConfig {
    pub(crate) fn database_map_size(&self) -> Result<usize, String> {
        let bytes = self.database_map_size_bytes;
        if bytes < 1 << 20 || bytes % (1 << 20) != 0 {
            return Err("database_map_size_bytes must be a positive whole number of MiB".into());
        }
        let size = usize::try_from(bytes)
            .map_err(|_| "database map size exceeds this platform's address space".to_owned())?;
        if size > isize::MAX as usize {
            return Err("database map size exceeds this platform's address space".into());
        }
        Ok(size)
    }

    pub(crate) fn recovery_scratch_budget(&self) -> Result<Option<u64>, String> {
        self.body_retirement.validate()?;
        if self
            .recovery_scratch_bytes
            .is_some_and(|bytes| bytes < 4 << 20)
        {
            return Err("recovery scratch budget must be at least4MiB".into());
        }
        let retirement = self
            .body_retirement
            .enabled
            .then_some(self.body_retirement.max_scratch_bytes)
            .flatten();
        if let (Some(recovery), Some(retirement)) = (self.recovery_scratch_bytes, retirement) {
            if recovery != retirement {
                return Err("recovery and retirement scratch budgets must match".into());
            }
        }
        Ok(self.recovery_scratch_bytes.or(retirement))
    }

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
        config.database_map_size().map_err(NodeError::Config)?;
        config.effective_peer_limits()?;
        config.validate_drand_relays()?;
        config
            .recovery_scratch_budget()
            .map_err(NodeError::Config)?;
        config
            .snapshot_transfer
            .validate()
            .map_err(|error| NodeError::Config(error.to_string()))?;
        Ok(config)
    }

    pub(crate) fn effective_peer_limits(
        &self,
    ) -> Result<burst_network::admission::PeerLimits, NodeError> {
        let limits = self.peer_limits.clone().unwrap_or_else(|| {
            if self.network == burst_types::NetworkId::Live {
                burst_network::admission::PeerLimits::default()
            } else {
                burst_network::admission::PeerLimits::for_test_network(self.max_peers)
            }
        });
        limits
            .validate()
            .map_err(|error| NodeError::Config(error.into()))?;
        self.bandwidth_limits
            .validate()
            .map_err(|error| NodeError::Config(error.into()))?;
        Ok(limits)
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
            database_map_size_bytes: default_database_map_size_bytes(),
            params: ProtocolParams::default(),
            max_peers: default_max_peers(),
            peer_limits: None,
            bandwidth_limits: Default::default(),
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
            recovery_scratch_bytes: None,
            body_retirement: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_limits_have_explicit_cluster_overrides_and_reject_invalid_budgets() {
        let mut config = NodeConfig::default();
        config.network = NetworkId::Live;
        assert_eq!(config.effective_peer_limits().unwrap().per_ip, 4);
        config.network = NetworkId::Dev;
        assert_eq!(
            config.effective_peer_limits().unwrap().per_ip,
            config.max_peers
        );
        config.peer_limits = Some(burst_network::admission::PeerLimits {
            per_ip: 12,
            per_subnet: 24,
            outbound_reserved: 4,
        });
        let decoded = NodeConfig::from_toml_str(&config.to_toml_string()).unwrap();
        assert_eq!(decoded.effective_peer_limits().unwrap().per_ip, 12);
        assert!(NodeConfig::from_toml_str("[peer_limits]\nper_ip = 0\n").is_err());
        assert!(NodeConfig::from_toml_str(
            "[bandwidth_limits]\ntotal_bytes_per_second = 65540\nbulk_bytes_per_second = 65540\n"
        )
        .is_err());
    }

    #[test]
    fn database_map_configuration_defaults_roundtrips_and_rejects_invalid_sizes() {
        assert_eq!(
            NodeConfig::from_toml_str("")
                .unwrap()
                .database_map_size()
                .unwrap(),
            1 << 30
        );
        let bytes = if usize::BITS >= 64 {
            4u64 << 30
        } else {
            1536u64 << 20
        };
        let config =
            NodeConfig::from_toml_str(&format!("database_map_size_bytes = {bytes}\n")).unwrap();
        assert_eq!(config.database_map_size().unwrap() as u64, bytes);
        assert_eq!(
            NodeConfig::from_toml_str(&config.to_toml_string())
                .unwrap()
                .database_map_size_bytes,
            bytes
        );
        for bytes in [0, (1 << 20) - 1, (1 << 30) + 1, u64::MAX] {
            let config = NodeConfig {
                database_map_size_bytes: bytes,
                ..NodeConfig::default()
            };
            assert!(config.database_map_size().is_err());
            assert!(
                NodeConfig::from_toml_str(&format!("database_map_size_bytes = {bytes}\n")).is_err()
            );
        }
    }

    #[test]
    fn recovery_budget_is_optional_and_agrees_with_enabled_retirement() {
        assert_eq!(
            NodeConfig::default().recovery_scratch_budget().unwrap(),
            None
        );
        for bytes in [0, (4 << 20) - 1] {
            assert!(
                NodeConfig::from_toml_str(&format!("recovery_scratch_bytes = {bytes}\n")).is_err()
            );
        }
        let mut config = NodeConfig::from_toml_str("recovery_scratch_bytes = 67108864\n").unwrap();
        assert_eq!(config.recovery_scratch_budget().unwrap(), Some(64 << 20));
        assert!(!config.body_retirement.enabled);
        assert_eq!(
            NodeConfig::from_toml_str(&config.to_toml_string())
                .unwrap()
                .recovery_scratch_budget()
                .unwrap(),
            Some(64 << 20)
        );
        config.body_retirement.enabled = true;
        config.body_retirement.max_scratch_bytes = Some(32 << 20);
        assert!(config.recovery_scratch_budget().is_err());
        config.body_retirement.max_scratch_bytes = Some(64 << 20);
        assert_eq!(config.recovery_scratch_budget().unwrap(), Some(64 << 20));
        config.recovery_scratch_bytes = None;
        assert_eq!(config.recovery_scratch_budget().unwrap(), Some(64 << 20));
        config.body_retirement.enabled = false;
        assert_eq!(config.recovery_scratch_budget().unwrap(), None);
    }

    #[test]
    fn physical_retirement_policy_is_opt_in_and_budgeted() {
        assert!(
            !NodeConfig::from_toml_str("")
                .unwrap()
                .body_retirement
                .enabled
        );
        let parsed = NodeConfig::from_toml_str("[body_retirement]\nenabled = true\nmax_scratch_bytes = 67108864\nretain_seconds = 0\nmax_examined = 1\n").unwrap();
        assert!(parsed.body_retirement.enabled);
        assert_eq!(parsed.body_retirement.max_scratch_bytes, Some(64 << 20));
        for suffix in [
            "",
            "max_scratch_bytes = 0\n",
            "max_scratch_bytes = 4194303\n",
        ] {
            assert!(NodeConfig::from_toml_str(&format!(
                "[body_retirement]\nenabled = true\n{suffix}"
            ))
            .is_err());
        }
        assert_eq!(parsed.body_retirement.max_capture_secs, 120);
        assert_eq!(parsed.body_retirement.max_capture_growth_bytes, 64 << 20);
        assert_eq!(parsed.body_retirement.min_capture_headroom_bytes, 4 << 20);
        assert_eq!(
            NodeConfig::from_toml_str(&parsed.to_toml_string())
                .unwrap()
                .body_retirement
                .max_capture_secs,
            120
        );
        assert_eq!(parsed.body_retirement.max_examined, 1);
        assert_eq!(parsed.body_retirement.retain_seconds, 0);
        for (field, value) in [
            ("max_capture_secs", 0),
            ("max_capture_secs", 121),
            ("max_capture_growth_bytes", 0),
            ("max_capture_growth_bytes", (64 << 20) + 1),
            ("min_capture_headroom_bytes", (4 << 20) - 1),
            ("interval_secs", 0),
            ("interval_secs", 31_536_001),
            ("max_examined", 0),
            ("max_examined", 131_073),
            ("max_bodies", 0),
            ("max_bodies", 131_073),
            ("max_bytes", 0),
            ("max_bytes", (64 << 20) + 1),
        ] {
            assert!(
                NodeConfig::from_toml_str(&format!("[body_retirement]\n{field} = {value}\n"))
                    .is_err()
            );
        }
        assert!(NodeConfig::from_toml_str("[body_retirement]\nmax_body = 1\n").is_err());
    }

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
