# Simplicity audit fixes — current handoff

This record covers its dated source batch. Earlier stages below are historical;
consult the final disposition before reopening a finding. Later source edits require
input-scope review before reusing its receipts.

Scope: all S01–S20 and D01 from the October 30 audits. Existing unrelated working-tree changes are preserved. No release gate is closed by this tracker.

| Item | Requirement | Current status |
|---|---|---|
| S01 | Obsolete verification processor | Runtime wiring removed; node compile passed; real orchestrator regression passed |
| S02 | Verification status storage errors | Implemented; RPC compile passed; direct propagation reviewed |
| S03 | Signing predecessor reads | Implemented; RPC compile passed; retained hash checks reviewed |
| S04 | Unused parameter trait | Removed; governance compile and parameter tests passed |
| S05 | Parameter registry duplication | Implemented; catalog/serialized discriminants, width and activation tests passed |
| S06 | Typed signing RPC contracts | Implemented for shared proposal review; signing review and RPC response tests passed |
| S07 | Unused adaptive difficulty | Removed; node compile and work dependency compile passed |
| S08 | Random gossip fanout | Implemented; 7 broadcast tests passed |
| S09 | Unused network/work/stats prototypes | Removed; dependent library/node compile passed; active bandwidth constant preserved |
| S10 | Simulation rollback API | Misleading wrapper removed; node compile passed; equivalent simulation test calls retained |
| S11 | Unused UI accrual | Removed; 4 amount-display tests passed on actual accrual path |
| S12 | Alternate verifier selectors | Removed alternate APIs; node compile passed; real orchestrator regression passed |
| S13 | Genesis construction | Node/key generator share configured constructor; both compile; legacy fixture bytes retained |
| S14 | Unused adaptive quorum | Removed; governed quorum regression passed |
| S15 | Auto-merge aggregation | Unused sum removed; 10 auto-merge tests passed including u128 overflow inputs |
| S16 | Typed WebSocket events | Typed event channel implemented; 16 WebSocket tests passed |
| S17 | Merge review lookup | Indexed lookup implemented; duplicate/change/overflow tests passed |
| S18 | Shared hash parsing | Shared parser implemented; grammar regression and consuming library/node checks passed |
| S19 | Persistent desktop HTTP clients | Both clients pooled; wallet native compile passed; dashboard native compile passed |
| S20 | Actual wire-codec coverage | Legacy codec scope corrected; misleading agreement roundtrip removed; actual Protobuf coverage retained, not rerun |
| D01 | Preserve recovery and name custom derivation accurately | Legacy-v1 documented; 8 mnemonic tests passed, including independent key vector |

## Current handoff and reuse table

Implementation and focused validation complete for this audit batch: **57 tests
passed** (50 library, 1 hash grammar, 4 UI, 1 real orchestrator, 1 threshold-provider),
plus production node/key-generator and both native desktop compilation checks.
No job remains active. No VMs started. Full node-library, multi-node and packaged
release qualification were not run; existing release gates remain open.
Evidence root: `/tmp/burst-simplicity-20261030`.

| Changed dependency | Proven scope / latest receipt or log | Invalidation / next action |
|---|---|---|
| Shared hash types | `hash-receipt.json`: exact hex grammar, 1 test | Parser/types inputs; no rerun needed |
| RPC/wallet/governance/network/crypto/WebSocket libraries | `test-build-receipt.json`, `runtime-receipt.json`: six artifacts, 50 focused tests | Subsequent verifier edit is test-only; do not rebuild all six merely to test it |
| Node wiring and configured genesis | `node-receipt.json`: node library + genesis_keygen compile | Production dependencies; new verification test does not alter production behavior. Full node test qualification remains separate |
| Wallet native HTTP reuse | `wallet-native-receipt.json`: `cargo check --lib --locked` | Native transport/shared dependency changes; compilation is not a pool performance benchmark |
| Dashboard native HTTP reuse | `dashboard-native-receipt.json`: native compile passed | Native transport or app dependency changes; no rerun needed |
| Wallet removed accrual helper | `amount-format.log`: 4 tests | Formatter/BRN behavior. Subsequent EOF whitespace correction needs no rerun |
| Replacement verifier regression | `verifier-receipt.json`: real threshold/panel/endorsement assertions pass; `threshold-receipt.json`: unavailable provider fails closed | Verification/VRF behavior or test changes; no full node test build needed for this component scope |

S06 shares typed required proposal review fields; unrelated RPC families remain
outside this incremental migration. S13 retains explicit historical fixture bytes
while sharing the configured launch constructor. S20 scopes the legacy codec and
removes its misleading agreement-state roundtrip; existing actual Protobuf tests
were retained, not rerun or counted as new evidence. S01's replacement regression
is a component round, not certified enrollment evidence.

Removed prototype-local tests along with the unused contracts they tested. Active
bandwidth, queues, work thresholds, node bootstrap and metrics remain. The old
verification toy test is replaced with assertions against the real orchestrator.
These removals do not erase or replace production acceptance requirements.

## Preserved failed attempts / resolved diagnostics

* `wallet-review.log`: initial unreceipted compile failed (exit 101) because
  deleting the parameter trait also removed GovernanceError, still needed by the
  name parser. Import restored; later integrated compilation and tests passed.
* `library-receipt.json`: compile attempt 1 failed (exit 101), because prototype
  throttle removal also removed the default constant used by active egress.
  Preserved the same value in bandwidth.rs. Attempt 2 used `prepare` with the
  failed receipt and changed inputs, then passed (`library-receipt-2.json`).
  The later six-artifact test build supersedes that compile for its tested scope.
* Hash receipt finalization initially ran from the parent directory and could
  not find the tool. Only finalization was repeated from the correct directory;
  the passing test was not rerun.

Receipt inventories cover their declared inputs, not all possible build-host
configuration. No missing/changed evidence is silently promoted to a release pass.

## Final evidence review

The hash receipt checker verified unchanged inputs and evidence. Checking the
50-test runtime receipt after adding the isolated verifier regression correctly
reported changed input inventory. The only intervening relevant edit was the
new cfg(test) verification module (and removal of its duplicate node-integration
placement); it did not change production behavior or the six executed test bodies.
The original 50-test result is retained with that explicit impact limitation,
not presented as a newly hash-matching run. The new regression has its own passing
receipt. No unchanged suite was rerun to manufacture matching status.

Evidence is also archived outside git at
`/home/nitesh/.local/state/burst/audits/simplicity-20261030`.
Receipts retain their original absolute /tmp input/log paths; the archive preserves
the original bytes, not a rewritten or newly qualified receipt.

All changes remain uncommitted. Earlier audit documents are historical findings;
this table records the implemented disposition and the precise validation scope.
