# BURST Protocol

A parameterized economic protocol where every monetary distribution model is a configuration.

BURST is defined by two parameters:
- **r** — the BRN accrual rate (universal income speed)
- **e** — the TRST expiry period (how long earned currency remains transferable)

Normal money is BURST with r=0 and e=∞. Every other economic model is a different configuration.

## Architecture

This is a Rust workspace organized into modular crates:

| Layer | Crates | Purpose |
|-------|--------|---------|
| Foundation | `types`, `crypto`, `utils` | Fundamental types, Ed25519/Blake2b crypto, shared utilities |
| Core Protocol | `brn`, `trst`, `transactions` | BRN computation, TRST lifecycle + merger graph, all transaction types |
| Storage | `store`, `store_lmdb`, `ledger` | Abstract storage traits, LMDB backend, DAG block-lattice |
| Verification | `verification`, `vrf`, `consensus`, `work` | Humanity verification, VRF, double-spend resolution, anti-spam PoW |
| Governance | `governance`, `consti` | 4-phase democratic governance, on-chain constitution |
| Networking | `messages`, `protocol`, `network` | Message types, wire protocol, P2P networking |
| Application | `node`, `daemon`, `rpc`, `websocket` | Node orchestration, CLI entry point, JSON-RPC, WebSocket |
| Client | `wallet_core`, `groups` | Wallet library, group trust layer |
| Testing | `nullables` | Nullable infrastructure for deterministic testing |

## Building

```bash
cargo build
```

## Running a Node

```bash
cargo run --bin burst-daemon -- --network=dev node run
```

## License

MIT License. Copyright 2025 Nitesh Gautam.

See [the whitepaper](../burst_source_of_truth/BURST_WHITEPAPER.pdf) for the full design.
