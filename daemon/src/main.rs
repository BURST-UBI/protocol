//! BURST daemon — entry point for running a BURST node.

use anyhow::Context;
use burst_node::NodeConfig;
use burst_types::NetworkId;
use clap::Parser;
use std::path::PathBuf;

mod snapshot;
mod genesis_keygen;

#[cfg(windows)]
mod service;

#[derive(Parser)]
#[command(name = "burst-daemon", about = "BURST protocol node daemon")]
struct Cli {
    /// Run under the Windows Service Control Manager (used by the installer).
    #[cfg(windows)]
    #[arg(long)]
    windows_service: bool,

    /// Network to connect to: "live", "test", or "dev".
    /// When a config file is provided, defaults to the file's network value.
    #[arg(long, env = "BURST_NETWORK", value_parser = parse_network)]
    network: Option<NetworkId>,

    /// Data directory for ledger storage (defaults to "./burst_data", or the
    /// config-file value).
    #[arg(long, env = "BURST_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// File containing the representative's private seed (64 hex characters, mode 600).
    #[arg(long, env = "BURST_REPRESENTATIVE_KEY_FILE")]
    representative_key_file: Option<PathBuf>,

    /// Follow the ledger without signing consensus votes.
    #[arg(long)]
    no_representative: bool,

    /// Port for P2P connections (defaults to network default).
    #[arg(long, env = "BURST_P2P_PORT")]
    port: Option<u16>,

    /// Enable RPC server.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true", env = "BURST_ENABLE_RPC")]
    rpc: Option<bool>,

    /// RPC server port (defaults to 7077, or the config-file value).
    #[arg(long, env = "BURST_RPC_PORT")]
    rpc_port: Option<u16>,

    /// Address the RPC server binds to (defaults to "127.0.0.1", or the
    /// config-file value). Use "0.0.0.0" to expose on all interfaces — only
    /// behind a firewall, since the RPC includes genesis_endorse.
    #[arg(long, env = "BURST_RPC_BIND")]
    rpc_bind: Option<String>,

    /// Enable WebSocket server.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true", env = "BURST_ENABLE_WEBSOCKET")]
    websocket: Option<bool>,

    /// WebSocket server port (defaults to 7078, or the config-file value).
    #[arg(long, env = "BURST_WS_PORT")]
    websocket_port: Option<u16>,

    /// Event subscription bind address (defaults to localhost or the config value).
    #[arg(long, env = "BURST_WS_BIND")]
    websocket_bind: Option<String>,

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
    /// Generate a network creator identity without starting a node.
    GenesisKeygen(genesis_keygen::Options),
    /// Attest, export or verify an authenticated economic snapshot.
    Snapshot {
        #[command(subcommand)]
        action: snapshot::Action,
    },
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

fn parse_network(value: &str) -> Result<NetworkId, String> {
    match value.to_ascii_lowercase().as_str() {
        "live" => Ok(NetworkId::Live),
        "test" => Ok(NetworkId::Test),
        "dev" => Ok(NetworkId::Dev),
        _ => Err("network must be live, test or dev".into()),
    }
}

fn resolve_config(cli: Cli) -> anyhow::Result<(NodeConfig, Command)> {
    let cli_network = cli.network;

    let file_config: Option<NodeConfig> = if let Some(ref config_path) = cli.config {
        let contents = std::fs::read_to_string(config_path)
            .with_context(|| format!("cannot read node config {}", config_path.display()))?;
        let config = toml::from_str::<NodeConfig>(&contents)
            .with_context(|| format!("cannot parse node config {}", config_path.display()))?;
        Some(config)
    } else {
        None
    };

    let enable_upnp = !cli.disable_upnp;

    let config = if let Some(file_cfg) = file_config {
        let network = cli_network.unwrap_or(file_cfg.network);
        NodeConfig {
            network,
            data_dir: cli.data_dir.unwrap_or(file_cfg.data_dir),
            representative_key_file: cli
                .representative_key_file
                .or(file_cfg.representative_key_file),
            enable_representative: file_cfg.enable_representative && !cli.no_representative,
            port: cli.port.unwrap_or(file_cfg.port),
            enable_rpc: cli.rpc.unwrap_or(file_cfg.enable_rpc),
            rpc_port: cli.rpc_port.unwrap_or(file_cfg.rpc_port),
            rpc_bind: cli.rpc_bind.unwrap_or(file_cfg.rpc_bind),
            enable_websocket: cli.websocket.unwrap_or(file_cfg.enable_websocket),
            websocket_port: cli.websocket_port.unwrap_or(file_cfg.websocket_port),
            websocket_bind: cli.websocket_bind.unwrap_or(file_cfg.websocket_bind),
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
            representative_key_file: cli.representative_key_file,
            enable_representative: !cli.no_representative,
            port: cli.port.unwrap_or(network.default_port()),
            enable_rpc: cli.rpc.unwrap_or(true),
            rpc_port: cli.rpc_port.unwrap_or(7077),
            rpc_bind: cli.rpc_bind.unwrap_or_else(|| "127.0.0.1".to_string()),
            enable_websocket: cli.websocket.unwrap_or(false),
            websocket_port: cli.websocket_port.unwrap_or(7078),
            websocket_bind: cli.websocket_bind.unwrap_or_else(|| "127.0.0.1".into()),
            bootstrap_peers: cli.bootstrap_peers,
            max_peers: cli.max_peers.unwrap_or(50),
            enable_metrics: cli.metrics,
            enable_faucet: cli.faucet,
            enable_upnp,
            log_level: cli.log_level.unwrap_or_else(|| "info".to_string()),
            ..Default::default()
        }
    };

    // The faucet supplies test funding, never verification or vote delegation.
    // Force it off on Live regardless of
    // flags or config file; the RPC handlers independently enforce this too.
    let mut config = config;
    // Offline snapshot authentication and node startup need the same genesis rules.
    match config.network {
        NetworkId::Test => config.params = burst_types::ProtocolParams::testnet_defaults(),
        NetworkId::Dev => config.params = burst_types::ProtocolParams::devnet_defaults(),
        NetworkId::Live => {}
    }
    if config.network == NetworkId::Live && config.enable_faucet {
        tracing::warn!("faucet requested on the live network — refusing (faucet is testnet-only)");
        config.enable_faucet = false;
    }
    if config.network == NetworkId::Live && !burst_node::genesis_key::live_identity_configured() {
        tracing::error!(
            "live network selected but the genesis public identity is not configured              (still the all-zeros placeholder). Generate one with `burst-daemon genesis-keygen live --seed-file PATH` and bake              the public key into node/src/genesis_key.rs before launching a real network."
        );
    }

    Ok((config, cli.command))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    burst_utils::init_tracing();
    let cli = Cli::parse();
    #[cfg(windows)]
    if cli.windows_service {
        return service::dispatch();
    }
    run(cli, None, None).await
}

async fn wait_for_stop(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

async fn run(
    cli: Cli,
    stop: Option<tokio::sync::watch::Receiver<bool>>,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    // Key generation needs no existing configuration, ledger or network identity.
    if let Command::GenesisKeygen(options) = &cli.command {
        return genesis_keygen::run(options);
    }
    let (config, command) = resolve_config(cli)?;

    match command {
        Command::GenesisKeygen(_) => unreachable!("key generation returned before configuration"),
        Command::Snapshot { action } => snapshot::run(&config, action)?,
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

                match config.network {
                    NetworkId::Test => {
                        tracing::info!("using fast governance timelines for testnet");
                    }
                    NetworkId::Dev => {
                        tracing::info!("using development proof-of-work difficulty");
                    }
                    NetworkId::Live => {}
                }
                // Keep startup state off the supervisor/Windows service stack.
                let mut node = Box::pin(burst_node::BurstNode::new(config)).await?;
                if let Some(ready) = ready {
                    let _ = ready.send(());
                }
                let started = if let Some(mut stop) = stop {
                    let shutdown = node.shutdown.clone();
                    let mut start = Box::pin(node.start_with_services(burst_daemon::start_public_services));
                    tokio::select! {
                        result = &mut start => result,
                        _ = wait_for_stop(&mut stop) => {
                            shutdown.shutdown();
                            start.await
                        }
                    }
                } else {
                    Box::pin(node.start_with_services(burst_daemon::start_public_services)).await
                };

                tracing::info!("Node run ended; stopping subsystems");
                let stopped = node.stop().await;
                started?;
                stopped?;

                tracing::info!("BURST daemon exited cleanly");
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn genesis_keygen_needs_no_node_config_or_data_directory() {
        let directory = tempfile::tempdir().unwrap();
        let missing_config = directory.path().join("missing.toml");
        let data = directory.path().join("unused-ledger");
        let seed = directory.path().join("genesis.seed");
        let cli = Cli::try_parse_from([
            "burst-daemon", "--config", missing_config.to_str().unwrap(),
            "--data-dir", data.to_str().unwrap(), "genesis-keygen", "test",
            "--seed-file", seed.to_str().unwrap(),
        ]).unwrap();
        run(cli, None, None).await.unwrap();
        assert!(seed.is_file());
        assert!(!missing_config.exists());
        assert!(!data.exists());
    }

    #[test]
    fn genesis_keygen_cli_requires_network_and_one_output_mode() {
        for args in [
            vec!["burst-daemon", "genesis-keygen"],
            vec!["burst-daemon", "genesis-keygen", "test"],
            vec!["burst-daemon", "genesis-keygen", "typo", "--show-secret"],
            vec!["burst-daemon", "genesis-keygen", "test", "--show-secret", "--seed-file", "seed"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(Cli::try_parse_from([
            "burst-daemon", "genesis-keygen", "live", "--show-secret",
        ]).is_ok());
    }


    #[test]
    fn node_supervisor_future_does_not_embed_startup_state() {
        let cli = Cli::try_parse_from(["burst-daemon", "node", "run"]).unwrap();
        let future = run(cli, None, None);
        assert!(std::mem::size_of_val(&future) < 64 * 1024);
    }

    #[tokio::test]
    async fn stop_signal_is_latched_and_sender_loss_also_stops() {
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_stop(&mut receiver),
        )
        .await
        .unwrap();
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        drop(sender);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_stop(&mut receiver),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn supervised_stop_drains_the_node_and_allows_the_same_ledger_to_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("node.toml");
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join("ledger"),
            port: 0,
            enable_rpc: false,
            enable_websocket: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: Vec::new(),
            ..NodeConfig::default()
        };
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        for _ in 0..2 {
            let (stop, receiver) = tokio::sync::watch::channel(false);
            let (ready, started) = tokio::sync::oneshot::channel();
            let command = cli(&["--config", config_path.to_str().unwrap()]);
            let worker = tokio::spawn(run(command, Some(receiver), Some(ready)));
            tokio::time::timeout(std::time::Duration::from_secs(15), started)
                .await
                .unwrap()
                .unwrap();
            stop.send(true).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(15), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }

    fn cli(args: &[&str]) -> Cli {
        let mut full = vec!["burst-daemon"];
        full.extend_from_slice(args);
        full.extend(["node", "run"]);
        Cli::try_parse_from(full).unwrap()
    }

    fn configured(text: &str, overrides: &[&str]) -> anyhow::Result<NodeConfig> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("node.toml");
        std::fs::write(&path, text).unwrap();
        let mut args = vec!["--config", path.to_str().unwrap()];
        args.extend_from_slice(overrides);
        resolve_config(cli(&args)).map(|(config, _)| config)
    }

    #[test]
    fn unknown_network_never_becomes_devnet() {
        for value in ["", "main", "production", "lve", "test "] {
            assert!(
                Cli::try_parse_from(["burst-daemon", "--network", value, "node", "run"]).is_err()
            );
        }
        for (value, network) in [
            ("LIVE", NetworkId::Live),
            ("Test", NetworkId::Test),
            ("dev", NetworkId::Dev),
        ] {
            assert_eq!(
                resolve_config(cli(&["--network", value]))
                    .unwrap()
                    .0
                    .network,
                network
            );
        }
    }

    #[test]
    fn snapshot_and_node_commands_use_the_same_network_genesis_rules() {
        for (network, expected) in [
            ("test", burst_types::ProtocolParams::testnet_defaults()),
            ("dev", burst_types::ProtocolParams::devnet_defaults()),
            ("live", burst_types::ProtocolParams::default()),
        ] {
            let node = resolve_config(cli(&["--network", network])).unwrap().0;
            let snapshot = Cli::try_parse_from([
                "burst-daemon",
                "--network",
                network,
                "snapshot",
                "verify",
                "--input",
                "unused.snapshot",
            ])
            .unwrap();
            let snapshot = resolve_config(snapshot).unwrap().0;
            assert_eq!(node.params.params_hash(), expected.params_hash());
            assert_eq!(snapshot.params.params_hash(), expected.params_hash());
            let file = configured(&format!("network = '{:?}'", node.network), &[]).unwrap();
            assert_eq!(file.params.params_hash(), expected.params_hash());
        }
    }

    #[test]
    fn explicit_missing_or_invalid_config_never_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.toml");
        assert!(resolve_config(cli(&["--config", path.to_str().unwrap()])).is_err());
        for text in ["network = 'Lvie'", "port = 'wrong'", "not a TOML file"] {
            assert!(configured(text, &[]).is_err());
        }
    }

    #[test]
    fn absent_flags_preserve_file_network_and_server_settings() {
        let config = configured("network = 'Test'\nport = 17100\nenable_rpc = false\nenable_websocket = true\nrpc_bind = '127.0.0.2'", &[]).unwrap();
        assert_eq!(config.network, NetworkId::Test);
        assert_eq!(config.port, 17100);
        assert!(!config.enable_rpc);
        assert!(config.enable_websocket);
        assert_eq!(config.rpc_bind, "127.0.0.2");
    }

    #[test]
    fn explicit_server_flags_override_config_in_both_directions() {
        let text = "network = 'Test'\nenable_rpc = false\nenable_websocket = true";
        let enabled = configured(text, &["--rpc", "--websocket"]).unwrap();
        assert!(enabled.enable_rpc && enabled.enable_websocket);
        let disabled = configured(text, &["--rpc=false", "--websocket=false"]).unwrap();
        assert!(!disabled.enable_rpc && !disabled.enable_websocket);
    }

    #[test]
    fn event_bind_is_local_unless_explicitly_configured() {
        assert_eq!(
            resolve_config(cli(&[])).unwrap().0.websocket_bind,
            "127.0.0.1"
        );
        assert_eq!(
            configured("websocket_bind = '127.0.0.2'", &[])
                .unwrap()
                .websocket_bind,
            "127.0.0.2"
        );
        assert_eq!(
            configured("websocket_bind = '127.0.0.2'", &["--websocket-bind", "::1"])
                .unwrap()
                .websocket_bind,
            "::1"
        );
    }

    #[test]
    fn live_never_enables_test_funding_from_flags_or_config() {
        let config = configured("network = 'Live'\nenable_faucet = true", &["--faucet"]).unwrap();
        assert!(!config.enable_faucet);
        let config = resolve_config(cli(&["--network", "live", "--faucet"]))
            .unwrap()
            .0;
        assert!(!config.enable_faucet);
    }
}
