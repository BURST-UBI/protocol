# Browser wallet core

`burst-wallet-core` and `burst-crypto` build for `wasm32-unknown-unknown`.
The browser client has the native client's RPC, signed payments, governance,
appeal, scoped-delegation and provenance APIs. This is library support, not a
published web wallet or proof that every workflow has run in a browser.

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

## Reproduce

From `protocol/`, install the target once and check the libraries:

```sh
rustup target add wasm32-unknown-unknown
CARGO_BUILD_JOBS=1 cargo check -p burst-wallet-core -p burst-crypto --target wasm32-unknown-unknown
CARGO_BUILD_JOBS=1 cargo clippy -p burst-wallet-core -p burst-groups -p burst-browser --target wasm32-unknown-unknown -- -D warnings
```

The real-browser contract test needs `wasm-bindgen-cli` 0.2.126, Playwright and
Chromium. The CLI version must match the `wasm-bindgen` crate in `Cargo.toml`.

```sh
CARGO_BUILD_JOBS=1 cargo build -p burst-wallet-core --example browser_check --features browser-test --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir target/browser-check target/wasm32-unknown-unknown/debug/examples/browser_check.wasm
node --test wallet_core/tests/browser.cjs
```

If Playwright or Chromium is installed elsewhere, set `PLAYWRIGHT_MODULE` and
`CHROMIUM_PATH`. The test starts its own loopback server and headless browser and
closes both. Its fixed signing key exists only in the gated test adapter.

The test runs compiled WASM, uses browser entropy, checks clock and work generation,
verifies the payment signature independently in Node.js, exercises RPC submission
and confirmation, and checks group queries, size limits, redirects, cookies,
malformed responses and cancellation. It also tests opt-in group trust graphs,
live endorsement removal, cycles, unregistered groups and redirect rejection.
Crypto, RPC and transitive group queries run in a Web Worker as well as the page.
Its RPC server is a contract fixture, not a BURST ledger. Live-network browser
journeys, Safari/mobile behavior, encrypted-storage UI and production deployment
remain separate checks. CI currently runs compilation and Clippy, not Chromium.
