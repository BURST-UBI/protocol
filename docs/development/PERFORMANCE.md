# Performance

Complexity describes how work grows with a named input. It does not measure
latency or throughput. A fixed-size signature check can be constant in the number
of accounts while still costing substantially more CPU than a cached lookup.

## Local operations

| Operation | Relevant growth | Scope |
| --- | --- | --- |
| Time passing | No periodic per-account payout | BRN is computed from recorded eligibility and rate history. |
| BRN accrual over one interval | O(log R) for indexed valid history | R is the number of rate segments; malformed unindexed history uses a scan fallback. |
| BRN balance with paused periods | O((I + 1) log R) on the indexed path | I is the number of paused intervals inspected. |
| One loaded holding's expiry | Constant arithmetic | Does not include finding the holding or updating storage. |
| Wallet auto-merge selection | O(T log T) | Sorts T eligible holdings, then groups them. |
| Certified verifier selection | O(P log P) time, O(P) temporary storage | Scores and sorts the P-account pool; excludes authority/replay validation. |
| Verification reward allocation | Linear in panel size | Excludes fraud revocation and persistence. |
| Read/hash snapshot data | At least linear in bytes read | Certificates, state validation and installation add work. |

The indexed BRN path uses cumulative accrual at rate boundaries, rather than
scanning every segment for every balance. A rate change still updates shared
history; it does not issue a payout transaction to every wallet.

## Payments and state staging

A full payment includes authentication, predecessor and dependency checks,
economic calculations, indexes, consensus and persistence. The cost of one
arithmetic check is not the cost of that whole path.

Candidate engines remain borrowed until an operation needs mutable economic
state. BRN wallet storage uses a persistent ordered map of shared wallet records;
cloning the engine is not inherently a deep copy of every account. Mutations,
recording, serialization and persistence still have costs that must be examined
at their actual call sites.

Do not infer that every payment scans all accounts from the presence of a
`clone()`, or that every payment is O(1) because its signed block has bounded size.
The node also coordinates economic state with a database writer; independent
account chains do not imply unlimited parallel commits.

## Pending-payment queries

A page-size limit bounds one query result, not the stored backlog. Pending receipts
and their indexes still consume space even without a fixed per-recipient receipt
quota. Proof of work does not establish a bound on total accumulated storage.

The LMDB amount index stores larger amounts first, with hash order breaking ties.
A keyset page seeks after an `(amount, hash)` boundary and reads only the returned
payloads. An offset page first walks the skipped index rows, so a large offset
still costs work. Both inspect one extra index row to determine `has_more`.
Neither loads and sorts the entire account backlog, unlike the trait defaults.

Each page call opens its own read transaction. The cursor avoids shifting an
integer offset when earlier entries change, but does not freeze the backlog
across requests. Applications must tolerate receipts being added or consumed.

Outgoing totals use a pinned sender-index snapshot and decode one receipt at a
time. They avoid retaining a vector of the entire backlog, but still visit its
indexed rows and allocate each decoded record. Return-credit receipts are skipped
because their holdings have already been returned. Index rebuilds process small
batches inside one write transaction; they still scan the full source table.

See [pending storage](https://github.com/BURST-UBI/protocol/blob/main/store_lmdb/src/pending.rs)
and the [amount index](https://github.com/BURST-UBI/protocol/blob/main/store_lmdb/src/pending/amount_index.rs).

## Provenance and global changes

A merge records its immediate inputs instead of copying every ancestral burn
into each subsequent holding. That reduces repeated ancestry expansion, but does
not make all provenance operations constant-time.

Revocation and restoration traverse affected provenance and update affected
holdings and pending value. Graph edges, composition revisions, origin lookups,
allocation and durable writes all contribute. A widely circulated origin can
make this a large operation. Counting only the final affected holdings can miss
intermediate work.

Holder indexes identify affected wallets, but a vector-backed portfolio may still
scan every holding in each affected wallet. Indexed portfolios can locate the
relevant holdings directly. Neither representation eliminates graph work.

Changing the expiry period visits portfolios across the node. For a portfolio with
T holdings, the vector path scans O(T) records; the indexed path rebuilds its
expiry-dependent indexes in O(T log T). Unchanged portfolios can retain shared
storage. This global transition has a different cost from an ordinary payment.

## Governance counting

A direct walk follows a delegation chain. During electorate counting, the current
resolver memoizes results within the immutable counting context, reusing shared
suffixes and preserving each account's depth limit. It does not independently
walk the entire chain again for every voter.

For Q resolution queries and V reached nodes, the traversal performs O(Q + V)
work under expected constant-time map access. Eligibility, scoped lookups, key
ballot projection, proposal processing and checkpoint work are additional costs.
Results cannot be reused across changed counting contexts without checking their
dependencies.

## Storage, consensus and proof of work

Database cost depends on the index actually used, records returned, page layout
and cache state. A cursor API alone does not establish efficient random access or
pagination. Serialization and copying also grow with the bytes processed.

Vote verification and certificate work grow with the evidence examined. Network
traffic depends on peer topology, retransmission and synchronization. None of
these costs disappears because accounts have separate chains.

Proof-of-work generation searches for an acceptable nonce; validation checks the
submitted work. Generation time depends on threshold, hardware and implementation.
It must not be advertised as universally taking a fraction of a second.
For a uniform 64-bit work result and acceptance threshold θ, the probability of
success per attempt is `(2^64 − θ) / 2^64`. At the Live default threshold
`0xffff_fff8_0000_0000`, the expectation is 536,870,912 attempts. This is an
expectation, not a timing guarantee.

## Measurements and limits

Attach performance results to the exact source, artifact, workload, hardware,
resource caps and assertion measured. Development-profile results do not qualify
release binaries. A small successful fixture does not establish indefinite
history growth, a maximum safe population or sustained public-load capacity.

Streaming reduces temporary memory; it does not eliminate processing or retained
state. Pruning has recovery constraints and is not a proof of constant storage.
See [node recovery](../node/NODES.md) and [build qualification](../release/DOWNLOADS.md).

## Implementation

- [Indexed accrual and paused intervals](https://github.com/BURST-UBI/protocol/blob/main/brn/src/state.rs)
- [Persistent BRN wallet storage](https://github.com/BURST-UBI/protocol/blob/main/brn/src/wallet_states.rs)
- [Borrowed candidate staging](https://github.com/BURST-UBI/protocol/blob/main/node/src/economic_candidate/staging.rs)
- [TRST portfolio and expiry processing](https://github.com/BURST-UBI/protocol/blob/main/trst/src/engine.rs)
- [Provenance graph](https://github.com/BURST-UBI/protocol/blob/main/trst/src/merger_graph.rs)
- [Memoized delegation resolver](https://github.com/BURST-UBI/protocol/blob/main/governance/src/vote_delegation/ballot_resolver.rs)
- [Certified pool selection](https://github.com/BURST-UBI/protocol/blob/main/verification/src/selection_request.rs)

## Work generation and measurement

Work validation computes Blake2b with an eight-byte output over the little-endian
nonce followed by the 32-byte work root. The result is read as a little-endian u64
and must be greater than or equal to the threshold. The root is the previous
block hash, or an account-bound hash for an opening block, so work can be prepared
before choosing the next transaction's contents.

Native generation uses the current Rayon pool. The bounded entry point checks
cancellation and its deadline between batches of 4096 attempts per worker;
it does not interrupt each hash. The unbounded entry point has no deadline.
WebAssembly generation is synchronous; a browser caller must place it in a
worker to avoid blocking the UI.

The generation benchmark in `work/benches/work_bench.rs` uses thresholds from
zero to 100,000, for which almost every hash qualifies. Those cases measure
call and worker overhead. They cannot establish generation time at the default
protocol threshold. No fixed latency or flood-resistance claim follows from
passing these tests or benchmarks.

Sources: [work validation](https://github.com/BURST-UBI/protocol/blob/main/work/src/validator.rs),
[generation](https://github.com/BURST-UBI/protocol/blob/main/work/src/generator.rs),
[work roots](https://github.com/BURST-UBI/protocol/blob/main/ledger/src/state_block.rs).
