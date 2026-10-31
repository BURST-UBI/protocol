# BURST protocol

BURST connects a non-transferable allowance, BRN, with transferable TRST.
Verified people accrue BRN over eligible time. Burning it for a payment creates
TRST with tracked provenance and a governed expiry period.

This repository contains the Rust protocol implementation, node daemon and shared
wallet libraries. The [wallet](https://github.com/BURST-UBI/wallet) and
[node dashboard](https://github.com/BURST-UBI/node-gui) are separate applications.

## Read the documentation

The website's Markdown source lives in [`docs/`](docs/README.md).

- [Start with the two-minute introduction](docs/introduction/QUICK_TOUR.md).
- [Read the whitepaper](docs/introduction/WHITEPAPER.md) for the design and its assumptions.
- [Find a crate or subsystem](docs/development/ARCHITECTURE.md).
- [Build, run and test the code](docs/development/DEVELOPING.md).
- [Operate a node and preserve recovery state](docs/node/NODES.md).
- [Review RPC and peer service limits](docs/node/SERVICES.md).

## Downloads and policies

See [Downloads](docs/release/DOWNLOADS.md), [Code signing policy](docs/release/CODE_SIGNING.md),
and [Privacy and network behavior](docs/release/PRIVACY.md). SignPath Foundation is the
proposed Windows signing provider; approval and integration are not active.

## Development and release boundaries

Use a disposable Dev ledger for experiments. Test-network keys are public;
test funding does not establish a person's identity. Keep RPC on localhost and
sign wallet operations locally.

Some certified verification and governance paths are gated on Live. Read
[implementation limits](docs/introduction/LIMITATIONS.md) and
[build qualification](docs/release/DOWNLOADS.md) before choosing an artifact.
A passing unit test or a connected node does not establish release readiness.

Before expensive validation, follow the repository's
[validation workflow](spec/VALIDATION_WORKFLOW.md). Current engineering evidence
is indexed in [spec/README.md](spec/README.md).

## Acknowledgments

Thanks to [rsnano](https://github.com/rsnano-node/rsnano-node) and its contributors
for their open-source work, which has been a valuable reference for BURST’s
architecture and implementation.

## License

[MIT](LICENSE). Copyright 2024–2026 Nitesh Gautam.
