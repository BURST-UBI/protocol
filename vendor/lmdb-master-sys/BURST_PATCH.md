# Native file-growth ceiling

Source: crates.io lmdb-master-sys0.2.5, archive SHA256
`864808e0b19fb6dd3b70ba94ee671b82fce17554cf80aeb0a155c65bb08027df`.
Upstream LMDB license and copyright remain in lmdb/libraries/liblmdb/LICENSE
and COPYRIGHT. The workspace lockfile controls dependencies; packaged Cargo.lock
and registry metadata are omitted.

Changes: transaction-local exclusive page ceiling, zero when disabled; reset on
renewal, inherited by nested writers. mdb_page_alloc checks new-page extension
before incrementing mt_next_pgno or allocating dirty memory. Freelist reuse is
unchanged. Commit-time freelist/database allocations use the same boundary.
MDB_GROWTH_LIMIT is separate from native map exhaustion and follows the existing
transaction-error/abort path. The setter is restricted to active root writers,
rejects ceilings below already allocated pages and cannot relax a ceiling.
No persistent database/page layout or map size changes. No callback, wait, new
lock or page scan. C API declarations, shipped Rust bindings and bindgen signed
constant handling are kept consistent.

This is a file-page high-water ceiling, not a bound on all dirty-page allocations,
resident memory, physical disk blocks, durability or other processes. Heed supplies
the process-local lease and installs it on every new root writer. Application
capture/retry integration and platform/release qualification remain required.


Pressure identity extension: a transaction-local rejection bit is set only when
the new-page ceiling rejects allocation. Renewal and nested creation reset it.
A read-only getter lets Heed preserve the outcome before native abort/commit frees
or reuses storage. This adds no callback, atomic interoperability or persistent
format change. Native commit-time GrowthLimit is also preserved from its return.
