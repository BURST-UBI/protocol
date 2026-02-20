//! BURST daemon — entry point for running a BURST node.

use burst_node::NodeConfig;
use burst_types::NetworkId;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "burst-daemon", about = "BURST protocol node daemon")]
struct Cli {
    /// Network to connect to: "live", "test", or "dev".
    #[arg(long, default_value = "dev", env = "BURST_NETWORK")]
    network: String,

    /// Data directory for ledger storage.
    #[arg(long, default_value = "./burst_data", env = "BURST_DATA_DIR")]
    data_dir: PathBuf,

    /// Port for P2P connections (defaults to network default).
    #[arg(long, env = "BURST_P2P_PORT")]
    port: Option<u16>,

    /// Enable RPC server.
    #[arg(long, default_value_t = true, env = "BURST_ENABLE_RPC")]
    rpc: bool,

    /// RPC server port.
    #[arg(long, default_value_t = 7077, env = "BURST_RPC_PORT")]
    rpc_port: u16,

    /// Enable WebSocket server.
    #[arg(long, env = "BURST_ENABLE_WEBSOCKET")]
    websocket: bool,

    /// WebSocket server port.
    #[arg(long, default_value_t = 7078, env = "BURST_WS_PORT")]
    websocket_port: u16,

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

    /// Log level: "trace", "debug", "info", "warn", "error".
    #[arg(long, default_value = "info", env = "BURST_LOG_LEVEL")]
    log_level: String,

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

    let network = match cli.network.as_str() {
        "live" => NetworkId::Live,
        "test" => NetworkId::Test,
        _ => NetworkId::Dev,
    };

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

    let config = if let Some(file_cfg) = file_config {
        NodeConfig {
            network,
            data_dir: cli.data_dir,
            port: cli.port.unwrap_or(file_cfg.port),
            enable_rpc: cli.rpc,
            rpc_port: cli.rpc_port,
            enable_websocket: cli.websocket,
            websocket_port: cli.websocket_port,
            bootstrap_peers: if cli.bootstrap_peers.is_empty() {
                file_cfg.bootstrap_peers
            } else {
                cli.bootstrap_peers
            },
            max_peers: cli.max_peers.unwrap_or(file_cfg.max_peers),
            enable_metrics: cli.metrics || file_cfg.enable_metrics,
            enable_faucet: cli.faucet || file_cfg.enable_faucet,
            log_level: cli.log_level,
            ..file_cfg
        }
    } else {
        NodeConfig {
            network,
            data_dir: cli.data_dir,
            port: cli.port.unwrap_or(network.default_port()),
            enable_rpc: cli.rpc,
            rpc_port: cli.rpc_port,
            enable_websocket: cli.websocket,
            websocket_port: cli.websocket_port,
            bootstrap_peers: cli.bootstrap_peers,
            max_peers: cli.max_peers.unwrap_or(50),
            enable_metrics: cli.metrics,
            enable_faucet: cli.faucet,
            log_level: cli.log_level,
            ..Default::default()
        }
    };

    match cli.command {
        Command::Node { action } => match action {
            NodeAction::Run => {
                tracing::info!(
                    "Starting BURST node on {} network (P2P:{}, RPC:{}, WS:{})",
                    network.as_str(),
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
                    tracing::info!(
                        "Bootstrap peers: {}",
                        config.bootstrap_peers.join(", ")
                    );
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
