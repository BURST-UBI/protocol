//! BURST daemon — entry point for running a BURST node.

use burst_node::NodeConfig;
use burst_types::NetworkId;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "burst-daemon", about = "BURST protocol node daemon")]
struct Cli {
    /// Network to connect to.
    #[arg(long, default_value = "dev")]
    network: String,

    /// Data directory for ledger storage.
    #[arg(long, default_value = "./burst_data")]
    data_dir: PathBuf,

    /// Port for P2P connections.
    #[arg(long)]
    port: Option<u16>,

    /// Enable RPC server.
    #[arg(long, default_value_t = true)]
    rpc: bool,

    /// RPC server port.
    #[arg(long, default_value_t = 7077)]
    rpc_port: u16,

    /// Enable WebSocket server.
    #[arg(long)]
    websocket: bool,

    /// WebSocket server port.
    #[arg(long, default_value_t = 7078)]
    websocket_port: u16,

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

    let config = NodeConfig {
        network,
        data_dir: cli.data_dir,
        port: cli.port.unwrap_or(network.default_port()),
        enable_rpc: cli.rpc,
        rpc_port: cli.rpc_port,
        enable_websocket: cli.websocket,
        websocket_port: cli.websocket_port,
        ..Default::default()
    };

    match cli.command {
        Command::Node { action } => match action {
            NodeAction::Run => {
                tracing::info!("Starting BURST node on {} network", network.as_str());
                let mut node = burst_node::BurstNode::new(config).await?;
                node.start().await?;
            }
        },
    }

    Ok(())
}
