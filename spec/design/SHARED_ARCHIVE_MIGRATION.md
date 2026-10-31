# Shared checkpoint-record migration

Production maintenance reclaims unowned shared-head nodes. It does not enable
legacy archive migration. The migration worker exists for controlled validation;
its functional results do not authorize production scheduling.

## Ownership and publication

The worker selects records through an indexed cursor, builds shared nodes and
stages envelope pages in bounded batches. Persisted continuation owns unfinished
roots so cleanup cannot reclaim them. The original record stays authoritative
until the complete staged reconstruction passes verification.

Publication must verify the source identity and atomically transfer the descriptor
and ownership. Changed source data cannot be overwritten by a stale worker.
Cancellation, restart and aborted publication must retain enough state to reclaim
staging safely. Cleanup must respect pinned readers and newer replacements.

Reuse the existing ordered tree builder, paged metadata, source observation and
shared-node ownership mechanisms. A second tree or migration scheduler would
split the same invariants across implementations.

## Implementation

- [Maintenance and production gate](../../node/src/checkpoints/store/record_storage/maintenance.rs)
- [Migration worker](../../node/src/checkpoints/store/record_storage/shared/migration.rs)
- [Ordered tree construction](../../node/src/ordered_radix.rs)
- [Paged metadata](../../node/src/paged_metadata.rs)
- [Transactional head ownership](../../store_lmdb/src/write_batch/checkpoint_heads.rs)

## Evidence and limits

The [retained migration contract and run record](../evidence/shared-archive-migration-20261031.txt)
preserves phase requirements, failed storage diagnostics and scoped restart,
publication and contention results. Metadata contention is not signed mixed traffic;
exact byte preservation is not a sustained latency or disk-capacity guarantee.
Production qualification remains separate. Follow [validation reuse](../VALIDATION_WORKFLOW.md)
before scheduling any further measurement.
