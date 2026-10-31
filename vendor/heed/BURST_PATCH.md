# Transaction identity backport

Source: crates.io heed 0.20.5, archive SHA256
`7d4f449bab7320c56003d37732a917e18798e2f1709d80263face2b4f9436ddb`.
License: MIT, retained in LICENSE from upstream tag v0.20.5.

Original source change: expose `RoTxn::id()` through LMDB's existing `mdb_txn_id`.
The same public API is provided by upstream heed 0.22.0. This backport avoids
changing the transaction TLS types, dependency versions or storage engine while
correcting snapshot cache identity. No Rust layout casts or private-pointer access
outside heed are used. Cargo.lock and examples were not modified upstream; the
packaged lockfile is omitted because the workspace lockfile controls dependencies.

Environment info reads the newest metadata page. LMDB publishes that page before
updating the reader transaction counter, so two equal environment observations do
not prove the identity of a newly opened read transaction. Cache tags must use the
transaction's own ID. The pinned-snapshot regression lives in burst-store-lmdb.


File-growth ceiling extension (2026-10-25): pairs with the workspace-patched
lmdb-master-sys0.2.5. `RwTxn::limit_file_extent` acquires one process-local RAII
lease under the existing exclusive native writer. Each later top-level writer
loads its ceiling once after acquiring that writer; nested transactions inherit
it natively. Dropping the lease does not relax an already active writer. The
lease retains its Env so the atomic setting cannot outlive its environment.
No layout casts or pointers outside Heed. No waits or callbacks inside allocation.
This bounds newly allocated file pages only, not resident memory or writes from
other processes. Application cancellation and retry integration is required before
production activation. Tests live in burst-store-lmdb read_view and error modules.


Pressure ownership extension: each guarded writer holds a transaction-specific
GrowthOutcome and shares the owning lease state. Native rejection is recorded
before abort/drop, or from native commit's GrowthLimit result after destruction.
A clean transaction and a nested transaction have distinct outcomes. GrowthWatch
polls only its lease's rejection flag and can wait for its owner to release it;
waiters never access an MDB_txn pointer. No waiting is performed by native allocation,
transaction teardown or commit notification. Unprotected writers retain the atomic
fast path without a state allocation or mutex acquisition. The lease registry uses
a mutex only during guarded writer setup and lease acquisition/release.


Retry priority: a guarded writer can reserve GrowthRetry before mutation. Clones
share one counted reservation. New capture registration returns None while a retry
reservation remains; dropping the last clone releases priority. Ordinary outcome
and watch handles do not reserve priority. Admission retains it through serial
retry and publication, preventing capture churn without an unbounded retry loop.

Advisory preflight: `Env::may_limit_file_extent` reads the existing owner/retry
atomics without allocating or opening a transaction. It lets optional capture
skip expensive authentication when priority is already held. A true result grants
no reservation; the writer-side acquisition remains authoritative and race-safe.
