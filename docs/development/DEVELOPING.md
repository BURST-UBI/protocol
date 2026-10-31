# Build and test

The protocol is a Rust workspace. `burst-daemon` runs a node;
`burst-wallet-core` contains the client and signing code used by the desktop
wallet. The wallet and dashboard are separate Tauri applications.

## Build the daemon

Install Rust through rustup, Git, and your platform's C/C++ build tools. The
repository's `rust-toolchain.toml` selects the Rust version. Use the committed
Cargo lockfile.

```bash
git clone https://github.com/BURST-UBI/protocol.git
cd protocol
cargo build --locked -p burst-daemon
```

The executable is `target/debug/burst-daemon` (`burst-daemon.exe` on Windows).
Use a debug build for development. A release build uses `--release` and writes
to `target/release/`.

If this checkout has an `AGENTS.md` with local resource limits, follow it when
running these commands. The project's `tools/limited-run.sh` is a Linux systemd
wrapper for that constrained development environment, not a prerequisite for
building BURST on another machine.

## Start a local node

From the protocol directory:

```bash
./target/debug/burst-daemon --network dev --data-dir .dev_docs_node \
  --port 27076 --rpc --rpc-port 7077 --websocket --websocket-port 7078 \
  --faucet --disable-upnp node run
```

This runs in the foreground. Stop it with Ctrl+C. The data directory persists
between runs; use a different directory when you need a separate network or a
fresh ledger. RPC listens on `127.0.0.1:7077`, and WebSocket subscriptions use
`ws://127.0.0.1:7078/ws`.

Check that RPC is responding:

```bash
curl --fail http://127.0.0.1:7077/health
curl http://127.0.0.1:7077/ \
  -H 'Content-Type: application/json' \
  -d '{"action":"telemetry"}'
```

`/health` returns `ok`. `telemetry` reports the selected network, peer count,
account count and other node state. A local Dev node need not have public peers.
For a multi-node network, supply `--bootstrap-peers HOST:PORT` and use the same
network identity on every node.

For file-based configuration, use `--config PATH` before `node run`.
[Node configuration](../node/NODES.md#configuration) lists common settings.

## Create and fund a test wallet

Create the wallet in the desktop application or with the client-side key APIs.
Do not use `wallet_create` RPC to create an account you intend to spend from:
that handler discards the private key.

With `--faucet` enabled on Dev or Test, request a payment to your disposable
wallet address:

```bash
curl http://127.0.0.1:7077/ \
  -H 'Content-Type: application/json' \
  -d '{"action":"faucet","account":"YOUR_DISPOSABLE_WALLET_ADDRESS"}'
```

The faucet burns one BRN from the network's genesis account and sends one TRST.
It needs genesis authority and enough accrued BRN. The recipient still signs a
receive operation. Funding does not verify the recipient.

For a small bootstrap network, receive the payment and set a running
representative before verifying another account. Otherwise its newly added
voting weight may be delegated to an offline representative.
The creator's `genesis_endorse` RPC requires an administrator bearer token;
see [bootstrap permissions](../protocol/BOOTSTRAP.md) and
[RPC access controls](../node/SERVICES.md#rpc-security-boundaries).

## Feature flags

Ordinary verifier selection, appeals, governance content and ledger-backed
voting delegation have release switches. To exercise experimental paths on
Dev or Test, build the daemon with:

```bash
cargo build --locked -p burst-daemon --features experimental-testnet
```

This flag does not enable those paths on Live. See
[feature availability](../protocol/VERIFICATION.md#network-availability).
Keep separate data directories for Dev, Test and Live.

## Run tests

Start with the crate you changed:

```bash
cargo test --locked -p burst-types
cargo test --locked -p burst-brn
cargo test --locked -p burst-trst
```

The node test runner groups the larger suite:

```bash
bash tools/test-node.sh fast
bash tools/test-node.sh slow
bash tools/test-node.sh network
bash tools/test-node.sh focused checkpoints::sync::input::tests::slow_polls_and_busy_inbox_both_make_progress
```

`focused` takes an exact test name. The runner uses the `validation` profile;
`BURST_NODE_TEST_BINARY` can select an already-built matching executable.
Network tests run serially by default. Check each ignored test's prerequisites
before enabling it.

For multi-node payment and restart tests, `run-validation.sh` takes prebuilt
artifacts. It does not compile them:

```bash
bash run-validation.sh --artifacts-dir "$PWD/target/debug"
```

The directory must contain `burst-daemon` and the wallet-core examples
`validate_network`, `validate_restart_payment` and `validate_pending_chain`.
The runner checks ports and saves its logs. Use the
[validation workflow](https://github.com/BURST-UBI/protocol/blob/main/spec/VALIDATION_WORKFLOW.md)
for release-candidate testing and reuse of recorded results.

## Build the desktop applications

Use sibling checkouts:

```text
burst/
  protocol/
  wallet/
  node-gui/
```

In `wallet/` or `node-gui/`, install Node.js, the platform dependencies required
by Tauri, then run:

```bash
npm ci
npm run tauri dev
```

Both applications default to RPC at `http://127.0.0.1:7077`.
`npm run dev` starts only the frontend; it does not provide native key storage
or signing. Use disposable profiles when testing payments or recovery.

Application CI fetches the protocol revision recorded in each application's
`ci/protocol-revision`. If you change a shared Rust API, update the affected
application and its pin together. A local sibling checkout can otherwise hide
an incompatibility with the revision CI builds.

## Documentation changes

Edit `docs/` and its `SUMMARY.md` navigation. Docusaurus in `website/` reads those
files directly; Vercel builds the same commit. There is no separate website
copy to update.

```bash
npm --prefix website ci
npm --prefix website run build
```

The build checks internal links and generates local search. Keep code examples
consistent with the RPC contracts in `rpc/messages/` and the public methods in
`wallet_core/`. Put machine-specific test receipts in engineering records, not
in user-facing setup instructions.
