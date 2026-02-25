# BURST Protocol

A parameterized economic protocol where every monetary distribution model is a configuration.

BURST is defined by two parameters:
- **r** — the BRN accrual rate (universal income speed)
- **e** — the TRST expiry period (how long earned currency remains transferable)

Normal money is BURST with r=0 and e=∞. Every other economic model is a different configuration.

## Quick Start

Install a BURST node on any Linux machine (x86_64 or aarch64):

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh
```

That's it. The script downloads a static binary, creates a systemd service, and enables auto-updates. No Docker, no build tools, no dependencies.

### What it sets up

| Component | Path |
|-----------|------|
| Binary | `/usr/local/bin/burst-daemon` |
| Config | `/etc/burst/config.toml` |
| Data | `/var/lib/burst` |
| Service | `burst-node.service` (systemd) |
| Auto-update | `burst-update.timer` (checks every 5 min) |

### Common commands

```bash
journalctl -u burst-node -f        # view logs
systemctl status burst-node         # check status
sudo systemctl restart burst-node   # restart
sudo nano /etc/burst/config.toml    # edit config
```

### Uninstall

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh -s -- --uninstall
```

## Architecture

Rust workspace organized into modular crates:

| Layer | Crates | Purpose |
|-------|--------|---------|
| Foundation | `types`, `crypto`, `utils` | Fundamental types, Ed25519/Blake2b crypto, shared utilities |
| Core Protocol | `brn`, `trst`, `transactions` | BRN computation, TRST lifecycle + merger graph, all transaction types |
| Storage | `store`, `store_lmdb`, `ledger` | Abstract storage traits, LMDB backend, DAG block-lattice |
| Verification | `verification`, `vrf`, `consensus`, `work` | Humanity verification, VRF, double-spend resolution, anti-spam PoW |
| Governance | `governance`, `consti` | 4-phase democratic governance, on-chain constitution |
| Networking | `messages`, `protocol`, `network` | Message types, wire protocol, P2P networking + NAT traversal |
| Application | `node`, `daemon`, `rpc`, `websocket` | Node orchestration, CLI entry point, JSON-RPC, WebSocket |
| Client | `wallet_core`, `groups` | Wallet library, group trust layer |
| Testing | `nullables` | Nullable infrastructure for deterministic testing |

## Building from Source

```bash
cargo build --release -p burst-daemon
```

The binary is at `target/release/burst-daemon`. For cross-compilation to arm64:

```bash
cross build --release -p burst-daemon --target aarch64-unknown-linux-musl
```

## Running a Dev Node

```bash
cargo run --bin burst-daemon -- node run
```

Or with a config file:

```bash
cargo run --bin burst-daemon -- --config testnet.toml node run
```

## Docker

Multi-arch Docker images (amd64 + arm64) are published on every push to `main`:

```bash
docker run -d --name burst \
  -p 17076:17076 -p 7077:7077 \
  -v burst-data:/home/burst/data \
  -e BURST_NETWORK=test \
  ghcr.io/burst-ubi/protocol:latest
```

## Testnet

The public testnet runs 5 VPS nodes + any number of community nodes. The seed node is at `167.172.83.88:17076`. Nodes installed via the install script automatically connect to it.

## License

MIT License. Copyright 2025 Nitesh Gautam.

See [the whitepaper](../burst_source_of_truth/BURST_WHITEPAPER.pdf) for the full design.
