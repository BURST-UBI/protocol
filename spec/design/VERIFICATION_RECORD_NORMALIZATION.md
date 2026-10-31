# Verification record storage

Verification state uses bounded physical rows instead of one growing serialized
record per target. Startup normalizes supported local storage after authenticated
recovery and before admission. This changes local representation, not signed
blocks, certified selection inputs or economic outcomes.

## Representation

A scalar header records counts and optional-domain presence. Separate domains hold
endorsements, endorser membership, selected members, votes, exclusions, prior votes,
selection-pool entries and attempt contexts. Identity keys locate members; ordinal
keys preserve the authenticated selection and committee order.

Readers check versions, identities, ordering, exact counts and snapshot budgets.
Missing, duplicate, extra or mixed-format rows fail. Scoped writes compare affected
domains and leave an unchanged selection pool alone. Changed panel/history domains
can still require member comparisons; normalized rows do not make an entire vote
or deadline transition constant time.

## Transactions and recovery

The storage adapter retains compatible historical undo through its existing
aggregate and target overlays. Normalization validates the conversion and publishes
data and the format marker transactionally. A rejected conversion must not expose
a partially normalized ledger or discard rollback rights.

Startup commits the validated cache and, when conversion occurred, forces storage
synchronization. A synchronization error after commit is an error, but it cannot
undo the already committed conversion. Recovery must preserve that distinction.

## Implementation

- [Normalized row codec](../../verification/src/orchestrator/records/normalized.rs)
- [Scoped deltas](../../verification/src/orchestrator/records/normalized/delta.rs)
- [Transactional storage and normalization](../../store_lmdb/src/write_batch/verification_records.rs)
- [Startup adapter and historical overlays](../../node/src/verification_storage.rs)

The [retained contract and validation record](../evidence/verification-record-normalization-20261031.txt)
preserves row/key/undo limits, migration requirements and large-panel regression
results. Those results address their recorded fixtures and inputs. They do not
qualify all cold-state, contention or production-resource behavior. Consult
[the handoff](../COMPLETION_LEDGER.md) before treating an earlier incomplete stage as
an outstanding implementation task.
