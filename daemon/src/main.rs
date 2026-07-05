//! BURST daemon — entry point for running a BURST node.

use burst_node::NodeConfig;
use burst_types::NetworkId;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "burst-daemon", about = "BURST protocol node daemon")]
struct Cli {
    /// Network to connect to: "live", "test", or "dev".
    /// When a config file is provided, defaults to the file's network value.
    #[arg(long, env = "BURST_NETWORK")]
    network: Option<String>,

    /// Data directory for ledger storage (defaults to "./burst_data", or the
    /// config-file value).
    #[arg(long, env = "BURST_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Port for P2P connections (defaults to network default).
    #[arg(long, env = "BURST_P2P_PORT")]
    port: Option<u16>,

    /// Enable RPC server.
    #[arg(long, default_value_t = true, env = "BURST_ENABLE_RPC")]
    rpc: bool,

    /// RPC server port (defaults to 7077, or the config-file value).
    #[arg(long, env = "BURST_RPC_PORT")]
    rpc_port: Option<u16>,

    /// Address the RPC server binds to (defaults to "127.0.0.1", or the
    /// config-file value). Use "0.0.0.0" to expose on all interfaces — only
    /// behind a firewall, since the RPC includes genesis_endorse.
    #[arg(long, env = "BURST_RPC_BIND")]
    rpc_bind: Option<String>,

    /// Enable WebSocket server.
    #[arg(long, env = "BURST_ENABLE_WEBSOCKET")]
    websocket: bool,

    /// WebSocket server port (defaults to 7078, or the config-file value).
    #[arg(long, env = "BURST_WS_PORT")]
    websocket_port: Option<u16>,

    /// Bootstrap peer addresses (comma-separated: "1.2.3.4:17076,5.6.7.8:17076").
    #[arg(long, env = "BURST_BOOTSTRAP_PEERS", value_delimiter = ',')]
    bootstrap_peers: Vec<String>,

    /// Maximum number of peer connections.
    #[arg(long, env = "BURST_MAX_PEERS")]
    max_peers: Option<usize>,

    /// Enable Prometheus metrics endpoint.
    #[arg(long, env = "BURST_ENABLE_METRICS")]
    metrics: bool,

    /// Enable testnet faucet endpoint.
    #[arg(long, env = "BURST_ENABLE_FAUCET")]
    faucet: bool,

    /// Disable UPnP port mapping (enabled by default on live/test networks).
    #[arg(long, env = "BURST_DISABLE_UPNP")]
    disable_upnp: bool,

    /// Log level: "trace", "debug", "info", "warn", "error" (defaults to "info", or the config-file value).
    #[arg(long, env = "BURST_LOG_LEVEL")]
    log_level: Option<String>,

    /// Path to a TOML configuration file. If provided, file settings
    /// are used as the base; CLI flags and env vars override them.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Subcommand.
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start the node.
    #[command(name = "node")]
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
}

#[derive(clap::Subcommand)]
enum NodeAction {
    /// Run the node.
    Run,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    burst_utils::init_tracing();

    let cli = Cli::parse();

    fn parse_network(s: &str) -> NetworkId {
        match s.to_lowercase().as_str() {
            "live" => NetworkId::Live,
            "test" => NetworkId::Test,
            _ => NetworkId::Dev,
        }
    }

    let cli_network = cli.network.as_deref().map(parse_network);

    let file_config: Option<NodeConfig> = if let Some(ref config_path) = cli.config {
        match std::fs::read_to_string(config_path) {
            Ok(contents) => match toml::from_str::<NodeConfig>(&contents) {
                Ok(cfg) => {
                    tracing::info!("Loaded config from {}", config_path.display());
                    Some(cfg)
                }
                Err(e) => {
                    tracing::warn!("Failed to parse config file: {e}, using CLI defaults");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read config file {}: {e}, using CLI defaults",
                    config_path.display()
                );
                None
            }
        }
    } else {
        None
    };

    let enable_upnp = !cli.disable_upnp;

    let config = if let Some(file_cfg) = file_config {
        let network = cli_network.unwrap_or(file_cfg.network);
        NodeConfig {
            network,
            data_dir: cli.data_dir.unwrap_or(file_cfg.data_dir),
            port: cli.port.unwrap_or(file_cfg.port),
            enable_rpc: cli.rpc,
            rpc_port: cli.rpc_port.unwrap_or(file_cfg.rpc_port),
            rpc_bind: cli.rpc_bind.unwrap_or(file_cfg.rpc_bind),
            enable_websocket: cli.websocket,
            websocket_port: cli.websocket_port.unwrap_or(file_cfg.websocket_port),
            bootstrap_peers: if cli.bootstrap_peers.is_empty() {
                file_cfg.bootstrap_peers
            } else {
                cli.bootstrap_peers
            },
            max_peers: cli.max_peers.unwrap_or(file_cfg.max_peers),
            enable_metrics: cli.metrics || file_cfg.enable_metrics,
            enable_faucet: cli.faucet || file_cfg.enable_faucet,
            enable_upnp: enable_upnp && file_cfg.enable_upnp,
            log_level: cli.log_level.unwrap_or(file_cfg.log_level),
            ..file_cfg
        }
    } else {
        let network = cli_network.unwrap_or(NetworkId::Dev);
        NodeConfig {
            network,
            data_dir: cli
                .data_dir
                .unwrap_or_else(|| PathBuf::from("./burst_data")),
            port: cli.port.unwrap_or(network.default_port()),
            enable_rpc: cli.rpc,
            rpc_port: cli.rpc_port.unwrap_or(7077),
            rpc_bind: cli.rpc_bind.unwrap_or_else(|| "127.0.0.1".to_string()),
            enable_websocket: cli.websocket,
            websocket_port: cli.websocket_port.unwrap_or(7078),
            bootstrap_peers: cli.bootstrap_peers,
            max_peers: cli.max_peers.unwrap_or(50),
            enable_metrics: cli.metrics,
            enable_faucet: cli.faucet,
            enable_upnp,
            log_level: cli.log_level.unwrap_or_else(|| "info".to_string()),
            ..Default::default()
        }
    };

    // The faucet mints verified wallets and unbacked TRST — a testnet-only
    // convenience. It must never run on the live network (it would violate the
    // "TRST only from burned BRN" invariant), so force it off there regardless
    // of flags or config file.
    let mut config = config;
    if config.network == NetworkId::Live && config.enable_faucet {
        tracing::warn!("faucet requested on the live network — refusing (faucet is testnet-only)");
        config.enable_faucet = false;
    }
    if config.network == NetworkId::Live && !burst_node::genesis_key::live_identity_configured() {
        tracing::error!(
            "live network selected but the genesis public identity is not configured              (still the all-zeros placeholder). Generate one with `genesis_keygen` and bake              the public key into node/src/genesis_key.rs before launching a real network."
        );
    }

    match cli.command {
        Command::Node { action } => match action {
            NodeAction::Run => {
                tracing::info!(
                    "Starting BURST node on {} network (P2P:{}, RPC:{}, WS:{})",
                    config.network.as_str(),
                    config.port,
                    if config.enable_rpc {
                        config.rpc_port.to_string()
                    } else {
                        "off".into()
                    },
                    if config.enable_websocket {
                        config.websocket_port.to_string()
                    } else {
                        "off".into()
                    },
                );
                if !config.bootstrap_peers.is_empty() {
                    tracing::info!("Bootstrap peers: {}", config.bootstrap_peers.join(", "));
                }

                let mut config = config;
                if config.network == NetworkId::Test {
                    config.params = burst_types::ProtocolParams::testnet_defaults();
                    tracing::info!("using fast governance timelines for testnet");
                }
                let mut node = burst_node::BurstNode::new(config).await?;
                node.start().await?;

                tracing::info!("Shutdown signal received — stopping node");
                node.stop().await?;

                tracing::info!("BURST daemon exited cleanly");
            }
        },
    }

    Ok(())
}
