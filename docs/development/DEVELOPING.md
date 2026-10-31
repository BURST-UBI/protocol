# Build and test

Run protocol commands from the protocol repository. Keep the wallet and
node dashboard in sibling directories when using the examples below.

## Toolchain and build

The workspace pins its Rust toolchain in
[`rust-toolchain.toml`](https://github.com/BURST-UBI/protocol/blob/main/rust-toolchain.toml).
Use the committed Cargo lockfile. Desktop applications also need Node.js, npm
and the native dependencies required by their Tauri build.

On the development desktop, run builds and tests through the resource wrapper:

```bash
bash tools/limited-run.sh cargo check --locked -p burst-types
```

The wrapper caps the whole job at one CPU equivalent, 3 GiB RAM and no swap.
It admits one job at a time, checks available memory and stops after 20 minutes.
Do not raise these limits to force a build through. Full node test compilation
and optimized builds belong on a suitably provisioned validation host.

Before an expensive check, read the
[validation workflow](https://github.com/BURST-UBI/protocol/blob/main/spec/VALIDATION_WORKFLOW.md).
Reuse a matching executable and its verified evidence where possible. Source,
feature set, toolchain and build profile determine whether that evidence applies.

`tools/` contains the CI checks, node test runner, resource wrapper and installer
checks. CI runs `tools/*.test.mjs`; the PowerShell checks run on Windows.
`tools/fixtures/` contains fake Cargo and system commands used to test the runners
without compiling Rust or starting services. They are not protocol fixtures.

`tools/validation/` contains manual candidate-build, package, snapshot and
genesis-key checks. These need the artifacts or prepared ledger
specified by each script. Linux candidate tools expect sibling `protocol`, `wallet`
and `node-gui` checkouts and an explicitly selected validation host. They do not
publish. Package checks invoke the wallet and dashboard repositories' own native
sessions; WebDriver setup and UI security assertions belong to those applications.
Use `--embedded` with each app's `tests/native-session.mjs` for packaged CSP and
IPC checks. The Windows archive runner accepts only a new source directory; for an
existing workspace, follow its recorded validation plan instead of overlaying files.

The Rust account-history checker reuses `NodeClient` and the shared RPC contracts:

```bash
bash tools/limited-run.sh cargo run --locked -p burst-wallet-core \
  --example verify_account_history -- archive.json http://127.0.0.1:7077
```

It compares the complete archive against page sizes 1, 2 and 25, rejects repeated
cursors, and checks that the reads leave balances and authority unchanged. The
shared client uses its normal 30-second request timeout.

## Start a disposable node

Given an existing compatible daemon in `target/debug/`, run:

```bash
./target/debug/burst-daemon --network dev --data-dir .dev_docs_node \
  --port 27076 --rpc --rpc-port 7077 --websocket --websocket-port 7078 \
  --faucet --disable-upnp node run
```

This command starts a foreground process. Stop it with Ctrl+C. Its data directory
survives restart. Choose a different directory for a fresh ledger; do not erase
an existing database just to get past a compatibility error.

To use a TOML configuration, pass `--config PATH` before `node run`. CLI flags and
the supported environment variables override file settings. Use `--help` on the
exact executable for its available options.

`dev-user.sh --no-build` is another local launcher, but it specifically expects
`target/release/burst-daemon`. Without `--no-build` it starts a release build.
Do not invoke that default build path on the constrained desktop.

A database is bound to its network identity. Use separate directories for Dev,
Test and Live. Peer compatibility is defined by `PROTOCOL_VERSION` in
[`types/src/lib.rs`](https://github.com/BURST-UBI/protocol/blob/main/types/src/lib.rs);
signed blocks have their own format version. Rebuild dependent apps and the node
together after an incompatible shared-schema change. The node does not
implicitly migrate an incompatible ledger.

## Connect the applications

In each app's directory, install its locked dependencies with `npm ci`, then run
`npm run tauri dev`. Use the protocol resource wrapper for those jobs on the
constrained desktop, and run only one compiler at a time.

The wallet and dashboard default to RPC at `http://127.0.0.1:7077`.
`npm run dev` alone starts a browser preview; it does not provide native key
storage or signing. Use a disposable app profile for native tests.

## Application CI dependencies

The wallet and node dashboard use path dependencies into a sibling protocol
checkout. Their native CI jobs fetch the protocol commit recorded in each
application's `ci/protocol-revision`; local sibling edits are not part of that
checkout. The dashboard also accepts a `protocol_revision` input for manual runs,
validated as a full lowercase 40-character commit SHA. The wallet's native job
reads its pin directly.

When changing a shared dependency, qualify the coherent application and protocol
sources together. Before selecting a new CI pin, ensure that commit is available
to the checkout action and contains the required crates. Update each affected
application's pin and lockfile as needed. A successful local build does not prove
that the pinned remote checkout contains uncommitted changes.

The dashboard preflight checks for `http_client/Cargo.toml` and runs Cargo metadata.
It does not replace compilation or the later `cargo test --locked --lib`. Browser
checks use mocked IPC and cannot qualify native signing, storage or OS integration.
See the [dashboard workflow](https://github.com/BURST-UBI/node-gui/blob/main/.github/workflows/ci.yml)
and [wallet workflow](https://github.com/BURST-UBI/wallet/blob/main/.github/workflows/ci.yml)
for the actual job conditions and commands.

## Funding and verification

The Dev/Test faucet requires explicit opt-in. Live denies it. For a disposable
wallet, submit its public address:

```bash
curl --max-time 35 http://127.0.0.1:7077 \
  -H 'Content-Type: application/json' \
  -d '{"action":"faucet","account":"YOUR_DISPOSABLE_WALLET_ADDRESS"}'
```

The faucet burns BRN from the test genesis account and creates a pending payment.
The recipient must sign a receive, or let its wallet's configured acceptance
policy do so. Receiving funds does not verify the recipient; unverified accounts
can receive but cannot spend.

During [bootstrap](../protocol/BOOTSTRAP.md), the creator can endorse eligible wallets through
`genesis_endorse`. This administration action needs a bearer token; see
[RPC authorization](../node/SERVICES.md#rpc-security-boundaries). Never send a wallet's
private key with a funding or endorsement request.

Set a running representative and confirm that choice before verifying another
wallet in a small test network. A verified person represented by an offline node
can leave the network without enough available voting weight. Normal verification
uses the [certified selection path](../protocol/VERIFICATION.md#network-availability), with
its network gates still applying.

If submission times out after returning a block hash, inspect that action's
outcome before retrying. See [pending actions and recovery](../wallet/WALLETS.md).

## Choose a test

Small crate tests can run locally through `tools/limited-run.sh`. Node-library
tests use `tools/test-node.sh` on the capped validation host. Its default build
profile is `validation`; `BURST_NODE_TEST_BINARY` can select an already matching
absolute executable path. The runner's CPU affinity and worker limits do not
replace the host's memory limit.

```bash
bash tools/test-node.sh focused checkpoints::sync::input::tests::slow_polls_and_busy_inbox_both_make_progress
bash tools/test-node.sh fast
bash tools/test-node.sh slow
bash tools/test-node.sh network
```

Focused mode requires an exact test name. `all` runs the three groups in order
and stops on failure. `--list` lists a selected group but can still compile changed
code. Network fixtures run serially by default. CI can divide their sorted names
with `BURST_TEST_NETWORK_SHARDS` and a zero-based `BURST_TEST_NETWORK_SHARD`.
Ignored tests require their stated environment and explicit execution.

| Check | What it establishes |
| --- | --- |
| Domain crate tests | Rules and edge cases covered by those assertions. |
| Node integration tests | Admission, persistence and recovery in the selected fixture. |
| `run-validation.sh` | Signed multi-node journeys using supplied prebuilt artifacts. |
| `run-nat-validation.sh` | Reachability and recovery in an isolated Linux router lab. |
| App `npm run build` | Frontend type checking and bundling. |
| App `cargo check --manifest-path src-tauri/Cargo.toml` | Native backend compilation. |
| Browser regressions | UI behavior with fixture IPC, without real native signing. |
| Native app journeys | Real desktop IPC and signing in an isolated test environment. |

The network runner requires an explicit artifact directory:

```bash
bash tools/limited-run.sh bash run-validation.sh --artifacts-dir "$PWD/target/debug"
```

That directory must contain the daemon and the `validate_network`,
`validate_restart_payment` and `validate_pending_chain` wallet-core examples.
The runner does not build them. It checks ports before launch and retains evidence.
A resumed fixture is recovery evidence; it does not prove fresh onboarding.

The NAT runner requires a dedicated Linux Docker host with at least 6 GiB RAM,
existing daemon/transport/probe executables and a compatible lab image. Set
`BURST_NAT_VALIDATION_HOST`, `BURST_NAT_ARTIFACTS` and, if needed, `BURST_NAT_IMAGE`.
It runs a privileged, resource-capped container without external networking and
never rebuilds artifacts. This lab does not certify physical routers or ISP
behavior. See [reachability limits](../node/NODES.md#reachability-and-services).

App browser tests live in `wallet/tests/dashboard.test.cjs` and
`node-gui/tests/overview.test.cjs`. Native journeys live in each app's `tests/`
directory and require Vite, `tauri-driver`, disposable app data and a test RPC
endpoint. Read the driver's prerequisites before running it; never use a personal
wallet session. A browser preview passing does not qualify native payments.

Daemon smoke checks use `tools/daemon-smoke.mjs` on Unix and
`tools/windows-service-smoke.ps1` on a disposable Windows host. The Windows check
requires elevation and refuses to replace an existing `BurstNode` service.

## Test doubles

The `nullables` crate supplies small, explicitly injected test doubles. It does
not replace every node clock, socket or database automatically.

| Double | What it models | What it does not model |
| --- | --- | --- |
| `NullClock` | An atomic timestamp changed by the test | Tokio timers, wall-clock synchronization or monotonic time |
| `NullNetwork` | Recorded sends and a manually populated FIFO inbox | Socket delivery, bounded queues, peer authentication or failures |
| `NullRandom` | A repeating sequence of configured outputs | Proof verification or context-bound randomness; verification always succeeds |
| `NullStore` | Separate account, block and account-chain maps | Durable commits, cross-map transactions or block-height lookups |

Use a nonempty output list for `NullRandom`; requesting an output from an empty
list panics. Populate `NullStore`'s chain list explicitly when a fixture needs it.
Adding or deleting block bytes does not maintain that list or account metadata.
Tests of authentication, atomic persistence and restart must exercise the real
implementation rather than infer those guarantees from these doubles.

Source: [test doubles](https://github.com/BURST-UBI/protocol/blob/main/nullables/src/lib.rs).

## Documentation changes

Edit public documentation in `docs/`. `.gitbook.yaml` points GitBook at that
folder; `SUMMARY.md` defines its navigation. Keep behavioral claims tied to
current code and label gated functionality. Put test receipts and deployment
history in engineering evidence, rather than the introduction or user guides.

For WASM bindings, browser persistence responsibilities and the compiled-WASM
contract fixture, see [browser wallet integration](BROWSER_WALLET.md).
