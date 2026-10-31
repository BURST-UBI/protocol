# Reference implementations

BURST uses rsnano, Nano and Tezos as engineering references. Their code helps
compare module boundaries, storage transactions, network services and consensus
recovery. BURST's accounting, identity rules and voting rights remain separate
protocol requirements.

## Read BURST first

- [Architecture](../../docs/development/ARCHITECTURE.md) maps crates and runtime responsibilities.
- [Performance](../../docs/development/PERFORMANCE.md) describes operation costs and their limits.
- [Cryptographic formats](../../docs/development/CRYPTOGRAPHY.md) defines signed data and encoding.
- [Consensus](../../docs/protocol/CONSENSUS.md) explains agreement and representative authority.
- [Node operation](../../docs/node/NODES.md) covers retained history and recovery.

A reference comparison should name the operation, identify the equivalent
implementation and explain any different requirement. Similar names or folder
layouts alone do not establish equivalent behavior.

## Reference revisions

These revisions identify the source used by the retained review. They are not
claims about the latest upstream release or independently audited selections.

| Project | Revision |
| --- | --- |
| [rsnano](https://github.com/rsnano-node/rsnano-node) | `414dd884c162f78f151b9d912a029bbc9a340866` |
| [Nano](https://github.com/nanocurrency/nano-node) | `f32f120a326ac6eab41c442f81bd7b9b18c2b0da` |
| [Tezos](https://gitlab.com/tezos/tezos) | `6ef1d49dee6a29b103fca2e8e507231308b01109` |

## Historical evidence

The [archived review](../evidence/reference-implementation-review-20261031.txt)
contains the original iteration notes and validation results. Requirement records
cite that artifact directly. Its old open items and passing results describe
those worktrees, not current implementation status or release approval.
Use the [qualification index](../README.md) for current evidence owners.
