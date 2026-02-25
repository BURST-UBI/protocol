# BURST Protocol

A parameterized economic protocol where every monetary distribution model is a configuration.

BURST is defined by two parameters:
- **r** — the BRN accrual rate (universal income speed)
- **e** — the TRST expiry period (how long earned currency remains transferable)

Normal money is BURST with r=0 and e=∞. Every other economic model is a different configuration.

## Quick Start

### Linux

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh
```

### macOS

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sh
```

### Windows (PowerShell as Administrator)

```powershell
irm https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.ps1 | iex
```

That's it. Each script downloads a pre-built binary, writes a default config, registers a background service, and enables auto-updates. No Docker, no build tools, no dependencies.

### What it sets up

| | Linux | macOS | Windows |
|---|---|---|---|
| Binary | `/usr/local/bin/burst-daemon` | `/usr/local/bin/burst-daemon` | `C:\ProgramData\BURST\burst-daemon.exe` |
| Config | `/etc/burst/config.toml` | `~/Library/Application Support/BURST/config.toml` | `C:\ProgramData\BURST\config.toml` |
| Data | `/var/lib/burst` | `~/Library/Application Support/BURST/data` | `C:\ProgramData\BURST\data` |
| Service | systemd | launchd | Windows Service |
| Auto-update | systemd timer (5 min) | launchd (5 min) | Scheduled Task (5 min) |

### Common commands

**Linux:**
```bash
journalctl -u burst-node -f         # view logs
systemctl status burst-node          # check status
sudo systemctl restart burst-node    # restart
```

**macOS:**
```bash
tail -f ~/Library/Application\ Support/BURST/burst-node.log   # view logs
launchctl kickstart -k gui/$(id -u)/com.burst.node             # restart
```

**Windows (PowerShell):**
```powershell
Get-Service BurstNode                # check status
Restart-Service BurstNode            # restart
```

### Uninstall

**Linux:** `curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh -s -- --uninstall`

**macOS:** `curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sh -s -- --uninstall`

**Windows:** Run `install.ps1 --uninstall` in an elevated PowerShell.

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

Cross-compile for Linux arm64:
```bash
cross build --release -p burst-daemon --target aarch64-unknown-linux-musl
```

## Running a Dev Node

```bash
cargo run --bin burst-daemon -- node run
```

With a config file:
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

## Supported Platforms

| Platform | Architecture | Build |
|----------|-------------|-------|
| Linux | x86_64 | Static (musl) |
| Linux | arm64 (RPi4) | Static (musl) |
| macOS | Apple Silicon (M1/M2/M3/M4) | Native |
| macOS | Intel | Native |
| Windows | x86_64 | Native (MSVC) |

All platforms use rustls for TLS (no OpenSSL dependency).

## Testnet

The public testnet runs 5 VPS nodes + any number of community nodes. The seed node is at `167.172.83.88:17076`. Nodes installed via the install scripts automatically connect to it.

## License

MIT License. Copyright 2025 Nitesh Gautam.

See [the whitepaper](../burst_source_of_truth/BURST_WHITEPAPER.pdf) for the full design.
