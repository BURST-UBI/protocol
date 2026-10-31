# Browser wallet integration

`burst-wallet-core` and `burst-crypto` target `wasm32-unknown-unknown` as well as
native platforms. Shared APIs cover signing, node RPC calls, governance votes, appeals,
delegated permissions and tracking where currency came from (provenance). This library is not a published browser wallet or a ready-made
JavaScript SDK; an application supplies its bindings and interface.

Native file-backed payment journals, offline-action persistence and signing-device
bindings are excluded from WASM. A browser application must provide durable action
storage and recovery behavior before offering equivalent workflows. Compiling the
shared signing code does not supply those guarantees.

Browser entropy comes from Web Crypto through `getrandom`. Timestamps use the
browser clock; the node still validates transaction timing. Keystore encryption
and decryption work on in-memory JSON values. File-path load/save functions are
native-only; a browser application must supply its own storage and backup UI.

Fetch requests omit cookies, reject redirects, keep CORS enabled and bypass caches.
Bodies are size-bounded while streaming. Timeouts and dropped requests abort the
network operation. A browser cannot distinguish some CORS, redirect and network
errors, so ambiguous group failures require review rather than automatic fallback.
An explicit timeout or server error retains the existing configurable fallback.

Local proof-of-work generation is single-threaded on WASM. Run it in a Web Worker,
or let `NodeClient` request work from the node. Never run difficult local work on
the UI thread. The library works in both page and worker contexts.

## Build and check

From `protocol/`, install the target if missing. Before compiling, follow the
[validation workflow](https://github.com/BURST-UBI/protocol/blob/main/spec/VALIDATION_WORKFLOW.md)
and reuse matching artifacts. Run one focused check at a time on the constrained
desktop; the commands below show alternatives and separate stages, not a required
full rerun after every edit.

```sh
rustup target add wasm32-unknown-unknown
bash tools/limited-run.sh cargo check --locked -p burst-wallet-core -p burst-crypto --target wasm32-unknown-unknown
bash tools/limited-run.sh cargo clippy --locked -p burst-wallet-core -p burst-groups -p burst-web-runtime --target wasm32-unknown-unknown -- -D warnings
```

The real-browser contract test needs `wasm-bindgen-cli` 0.2.126, Playwright and
Chromium. The CLI version must match the workspace `wasm-bindgen` pin in `Cargo.toml`.

```sh
bash tools/limited-run.sh cargo build --locked -p burst-wallet-core --example browser_check --features browser-test --target wasm32-unknown-unknown
bash tools/limited-run.sh wasm-bindgen --target web --out-dir target/browser-check target/wasm32-unknown-unknown/debug/examples/browser_check.wasm
bash tools/limited-run.sh node --test wallet_core/tests/browser.cjs
```

If Playwright or Chromium is installed elsewhere, set `PLAYWRIGHT_MODULE` and
`CHROMIUM_PATH`. The test starts a loopback server and headless browser. Its fixed signing key
exists only in the `browser-test` adapter and must never hold real value. Check
the browser installation before running: a launch failure occurs before this
driver enters its cleanup block and may leave its server running.

## What the browser fixture covers

The test runs compiled WASM, uses browser entropy, checks clock and easy-threshold work generation,
verifies the payment signature independently in Node.js, exercises RPC submission
and a fixture-reported confirmation, and checks group queries, size limits, redirects, cookies,
malformed responses and cancellation. It also tests opt-in group trust graphs,
live endorsement removal, cycles, unregistered groups and redirect rejection.
Crypto, RPC and transitive group queries run in a Web Worker as well as the page.
Its RPC server is a contract fixture, not a BURST ledger. Live-network browser
journeys, Safari/mobile behavior, encrypted-storage UI and production deployment
remain separate checks. CI currently runs compilation and Clippy, not Chromium.

## Source

- [WASM test bindings](https://github.com/BURST-UBI/protocol/blob/main/wallet_core/examples/browser_check.rs)
- [Browser contract fixture](https://github.com/BURST-UBI/protocol/blob/main/wallet_core/tests/browser.cjs)
- [Fetch and event-loop timers](https://github.com/BURST-UBI/protocol/blob/main/web_runtime/src/lib.rs)
- [Wallet keys and recovery](../wallet/WALLETS.md)
- [Network disclosure](../release/PRIVACY.md#group-services-and-browser-requests)
