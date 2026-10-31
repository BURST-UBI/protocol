# Authenticated snapshot archives

An archive contains certified authority history, a snapshot certificate and an
economic state body. It carries no private keys or local signing journals. A
receiver authenticates authority from its configured trust base, then validates
the body and reconciles local history before installation.

Read [node recovery](../../docs/node/NODES.md#authenticated-snapshots) for the operator model
and [checkpoints](../../docs/protocol/CHECKPOINTS.md) for authority, signing and installation.
The [retained format and validation record](../evidence/snapshot-archive-20261031.txt)
preserves the detailed encodings, compatibility requirements and scoped run results.

## Create and verify an archive

Each representative independently reconstructs a named certified epoch and records
its attestation durably before returning a signature. Never erase a signing journal
to sign different state for an already-attested epoch.

```sh
burst-daemon --network live --data-dir ./burst_data \
  --representative-key-file ./representative.key snapshot attest \
  --epoch CHECKPOINT_HASH --output representative.attestation

burst-daemon --network live --data-dir ./burst_data snapshot export \
  --epoch CHECKPOINT_HASH \
  --attestation representative-a.attestation \
  --attestation representative-b.attestation \
  --genesis-params-output original-genesis.json --output economy.snapshot

burst-daemon --network live snapshot verify --input economy.snapshot \
  --genesis-params original-genesis.json \
  --minimum-height 100 --expected-epoch CHECKPOINT_HASH
```

Use the intended network and independently obtained epoch/height. The example
height is not a network recommendation. Attestation count alone does not establish
quorum: signatures, human weights and the exact state commitment must match.
These commands remain subject to the build's certified-mode gates.

Obtain original genesis parameters through a trusted channel. Current governed
parameters cannot replace the original trust anchor. Verification writes no ledger
state and does not establish that an otherwise valid archive is the newest one.

## Transfer and installation

Snapshot transfer uses existing authenticated peer connections. Serving and fetching
are opt-in configuration. Downloaded data is staged and authenticated before the
output file is published without replacement. A matching explicit bootstrap setting
is required to install it; successful download alone does not change authority.

The local `snapshot_transfer.max_archive_bytes` budget applies to serving, fetching
and installation. It is not a consensus parameter or a limit on total disk/RAM use.
Staging, authority proofs and database projection need additional resources.

Installation preserves required confirmed history and durable signer commitments,
commits the projection, synchronizes storage and publishes runtime state. Neither
snapshot possession nor token expiry authorizes arbitrary historical-body deletion.
Production body retirement retains its separate gate.

## Implementation and format ownership

| Boundary | Source |
| --- | --- |
| Framing, certificate authentication and archive reader | [archive.rs](../../node/src/confirmed_replay/snapshot/archive.rs) |
| Canonical economic body | [body.rs](../../node/src/confirmed_replay/snapshot/body.rs) |
| Snapshot reconstruction and export | [export.rs](../../node/src/confirmed_replay/snapshot/export.rs) |
| Local commitment reconciliation | [reconciliation.rs](../../node/src/confirmed_replay/snapshot/reconciliation.rs) |
| Installed-state recovery | [installed.rs](../../node/src/confirmed_replay/snapshot/installed.rs) |
| Startup integration | [snapshot_bootstrap.rs](../../node/src/node/snapshot_bootstrap.rs) |

Archive framing, signed body versions and local database page formats are separate
compatibility boundaries. Loading supported old bytes does not authorize rewriting
their signed commitment. Use the retained format record together with these codecs
when changing a boundary, and follow [validation reuse](../VALIDATION_WORKFLOW.md).
