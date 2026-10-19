# Public release status

Current requirements, priorities and acceptance boundaries are in
[COMPLETION_PLAN.md](COMPLETION_PLAN.md); current status is in
[COMPLETION_LEDGER.md](COMPLETION_LEDGER.md). The iteration notes below are
historical evidence, not the current execution queue or capability sign-off.

Updated 2026-10-19. Public distribution remains closed. The selected Linux amd64
v7 archive is `burst-testnet-linux-amd64-v7-retention-candidate.tar.gz`, SHA-256
`5cfcd1ccdd3cbad77051b95fa8f5366fa06f09b0734665fadff2aaf4fb3abc1b`.
It contains the epoch-recovery and bounded-retention corrections and reuses the
qualified wallet/dashboard packages. Exact daemon identity/admission/restart
checks pass. The installed wallet also passes read-only governance outcomes,
constitution content and restored verification against retained certified state.
This does not turn earlier development signing journeys into fresh release runs.
The ledger records component hashes, failures, source comparisons and evidence
limits. The arithmetic bundle mentioned in older notes is no longer selected.

Remaining work includes offline-action scope, internal fragment-ID interpretation
and requirement/evidence reconciliation. Creator private-backup confirmation and
the public bootstrap/contact handoff remain. The coverage inventory
still contains unsigned-off requirements; its latest structural check passes.
Use `node tools/check-spec-coverage.mjs` for current counts. Those counts are not
a missing-feature count or a security assessment.
The new installed readback used the existing packages and a copied ledger. No
product rebuild, panel, proposal, vote or payment journey was repeated.

## Historical work through 2026-10-13

Iteration 290 replaces numeric checkpoint refresh with a queue of retained
selection heights and authenticated snapshot selection receipts. Each cycle
deduplicates and sorts those heights, then alternates refresh with forward sync.
Pruned non-selection heights no longer delay a late beacon. These heights are
scheduling hints; incoming records still pass the existing authentication and
replay path. No ledger history is deleted.

Validation passes in `/tmp/burst-selection-refresh-290.log`: the high-height
regression, receipt-only refresh, both delayed-beacon TCP/restart variants,
six checkpoint-sync tests and strict node library/test Clippy. The live pair
completes in 20.40 seconds. The capped job finished successfully. History
compaction and incremental authority storage remain open.

Iteration 289 connects snapshot-committed historical heads to witness recovery.
The cursor accepts only exact selection-receipt heads or the pinned genesis,
keeps the current authenticated snapshot context and still checks body hashes,
signatures and heights. Unknown historical hashes retain the ancestry walk.
Startup saves verified replay-dependency bodies in its coordinated transaction.
Fresh bootstrap and live installation use the same bounded witness path.

The exact-head/signature regression passes in 0.87 seconds and the live
sparse-source journey passes in 9.95 seconds in
`/tmp/burst-committed-bootstrap-289.log`. The archival source is stopped before
the sparse relay restarts alone and serves a fresh observer. Committee state,
snapshot bytes, valid payment and stale-epoch rejection are checked. The earlier
live attempt in `/tmp/burst-committed-history-289-fixed.log` timed out before the
fresh-installation path was updated. The broader run passed 103 tests and failed
one mock-service fixture that exited after its first request. The fixture now
serves all bounded requests until recovery releases the channel, preserving its
state-equality and no-write assertions. The failed result is in
`/tmp/burst-committed-bootstrap-289-regression.log`. The rerun passes 104
snapshot-history tests, with one ignored and live-installation tests excluded,
followed by strict node library/test Clippy in
`/tmp/burst-committed-bootstrap-289-regression-fixed.log`. The capped job exited
successfully and the validation service is inactive.

This closes the recorded sparse restart/bootstrap fixture, not general history
compaction or public release. Dependency-aware bounded retention, incremental
authority storage and the remaining release checks below are still open.

Iteration 288 adds a sparse-source journey that shuts down the archival node,
restarts its snapshot-bootstrapped peer and then attempts a fresh observer
download and payment. This journey is not passing. Recovery waits for unavailable
historical witnesses after the archival peer is removed. The diagnostic log is
`/tmp/burst-sparse-source-288-diagnostic.log`; the runs were stopped after observing
the repeated unavailable-history requests, not counted as passes.

One unnecessary traversal is fixed. When all remaining requested hashes are
other authenticated account heads, the witness walker now changes accounts
instead of walking older bodies from the completed account. The new head-only
fixture passes with those older bodies absent. Historical selection-receipt heads
and replay dependencies still require authenticated recovery across pruning;
the head-only change does not remove that requirement. The live regression stays
enabled and release gates stay closed.

All 20 witness tests and strict node library/test Clippy pass in
`/tmp/burst-sparse-source-288-witness.log`. The capped job finished and the
validation service is inactive. This does not close the live sparse-source gap.

Iteration 287 checks the local-witness fallback added in iteration 286. A valid
local batch resolves the request; missing bodies and a changed stored signature
leave it pending for peer recovery. A valid peer response then resolves the same
authenticated request. The fixture compares metadata before and after and
revalidates the returned block against the original snapshot cursor. The focused
test, all 19 witness tests and strict node library/test Clippy pass in
`/tmp/burst-local-witness-validation-287.log`. No release gates changed.

The first broader run passed 102 snapshot tests but failed the queue-cancellation
fixture, which expected a locally available request to remain pending. That
fixture now uses an empty service store while keeping its authenticated request,
so it tests peer-waiting and cancellation rather than relying on missing local
recovery. The original result is retained in
`/tmp/burst-local-witness-validation-287-regression.log`.

The corrected cancellation fixture, 103 snapshot-history tests and strict node
library/test Clippy pass in
`/tmp/burst-local-witness-validation-287-regression-fixed.log`. One snapshot test
is ignored and live-installation fixtures are excluded from that broader run.
The capped job finished successfully; the validation service is inactive.

Iteration 286 extends delayed-beacon refresh to a source whose original selection
record has been removed from the temporary fixture database. Both nodes restart
after committee publication. The receiver retains the complete derived committee
without recreating the selection record; the source's record stays absent too.

Both-node restart exposed an unnecessary peer dependency. The witness service
checked its memory cache and then requested peer bodies even when the requested
history was retained locally. It now tries a bounded local batch through the same
authenticated cursor, signature, byte and block checks used for peer input.
Unavailable local history falls back to peers. It does not install blocks.
Both live variants pass in 92.50 seconds, followed by 18 witness tests and strict
node library/test Clippy in `/tmp/burst-pruned-record-refresh-286-local.log`.
The earlier readiness failures remain in the iteration-286 logs. The source
still retains historical block bodies; fully pruned-source catch-up remains open.

Iteration 285 adds a TCP delayed-beacon journey with two running nodes. The
receiver starts from a height-3 snapshot containing the pending height-2
selection, without its physical record. Only after receiver readiness does the
source learn the beacon. Automatic refresh imports it and publishes the exact
derived committee without changing the payment epoch or recreating the record.
The receiver then restarts with the same complete verification state.

The first storage-only test passed, but the stronger publication assertion exposed
a startup bug. Economic recovery derived the published committee correctly;
cache staging then wrote the frozen pending state back over it. Cache derivation
now reconstructs the same authenticated publication before staging final rows.
Strict state comparisons remain. The stronger journey passes in 30.10 seconds
in `/tmp/burst-live-refresh-285-fixed.log`; its earlier failure is preserved in
`/tmp/burst-live-refresh-285-publication.log`. The source retains proof bodies and
the original selection record. This is not fully pruned-source catch-up evidence.

The same capped run passes 102 snapshot-history tests in 57.57 seconds, with one
ignored and live-installation fixtures excluded, then strict node library/test
Clippy. The job exited successfully. Release gates remain closed.

Iteration 284 fixes export of a delayed beacon when the original selection
record is still retained without it. Export verifies the snapshot receipt,
retained certificate, heads, target and saved beacon before combining them.
It preserves the original certificate and does not rewrite the stored record.
The regression covers record-only transfer output, invalid saved randomness and
database reopen. Five compact-persistence tests, six checkpoint-sync tests and
strict node library/test Clippy pass in `/tmp/burst-retained-beacon-284.log`.
Iteration 285 adds running-node delayed-refresh and publication/restart evidence.

The existing TCP record-only catch-up, activation, payment and restart journey
also passes in 34.92 seconds in `/tmp/burst-retained-beacon-284-live.log`.
That fixture retains source proof bodies and supplies its beacon before catch-up;
it does not establish delayed-refresh scheduling. The capped jobs finished and
the validation service is inactive. Release gates remain closed.

Iteration 283 adds delayed beacon import from an authenticated snapshot selection
receipt when the original record is absent. Import checks the peer certificate
against the receipt's parent and compares the complete manifest before attaching
the beacon. Export can reconstruct that missing selection record with its stored
beacon. Neither operation recreates the pruned history or advances payment state.
The new fixture uses a selection at the snapshot boundary and no connected peers.
It rejects a changed beacon round, history heads and checkpoint parent, then
checks successful import and an idempotent retry. Five compact-persistence tests,
six checkpoint-sync tests and strict node library/test Clippy pass in
`/tmp/burst-receipt-refresh-283.log`. The first compile attempt had test-only type
and missing-key API errors; those were corrected before the passing run.
Iteration 284 covers retained-record beacon export. Running-node delayed-refresh
scheduling, fully pruned-source catch-up and release validation remain open.

Iteration 282 fixes the verification-cache mismatch exposed by automatic catch-up.
Local committee publication had moved the persisted target to Voting with the
certified committee; compact applied replay still held the frozen Endorsing
state. Recovery now derives that publication from the replayed request,
authenticated certificate and exact beacon, then compares the entire snapshot.
It does not copy committee membership from the cache or bypass equality checks.
The focused test rejects modified membership/timestamps and missing selection
evidence without changing the candidate. It passes, and the previously failing
TCP catch-up/activation/payment/restart journey passes in 34.97 seconds, followed
by strict node/daemon test-and-binary Clippy in
`/tmp/burst-verification-projection-282-fixed.log`. The capped job exited
successfully. This fixture retains source proof bodies and does not cross height
128. Release gates remain closed.

The broader capped run passes 806 fast node tests, with six ignored and 91
excluded, and 101 snapshot-history tests, with one ignored and live-installation
fixtures excluded. Strict node/daemon test-and-binary Clippy, coverage-index and
whitespace checks pass in `/tmp/burst-verification-projection-282-regression.log`.

Iteration 281 added a running-node record-only catch-up test that initially
failed. After correcting source setup, the receiver still timed out before the
requested epoch. `/tmp/burst-record-catchup-281-diagnostic.log` reports a persisted
verification-engine/replay mismatch during installation, followed by rejected
height-4 imports whose parent is not installed. The source retains proof bodies;
the test disables its separate agreement relay and serves record-only transfers.
Iteration 282 confirms and fixes the committee-publication mismatch. The
regression remains enabled. Public release stays blocked.

Iteration 280 connects completed transfers at snapshot-based nodes to compact
replay and atomic beacon persistence. Sparse export uses bounded version-3
record-only transfers instead of requiring the pruned ledger prefix. Compact
import also accepts version 2, discards its supplied history and retrieves
cursor-checked witnesses through the independent peer proof service. Nodes
without a snapshot retain full-prefix import and reject version 3.
The direct importer fixture passes both formats with beacon recovery and a
wrong-height rejection, followed by all six sync tests and strict node/daemon
test-and-binary Clippy in `/tmp/burst-compact-transfer-280-fixed.log`. The capped
job exited successfully. Initial fixture setup used the wrong mixed-history
loader and was corrected. The test uses real peer proofs but does not exercise
automatic transfer through the running sync loop. Pre-base selection refresh,
later-height live catch-up and history compaction remain unfinished. Release
gates stay closed.

All 805 fast node tests pass, with six ignored and 90 excluded, followed by the
real-peer compact snapshot installation/restart/payment regression in 12.05
seconds. Coverage-index and whitespace checks pass in
`/tmp/burst-compact-transfer-280-regression.log`; the capped job exited
successfully. This regression does not prove automatic record-only catch-up.

Iteration 279 adds atomic compact selection/beacon persistence and a beacon-aware
peer-replay entry point. Beacon validation happens before a writer opens. New
selection and beacon records commit together; delayed attachment preserves the
original certificate. The new fixture checks invalid round, proof and output,
oversized proof, changed writer observations and idempotent alternate signer sets.
It and the existing atomic persistence test pass, followed by strict node/daemon
test-and-binary Clippy and whitespace checks in
`/tmp/burst-compact-beacon-279-fixed.log`. The capped job exited successfully.
The first compile failed on private-field access in the new test, corrected by
reading the stored serialization. Wire routing and pruned export remain
unfinished. Release gates stay closed.

All 99 snapshot-history regressions pass, with one ignored and live-installation
fixtures excluded, in `/tmp/burst-compact-beacon-279-regression.log`. Coverage-index
and whitespace checks pass in the same capped job. This includes existing peer
decision/recovery tests, but not a beacon-bearing wire import journey.

Iteration 278 removes the wire transport's height-128 ceiling while preserving
chunk and transfer byte limits, peer/request matching and digest checks. Polling
uses checked successor arithmetic. All six sync tests pass, including later
heights and `u64::MAX`, followed by strict node/daemon test-and-binary Clippy,
formatting and whitespace checks in `/tmp/burst-wire-heights-278.log`. The capped
job exited successfully. This does not complete compact catch-up: import still
needs replay against the authenticated snapshot base, independent beacon checks
and export without the pruned pre-base ledger. Release gates stay closed.

The existing certified deadline transfer/restart regression and real-peer compact
snapshot installation/restart/payment regression both pass in
`/tmp/burst-wire-heights-278-integration.log`. The coverage index reports no
format errors, with 380 sections still unsigned. These tests do not exercise a
live transition beyond height 128.

Iteration 277 removes the height-128 ceiling from shared compact manifest
builders for deadlines, governance, roster changes, activation and outcomes.
Checked successor arithmetic and parent-state checks remain. Full-prefix replay
retains its separate bounds. A construction fixture passes 129 signed authority
steps and builds governance/outcome manifests at height 130; wrong-parent state
rejects. This does not prove lifecycle execution or live catch-up at that height.
The focused fixture and 801 fast node tests pass, with six fast tests ignored
and 90 excluded, followed by strict node/daemon test-and-binary Clippy and
whitespace checks in `/tmp/burst-compact-proposals-277-retry.log`. The capped job
exited successfully after the first attempt was deferred for low available RAM.
Wire catch-up still calls full-prefix import and needs snapshot-based replay
integration, not just removal of its numeric height check. Release gates stay
closed.

Iteration 276 separates compact checkpoint heights from retained-history limits.
An authenticated height-129 path can be staged and decoded. Full-prefix replay
still rejects oversized history before traversal, and compact persistence still
bounds retained record count and bytes. The storage fixture also checks bounded
rejection at `u64::MAX`; it is not a running height-129 node test.
All 10 checkpoint-storage tests and 800 fast node tests pass, with six fast tests
ignored and 90 excluded, followed by strict node/daemon test-and-binary Clippy
and whitespace checks in `/tmp/burst-compact-height-276.log`.
The capped job exited successfully. Proposal and wire height limits, historical
compaction and persisted incremental authority remain unfinished. Release gates
stay closed.

The real-peer compact snapshot installation, restart and payment regression also
passes in 11.11 seconds in `/tmp/burst-compact-height-276-live.log`. This checks
the normal live path, not operation beyond height 128.

Iteration 275 adds bounded-page authority verification and removes the archive's
separate 128-step limit. The cursor passes 288 heights with electorate changes
and rejects a bad page without partial advancement. Archive-header authentication
passes a direct height-129 test, while an invalid state body still rejects.
The 16 MiB header bound remains, and export checks it before cloning the path.
Three consensus snapshot tests and 98 snapshot-history tests pass, with one
ignored and live-installation fixtures excluded, in
`/tmp/burst-authority-pages-275.log`. Both header tests and strict
consensus/node/daemon test-and-binary Clippy pass in
`/tmp/burst-authority-pages-275-header.log`. Both capped jobs exited successfully.
The archive still holds its complete path in a bounded header. Runtime and wire
height limits, persisted incremental authority and pruning remain unfinished.
Release gates stay closed.

Iteration 274 connects bounded checkpoint inventory scanning to compact recovery.
Installation now rechecks the complete key set under its writer lock as well as
the existing head and record contents. Insertions beyond the old numeric scan
and deletions during recovery reject. Missing successor checks remain in place.
Both new tests and all 798 fast node tests pass, with six ignored and 90 excluded,
followed by strict node/daemon test-and-binary Clippy and whitespace checks in
`/tmp/burst-compact-inventory-274.log`. The capped job exited successfully.
Archive and proposal height limits remain; release gates stay closed.
The real-peer live snapshot-installation, restart and payment regression passes
in 11.97 seconds in `/tmp/burst-compact-inventory-274-live.log`. This guards the
normal running path; the new insertion/deletion cases are writer-level tests.

Iteration 273 adds a byte-bounded, snapshot-consistent checkpoint inventory scan
to full-history loading. It finds malformed keys and orphan records beyond the
old 1–128 probes. The scan bounds retained record count separately from numeric
height. All 97 storage tests and 796 fast node tests pass, with six node tests
ignored and 90 excluded, followed by strict storage/node/daemon test-and-binary
Clippy and whitespace checks in `/tmp/burst-checkpoint-inventory-273-regression.log`.
The capped job exited successfully. One storage documentation test is ignored.
The absolute checkpoint-height ceiling still exists in other paths. Compact
inventory revalidation and incremental authority/history remain unfinished;
release gates stay closed.

Iteration 272 corrects fresh-admission clock tolerance to decision 23.7's 60
seconds in both directions. The former one-hour backdating allowance was not
the selected rule. Ordinary and fork admission share the corrected constant;
new tests cover both inclusive boundaries and rejection one second beyond them.
Certified historical admission and elected-fork replay retain their separate
checks. The fast suite passes 793 tests in 98.20 seconds, six ignored and 90
excluded, followed by strict node/daemon test-and-binary Clippy and whitespace
checks in `/tmp/burst-clock-window-272.log`. The capped job exited successfully.
This does not complete global clock or adversarial timing validation.
The authenticated wire merge-fork test passes in 6.25 seconds, and both payment
epoch integration tests pass in 1.41 seconds, in
`/tmp/burst-clock-window-272-network.log`. Formatting and whitespace checks pass
in the same successful capped job. Release gates remain closed.

Iteration 271 persists a recovered, untouched genesis BRN wallet before publishing
it in memory. This covers genesis-only databases whose metadata existed without
the wallet baseline. Startup still rejects missing BRN history after recorded
spending; it does not reset earned or spent balances. All six startup tests pass
in 0.27 seconds, including both new cases, followed by strict node/daemon
test-and-binary Clippy and whitespace checks in
`/tmp/burst-genesis-baseline-271.log`. The capped job exited successfully. Release
gates remain closed.

Iteration 270 separates fresh fork admission from elected payment replay.
Previously, installation checked the current clock again, so an admitted fork
could become too old while waiting for quorum. Installation now rechecks signed
contents and economic state without repeating the admission deadline. The caller
still requires the matching confirmed election, dependency closure and finality
checks. Fresh admission keeps its timestamp window. All 10 payment-transition
tests pass, including delayed replay and rejection of a forged signature, wrong
parameters and stale payment epoch. This is component evidence, not a delayed
independent-representative quorum test. Release gates remain closed.
The broader run exposed a missing genesis BRN wallet baseline. Genesis creation
now commits that wallet with the block and economic snapshots, so rollback of
the first payment cannot erase it. All six running-node fork tests pass in 13.57
seconds, and the wire-level merge-fork test passes in 5.78 seconds. The 10
payment-transition tests, strict node/daemon test-and-binary Clippy, formatting,
whitespace and coverage-index checks pass in
`/tmp/burst-elected-fork-270-fixed.log`. The capped job exited successfully.
The original failing run is retained in `/tmp/burst-elected-fork-270-regression.log`.
The startup regression passes 789 fast node tests in 98.00 seconds, with six
ignored and 90 excluded, plus eight confirmation-recovery and two corrupt or
missing economic-snapshot tests. `/tmp/burst-elected-fork-270-startup.log` records
the successful capped job. These checks do not complete the remaining release
validation.

Iteration 269 removes a coarse rollback conflict in TRST origin ownership.
Undo now compares individual origin memberships within a creator's set rather
than rejecting every later change to that set. Unrelated origins survive;
conflicting membership still rejects before any set is changed. Canonical
snapshot and cross-owner uniqueness validation remain in place. The TRST suite
passes 90 tests with two ignored, and all 14 node rollback-plan tests pass in
`/tmp/burst-origin-undo-269-fixed.log`. Strict TRST/node/daemon test-and-binary
Clippy, formatting and whitespace checks pass in the same successful capped job.
This does not resolve conflicting holdings, merge revisions, or missing lifecycle
dependencies. Finality and dependency-closure checks are unchanged.
The broader regression passes 787 fast node tests in 98.39 seconds, six ignored
and 90 excluded, plus the authenticated wire-level merge-fork test in 5.78
seconds, in `/tmp/burst-origin-undo-269-regression.log`. The capped job exited
successfully. That existing wire test is regression evidence, not a new
independent-signer same-creator branch scenario. Release gates remain closed.

Iteration 268 injects a test-only error after live snapshot commit and before
force-sync. The node reports the failure, retains the old in-memory election
authority and closes its RPC/P2P listeners rather than serving it against the
changed database. Real-peer restart recovers the new archive, matches lifecycle
state and accepts a valid payment after rejecting a stale one. The test passes
in 10.64 seconds in `/tmp/burst-live-snapshot-sync-failure-268-fixed.log`, followed
by strict node/daemon test-and-binary Clippy, formatting and whitespace checks.
The capped job exited successfully. The fault control is absent from production
builds. This checks the software failure path, not actual failing storage or
power-loss durability. Source gates remain closed.
The fast regression passes 787 tests in 95.56 seconds, with six ignored and
90 excluded, in `/tmp/burst-live-snapshot-sync-failure-268-regression.log`.
Coverage validation reports 388 sections, 380 not signed off and no format
errors. The capped regression job exited successfully.
The normal real-peer live-install journey also passes with injection disabled
in 10.82 seconds in `/tmp/burst-live-snapshot-sync-failure-268-normal.log`.

Iteration 267 extends the five SIGKILL checks to live replacement of an existing
compact base. The child must report the older installed epoch before crash
probes arm and replacement begins. A pre-commit crash preserves the old archive
and recovered state; post-commit crashes recover the new archive and state.
Every case preserves the captured ledger count and exact old-epoch signing
promise, then restarts, rejects a stale payment and accepts a valid payment.
All five boundaries pass in 261.60 seconds in
`/tmp/burst-live-snapshot-replacement-crashes-267.log`. Strict node/daemon
test-and-binary Clippy, formatting and whitespace checks pass in the same
successful capped job. This adds replacement process-crash evidence, not power
loss, disk faults or arbitrary scheduling. Production code and gates are unchanged.

Iteration 266 adds SIGKILL checks for first-time live snapshot installation at
five publication boundaries: prepared, committed and synced, economics,
authority and complete. Each disposable node had open RPC and P2P listeners
before the kill. Reopen preserves the boundary's ledger count and the exact
final-vote promise. Pre-commit state retains the old epoch and no snapshot base;
post-commit recovery matches the authenticated archive. Real-peer restart rejects
stale payments and accepts valid payments in every case. All five boundaries
pass in 251.08 seconds in `/tmp/burst-live-snapshot-crashes-266-fixed.log`, followed
by strict node/daemon test-and-binary Clippy, formatting and whitespace checks.
The capped job exited successfully. This is process-crash evidence for initial
installation, not existing-base replacement, power loss or disk faults. No
production code or release gate changed.

Iteration 265 verifies live installation and replacement using the nodes' normal
authenticated peer connections and ancestry-proof services. The tests withhold
ordinary checkpoint requests until installation finishes so catch-up cannot
preempt the requested snapshot. They then restore checkpoint delivery and check
payments and restart. Neither case uses the controlled proof adapter. All five
live-install tests pass in 40.80 seconds in
`/tmp/burst-live-snapshot-peer-265.log`. Certificates and clocks remain fixtures;
arbitrary network scheduling, full signing-race coverage and live-install process
crashes remain open. Production behavior and source gates are unchanged.
Strict node/daemon test-and-binary Clippy, formatting, whitespace and coverage
index checks pass in `/tmp/burst-live-snapshot-peer-265-static.log`. Both capped
jobs exited successfully. All four source gates remain false.

Iteration 264 checks changes made while live snapshot installation waits for
history proofs. A new final-vote promise survives installation and restart.
A new unresolved confirmation target makes publication fail without changing
the epoch, block count or metadata, including the durable confirmation queue.
All three live-install tests and strict node/daemon test-and-binary Clippy pass
in `/tmp/burst-live-snapshot-races-264-final.log`; formatting and whitespace
checks pass and the capped job exited successfully. Production code and gates
are unchanged. These tests control the proof handoff and insert local durable
records directly. They do not cover every signer/commit ordering or real peer
delivery during installation. The 787-test fast regression remains the previous
iteration's evidence, not a new run.

Iteration 263 adds a gated local snapshot-install handle for a running node.
It authenticates the archive and fetches history proofs outside the ledger worker.
The worker then rechecks finality and signing promises in its writer, commits
and synchronizes the projected state, and publishes matching runtime engines
under the existing admission and election locks. No remote RPC or pruning
permission was added. Full-history installation and replacement of an existing
compact base pass payment and restart checks in 9.88 seconds in
`/tmp/burst-live-snapshot-install-263-replacement.log`. The same capped job passes
strict node/daemon test-and-binary Clippy and exited successfully.
The installation tests use a controlled authenticated proof-transport adapter;
restart uses real peer transport. Full peer-delivered live installation,
signing races and live-install process-crash coverage remain open. Release
gates are unchanged.

The expanded cancellation/concurrency check also passes: while proof retrieval
is held, a second install is refused; cancelling the first leaves no installed
base and preserves signing promises. Reusing the handle then succeeds. Both
live journeys pass in 9.77 seconds in
`/tmp/burst-live-snapshot-install-263-verified.log`.
The same job passes 787 fast tests in 94.93 seconds, with six ignored and 83
excluded, strict node/daemon test-and-binary Clippy, formatting and whitespace
checks. The coverage index has 388 sections, 380 unsigned and no format errors.
That is a review backlog, not a completion percentage. The capped job exited
successfully; all four source gates remain false.

Iteration 262 adds a running checkpoint-service handoff from full-history mode
to authenticated compact recovery. It preserves the inbox and a dequeued packet,
gives shutdown priority and rejects a corrupt base without falling back to the
old runtime. Four service tests pass. The broad live run passed eleven cases;
settled-verification bootstrap hit the queue's transient full-or-contended
response. A bounded test retry preserves all admission assertions, and that case
passes in 18.98 seconds in `/tmp/burst-live-base-handoff-262-verified.log`.
The same job passes all four runtime checks, 787 fast tests in 87.88 seconds,
six ignored and 81 excluded, strict node/daemon test-and-binary Clippy, formatting
and whitespace checks. The job exited successfully.
This is service-level handoff evidence. The test publishes a base directly in
a disposable store without changing installed economics. Coordinated live
snapshot installation, publication races and full-node replacement validation
remain open. Source gates are unchanged.

Iteration 261 verifies post-snapshot review progression over running peer
connections. A target first becomes ready on an active-review archive. Only then
does its peer receive the later certified deadline or appeal outcome. Both nodes
install the successor; lifecycle records, balances, rewards and TRST provenance
agree. The target retains its original compact base, accepts payments and
restarts on the successor epoch. Fraud/inactivity deadline cases pass in 53.25
seconds in `/tmp/burst-review-progression-261.log`; successful appeals of both
judgments pass in 83.40 seconds in `/tmp/burst-review-progression-261-final.log`.
Certificates and clocks remain fixtures. This tests transport, installation and
recovery, not independent quorum formation or adversarial scheduling. No
production code or source gate changed.
The same final job passes settled-appeal regression in 58.92 seconds, 783 fast
tests in 84.88 seconds, six ignored and 81 excluded, strict node/daemon
test-and-binary Clippy, formatting and whitespace checks. It exited successfully.

Iteration 260 adds active challenge and active appeal archive bootstrap for both
fraud and inactivity. Recovery and restart preserve the original judgment,
committee, activation deadline, locked stakes and outcome records. Each journey
rejects a stale-epoch payment and accepts valid payments before and after restart.
Both challenge cases pass in 34.68 seconds and both appeal cases in 48.28 seconds
in `/tmp/burst-active-review-bootstrap-260.log`. These tests use fixture
certificates and controlled clocks. Completing a new certified review after
archive recovery and rotating a running worker's base remain separate work.
No production code or source gate changed.
The settled-appeal regression passes in 56.68 seconds, followed by 783 fast tests
in 86.77 seconds, six ignored and 79 excluded. Strict node/daemon test-and-binary
Clippy, formatting and whitespace checks also pass in
`/tmp/burst-active-review-bootstrap-260-regression.log`. The capped job exited
successfully.

Iteration 259 adds full-node archive bootstrap and restart for settled fraud and
inactivity challenges, plus successful appeals of both. Checks cover outcome
receipts, certification markers, identity state, holder balances, verification
state, stakes and pending rewards. TRST wallet and merger-graph bytes match the
full-history source before payment. All seven bootstrap tests pass in 190.46
seconds in `/tmp/burst-lifecycle-bootstrap-259-verified.log`.
The same capped job passes 783 fast tests in 96.61 seconds, six ignored and 77
excluded, strict node/daemon test-and-binary Clippy, formatting and whitespace
checks. It exited successfully.
Fixture setup now installs signed history and certificates sequentially; absent
accounts must be absent on both nodes. No production behavior or source gate
changed. Active challenge/appeal recovery, in-place worker rotation and the other
recorded release requirements remain open.

Iteration 258 checks five process-crash boundaries while replacing an existing
compact base with an active-panel archive. Before commit, the old base, epoch,
block count and metadata remain intact. After commit, recovered state matches
the authenticated archive. Accepted certificates and an unresolved final-vote
promise survive every boundary; each restart admits a payment and rejects the
old payment epoch. All five cases pass in 250.69 seconds in
`/tmp/burst-base-replacement-crashes-258.log`. This adds tests, not production
behavior. Power-loss recovery, challenge/appeal archives and in-place worker
rotation remain outside this evidence. Source gates remain closed.
The normal replacement journey, strict node/daemon test-and-binary Clippy,
formatting and whitespace checks also pass in
`/tmp/burst-base-replacement-crashes-258-static.log`. No full-suite rerun was
needed for this test-only addition; iteration 257 retains the latest fast-suite
evidence.

Iteration 257 adds forced-process-crash checks during archive download, before
file publication and after publication but before ledger installation. The test
requires closed RPC and genesis-only ledger state, then restarts, recovers the
active panel and submits a payment. The first run exposed a test comparison
against the serving node's newer payment state; it now uses the original archive.
All three boundaries pass in 131.07 seconds in
`/tmp/burst-download-crashes-257-verified.log`. The same run passes 783 fast tests
(six ignored, 74 excluded), strict node/daemon test-and-binary Clippy, formatting
and whitespace checks. The capped job exited successfully. Pause hooks are
test-only; source gates remain closed.

Iteration 256 adds startup replacement of an older compact base with accepted
successors. The first run passes certificate-byte preservation, active-panel
recovery, payments and restart in 10.66 seconds (`/tmp/burst-base-replacement-256.log`).
The strengthened run preserves an unresolved durable final-vote promise too.
All five bootstrap journeys pass in 79.32 seconds in
`/tmp/burst-base-replacement-256-final.log`, followed by strict test-inclusive
node Clippy, formatting and whitespace checks. The capped job exited successfully.
This does not cover changing a
running worker's base or every provisional-tail case. Gates remain closed.

Iteration 255 connects an explicitly configured peer download to startup archive
installation. Recovery gets a bounded snapshot-only inbox; ordinary traffic stays
closed. Fetch alone still stages a file. The initial started-node active-panel
download and payment/restart test passes in 16.68 seconds in
`/tmp/burst-snapshot-download-255-live-fixed.log`. Strengthened admission checks,
cancellation and inbox isolation also pass. All four bootstrap journeys, including
the strengthened closed-admission check, pass in 70.08 seconds in
`/tmp/burst-snapshot-download-255-final.log`. The same run passes 783 fast tests
(six ignored, 72 excluded), strict node/daemon test-and-binary Clippy, formatting
and whitespace checks. The resource-limited job exited successfully.
No release gate changed.

Iteration 254 adds completed-verification archive bootstrap. All three bootstrap
journeys pass in 46.81 seconds with full orchestrator, BRN stake-state and
pending-reward comparisons before and after restart. Evidence is in
`/tmp/burst-bootstrap-settled-254-verified.log`. Test-inclusive Clippy found an
existing proof-service test-module ordering warning; the unchanged tests now live
in a separate file. Strict test-inclusive Clippy, the proof-service test,
formatting and whitespace checks pass in `/tmp/burst-bootstrap-settled-254-static.log`.
These tests use signed fixture history and controlled clocks. Replacement crashes,
challenge/appeal archive journeys and in-place worker rotation remain open.

Iteration 253 fixes active-panel snapshot bootstrap. The new started-node test
first failed because the archive omitted the exact beacon proof. Archive framing
version 2 carries independently checked beacon supplements outside the unchanged
version-5 economic body. Bootstrap now preserves the selected committee and
activation deadline, admits a payment, restarts and admits another payment.
The initial passing live run is `/tmp/burst-bootstrap-active-253-fix.log`.
Malformed-proof checks, both bootstrap journeys and 781 fast tests pass in
`/tmp/burst-bootstrap-active-253-final.log`. Six tests are ignored and 70 excluded
from that fast group. Strict node/daemon Clippy, formatting and whitespace checks
also pass. Automatic download handoff and base replacement remain open.
Gates remain closed.

Iteration 252 passes five forced-process-crash boundaries during explicit archive
bootstrap. RPC remains closed before admission; restart finds genesis before
commit or the installed snapshot after commit, then stores a valid payment.
The test covers commit, force-sync and runtime-cache publication boundaries.
It is not a power-loss test or evidence for every lifecycle/archive combination.

Iteration 251 fixes archive-backed selection publication. The empty-node startup
archive journey now passes installation, peer-proof retrieval, payment admission,
restart and another payment. A separate committee test passes exact beacon
persistence and reopen without historical checkpoint records. Automatic
replacement crashes, challenge/appeal lifecycle bootstrap and
in-place worker rotation still need end-to-end evidence. Gates remain closed.
Iteration 250's genesis fix also remains: new databases use the canonical genesis
block independently of local signing-key availability. Existing databases were
not rewritten.

Iteration 249 fixes downloaded-snapshot preparation's missing accepted-head
update. The head now shares the ledger/economic transaction. Sparse stored-base
reconciliation preserves retained certificates and rejects missing successors,
conflicting records and damaged archives. All 18 reconciliation checks pass.
That preparation is now used by the gated startup path above. No release gate
changed.

Iteration 248 adds bounded reuse of authenticated historical proofs. The same
two-node restart and five-boundary crash journeys pass in 275.33 seconds combined,
compared with 702.59 seconds across the previous runs. This is fixture timing,
not a production benchmark. Cached bodies remain bound to their authenticated
snapshot and account prefix; installation still performs its final writer checks.
The cache holds at most 128 bodies and 1 MiB of encoded block data.

Iteration 247 passes forced process crashes at five boundaries of the second
compact successor installation. Each case checks authenticated persisted state,
restarts the node, rejects a stale-epoch payment and stores a valid payment.
Restored economic, authority and ledger-count caches match storage. The test
exposed and fixed a recovery race when a normal payment changes retained history
without changing the installed epoch. Authentication now retries if a failed
attempt overlaps a database revision change; stable-state errors still fail.

Iteration 246's two-node installation, RPC payment relay and restart journey
also passes again. The sparse node never has the full checkpoint prefix.
Mixed-lifecycle multi-height validation remains open. The crash fixture
uses supplied certificates and controlled clocks, not fresh Byzantine agreement
or a power-loss simulation.

Iteration 252's regression and bootstrap evidence remains in
`/tmp/burst-bootstrap-crash-252-final.log`; its crash evidence is in
`/tmp/burst-bootstrap-crash-252.log`. The latest checks are recorded above.

Iteration 245 connects compact successors to the gated checkpoint publication
runtime. All six event types pass compact commit and installed-state recovery.
The runtime-object test checks engine/frontier/count publication, queue clearing,
old promise preservation and idempotence. Coordinator rotation now uses the
checked commit. Its five focused checks and 774 fast tests passed before the
started-node work above. This is not a completed public release.

Iteration 244's preparation and recovery fix remains covered. Lifecycle
projection uses the final verification engine once, avoiding intermediate
snapshot comparisons and duplicate undo rows. Snapshot bytes are unchanged.

- Owner-authorized expired-proposal recovery uses later consensus rounds without
  clearing durable locks. Independent signer stores, restart and TCP service
  tests pass under controlled clocks. Delayed prepare proofs retain their exact
  checkpoint and beacon. See `DEADLINE_RECOVERY.md`.
- Quorum-authenticated economic state has canonical encoding, restoration and
  bounded tail replay across certified checkpoints. Archives authenticate authority and signatures
  before reading the body. The daemon can attest, export and verify named epochs;
  the executable operator journey passes, including insufficient-quorum rejection.
  Opt-in peer staging passes multi-chunk TCP transfer and restart checks without
  changing payment authority. Forged headers and modified/truncated archives are
  rejected; existing destination files are preserved.
  Availability discovery now checks an independent minimum height against the
  authenticated header. Two fully started nodes pass discovery, transfer over
  their normal handshake/routing path and receiver restart without the server.
  See `SNAPSHOT_ARCHIVE.md`.
- Historical membership recovery now verifies bounded chunks from authenticated
  account heads. A new suffix reader matches full-history commitments while
  reading no old blocks from its input store. Compact same-epoch execution now
  shares the ordinary economic executor. Six new tests cover payments, panels,
  governance, authenticated bodies and rejection paths. Full-state byte
  comparisons also exposed and fixed nondeterministic embedded verifier maps.
  Compact continuation also matches full replay through selection, activation,
  deadlines, fraud outcomes, appeals, roster changes and governance cuts.
  Direct manifest validation covers non-selection events, and successor
  selections now reconstruct their frozen inputs from compact state. Current
  archives also restore pre-base selections without ledger reads when they carry
  the historical parent authority path. Recovery-only
  records cannot authorize local signing or satisfy its inclusion fences.
  The gated live installer now consumes compact successors. A two-successor
  started-node admission/restart journey and five second-successor process-crash
  cases pass. Explicit archive startup and its five-boundary crash journey pass.
- Economic snapshot version 5 retains replay-derived mint-index data and keeps
  the proof-independent verifier execution state introduced in version 4.
  The version 4 change removed quorum-proof variability from verifier
  execution state. Five independent stores produce identical state bytes with
  different valid proofs of the same selection. Certificate and beacon checks
  remain mandatory; existing attestation journals are preserved.
- Snapshot installation now has transaction-owned reconciliation of cemented
  heads, pending confirmations, local checkpoint decisions and signer journals.
  Ten reconciliation tests pass. Compact ledger projection now accepts
  authenticated heads and witnesses while preserving retained cemented bodies.
  Full and compact installation now share economic projection. The compact
  writer also stores an authenticated recovery base and closes the old epoch
  without dropping signer promises. Its gated startup-only commit now connects
  explicit archive installation to installed-state/cache recovery. The fresh-node
  journey and process-crash checks pass. Explicitly requested download-to-install
  now passes the fresh-node payment/restart journey too, including process crashes
  around download publication. Startup base replacement preserves accepted history
  and promises; replacement crashes and running-worker rotation remain open.
- Stored recovery now reads the authenticated base and later checkpoint records
  after reopening a suffix-only store. Comparisons with full replay cover the
  lifecycle and governance boundary tests. Older required beacon proofs are
  retained separately and reauthenticated. Missing or altered recovery inputs
  fail without writes. Stored recovery supports both a caller-supplied proof
  provider and automatic multi-account peer retrieval. Explicit archive startup
  now uses this path; broader lifecycle/bootstrap coverage remains unfinished.
  Stored recovery now derives its bounded witness request list from the checked
  suffix and authenticated state. Its proof provider no longer uses full replay
  to discover dependencies.
- On-demand historical proof fetching now uses authenticated peer connections.
  It checks ancestry against snapshot heads, rotates stalled peers and bounds
  requests, responses, retained bodies and total work. A two-node TCP test passes
  proof retrieval without installing blocks or changing payment authority.
  Stored successor recovery now connects this transport to the checked
  multi-account plan. Account hints cannot replace membership proofs, and all
  account walks share the same resource limits.
  Witness transfer now runs in a separate read-only service with its own bounded
  inbox, independent of checkpoint authority initialization. Tests cover early
  start, queue reuse, routing and shutdown without metadata writes. The node's
  registry now starts in proof-only recovery mode and opens ordinary traffic
  after existing startup restoration. Socket tests cover admission filtering,
  retransmission after handoff, outbound filtering and terminal shutdown. The
  authenticated listener and proof service now start before authority restoration.
  Tests fetch a proof while restoration is paused, reject block admission and
  verify cleanup after restoration or listener-binding failure. Authority replay
  runs on a blocking worker. Recovery now also dials configured seeds and cached
  peers through the existing authenticated connector, with retries until
  handoff and shared duplicate-dial protection. A paused-startup TCP test covers
  both address sources and proof replies. Gated startup now routes an installed
  compact base through the recovery publication coordinator. Complete pruned-node
  startup and interruption validation remain unfinished.
- Compact installed-metadata validation now reads retained snapshot heads and
  the cemented suffix instead of the entire prefix. It checks the installed
  authority and voting limits, then reconstructs the governance schedule without
  including provisional blocks. A writer-owned recheck rejects changes during
  proof retrieval. Gated compact startup now consumes this validation.
- Exact installed-epoch recovery now connects the stored base, certified suffix,
  peer proof delivery and installed-metadata validation. It does not substitute
  a newer accepted checkpoint. A final writer-owned check pins the archive,
  required checkpoint records and accepted-tip marker as well as the observed
  ledger cut. Live compact startup still needs the remaining state checks.
  The staged undo projection now retains a matching replay-derived runtime
  bundle. Its TRST and account caches are checked at the same recovery clock,
  alongside the installed anchor, frontiers and counts. Eleven recovery tests
  pass, including comparison with committed fixture rows. A gated startup
  coordinator holds all publication locks, prepares elections and frontiers,
  commits and force-syncs before replacing live engines and authority. The
  admitted-order fixture checks publication while ingress remains closed.
  Compact coordinator crash recovery now passes SIGKILL at prepared, synced,
  economic-publication and complete-publication boundaries. Reopening and
  republishing restores matching state, authority, caches, frontiers and counts
  while ingress stays closed. Full pruned-history worker operation and full
  startup crash tests remain; this fixture calls the coordinator directly.
  Checkpoint-sync initialization now authenticates a retained archive and its
  replayed successors, fetching missing ancestry through the proof worker and
  rechecking base and record bytes before use. Sparse-history and 18 sync tests
  pass. Agreement and checkpoint import/export still require compact-history
  integration; this initializer does not grant signing or import authority.
  Initialization also rejects an accepted-tip marker below the recovered
  archive. Two new tests cover missing-tip and damaged-suffix rejection without
  writes; all eleven installed-recovery tests and strict Clippy passed in
  iteration 206. This does not establish live pruned-node agreement.
  Compact successor recovery now retains its last replayed decision and can
  construct the same bounded relay as full-history agreement under the original
  parent context. Base-only archives supply no invented decision manifest.
  The relay itself does not supply compact candidate replay or signing authority.
  Selection proposals can now replay without a decision certificate against a
  private copy of an authenticated continuation. Exact manifest comparison and
  unchanged-base checks pass, while the existing signing inventory requirement
  remains enforced. The later candidate integration below adds signing support.
  Compact selection coverage can now preflight current final-vote promises
  using authenticated heads, witnesses and suffix hashes. It detects omitted
  promises and epoch changes without writing fences or journals. This check
  must run again inside the eventual signing transaction; it grants no signing
  authority on its own.
  Candidate coverage also carries installed-authority/cutoff/vote-limit
  expectations and authenticated account positions. A writer-owned finality
  preflight rejects changed metadata, regressed or conflicting cemented heads
  and unproved confirmation targets. A gated compact-selection API now repeats
  those checks inside the existing signer-journal transaction, requires the
  accepted parent tip and atomically seals final-vote promises. Complete frozen
  selection receipts are mandatory. The ordinary full-history API still rejects
  bare compact replays. All six event kinds, including governance, now
  share compact driver and single-height service integration. Peer replay checks
  unknown manifests before signing; retained journals recover through the same
  path. Certificate persistence uses the existing chain encodings and does not
  install economic state. Outcome settlement still uses the full-history
  executor and exact manifest constructor. Compact discovery now derives all
  six candidate kinds from a checked confirmed suffix using the archival
  runtime's priority order and exact governance cut. Its peer-backed entry point
  rechecks authority and local metadata without writes. Service startup now
  recovers retained choices first, discovers only when none exist, and returns
  an unchanged idle base when time or a due event is absent. Continuous idle
  retry, observer admission, coincident competing lifecycle checks, economic handoff and
  runtime height rotation remain unfinished.
- Retained-history recovery checks the complete physical block/index inventory.
  Older retained bodies must belong to the authenticated account prefixes.
  Local contiguous history needs no peer fetch; sparse gaps use membership proofs.
  The final writer-owned recheck pins every retained row and account observation.
- Installed recovery now validates and pins local signer journals, including
  later accepted decisions without advancing the installed epoch. Applied tails
  use shared economic replay without confirming provisional heads or executing
  deadline settlements. Stored account, BRN, pending-transfer, TRST provenance,
  verification and governance state must match replay. Time-derived expiry is
  normalized only in private comparison copies. The staged index/undo projection
  and gated publication coordinator below now consume this checked state.
- Compact recovery now checks frontier and pending-sender lookups against their
  primary rows in the same transaction. Seven corruption cases reject without
  repair. Expiry-index cleanup now deletes exact stored keys in batches of at
  most 1,024 rows per node tick. It preserves origin and reverse mappings,
  ledger bodies and token portfolios. Two storage tests cover bounds, cutoff
  handling, provenance preservation and atomic rejection of damaged rows.
- Recovered mint history can now reconstruct origin, expiry and reverse lookups
  under a writer-owned startup guard. It rechecks recovery inputs before writing
  and commits only through the gated recovery publication coordinator after
  undo and runtime preparation. Expiry cleanup and earlier governed periods do not establish a
  different mint history.
- Undo recording and decoding now forbid changes to snapshot-attestation
  promises. Recovery pins the bounded undo inventory and rejects malformed
  keys, versions and records. These structural checks alone do not authenticate
  economic before-images; the replay-derived staging below is separate work.
  Nine recovery tests and fourteen rollback tests pass after this change, as
  does strict storage/node/daemon Clippy.
- Applied replay now streams intermediate states through a fallible internal
  observer without retaining a full state clone per block. Its test checks
  account transitions, unchanged finality and failure isolation. The uncommitted
  startup projection now derives bounded economic row deltas from those
  observations, including primary block/height rows, frontiers and mint indexes.
  Writer-owned reconciliation now excludes finalized blocks and preserves
  actual confirmation heights in provisional account images. Derived expiry
  caches now use one authenticated cutoff clock and preserve revocation and
  signed balances. Matching engine bytes and reconstructed journals can now
  stage in one uncommitted writer. A disposable test fixture publishes and
  rewinds a provisional payment tail while preserving its confirmed prefix.
  The later gated publication coordinator commits this projection and replaces
  runtime state under its publication locks. Complete live lifecycle recovery
  still needs end-to-end evidence.
  Recovery can now stage both journals and final account/portfolio caches at a
  supplied startup clock without changing signed execution or deadline state.
  The payment fixture publishes expired caches and rewinds its provisional tail
  while preserving the confirmed prefix. A clean database close/reopen now
  reauthenticates that state and reconstructs identical journals before rewind.
  The separate iteration 207 coordinator test covers process crashes, but not
  complete live compact startup with its remaining workers.
- A separate TRST snapshot rewind now normalizes expiry at a supplied clock and
  governed period. Tests preserve decision 6.9's expiry extension behavior while
  rejecting overlapping spending, ownership changes, revocation and malformed
  snapshots. Live rollback still uses the strict matcher until restored account
  counters and token state can be published in the same transaction.
- Replay now records bootstrap identity sources and binds the first subsequent
  account block through admission's existing helper. The test uses a causal
  payment dependency, not timestamp ordering between independent chains.
  Applied-tail recovery now uses local identity links as bounded ordering hints,
  then authenticates and executes the blocks and compares the complete result.
  Reconstructed journals use that same order. Certified replay cannot use these
  hints. Both neutral-account arrival orders pass a focused regression; mixed
  lifecycle and sparse-source recovery remain unfinished.
  Historical ordering sources can now come from the exact wallet record in the
  authenticated base without fetching an old body solely to order the suffix.
  Two new tests and the ten recovery tests pass. Live rollback still needs its
  separate historical-source handling.
  A stored-recovery fixture now covers three admission-helper interleavings
  across bootstrap exit. Recovery reconstructs their actual journal order, and
  test-only publication followed by rewind preserves the confirmed prefix and
  restores the prior verified count. All eleven recovery tests pass. This does
  not establish running-node admission/election or live startup publication.
- The most recent fast run passed 780 node tests after bootstrap crash validation
  in iteration 252, with six ignored and 69 excluded. A previously
  misleading peer-recovery fixture now requires an actual historical dependency
  and successful proof retrieval. Separate
  seven-store TCP service tests passed agreement, persistence and session recovery
  for all six event kinds. Each now also checks automatic discovery against its
  full-history manifest without changing the authenticated base. Proposers start
  without a supplied manifest, and all signers resume after reopen without a
  clock. Each event also passes keyless observer admission, reopen and duplicate
  acceptance without changing accounts or payment authority or creating a signer
  journal. Observer admission rechecks recovered records in the persistence
  transaction. Invalid certificates, false replay effects and authority changes
  during proof retrieval are rejected. Iteration 238 adds a single-height
  observer packet worker. All six fixtures pass decision receipt and validated
  relay over TCP followed by worker shutdown. Those fixtures alone did not test
  continuous node runtime selection, economic handoff or height rotation. Iteration 239 adds
  shared-base startup retry. A saved deadline survives an unavailable proof
  service and resumes when proofs return, without a clock or journal changes.
  Iteration 240 adds the single-height idle worker: all six TCP fixtures pass
  scheduled discovery after missing time and incoming-proposal startup for a
  follower without a local candidate. Iteration 241 adds the multi-height
  coordinator's installed-authority handoff check.
  Ten sparse-store cases pass, including rejection of rollback, foreign authority,
  an epoch marker without matching state and changes during proof retrieval.
  A newer accepted tip does not replace the installed authority. Proof delivery used the
  authenticated test channel. Outcome boundary checks also cover signing,
  restart, prepare-proof recovery and unchanged economic state during persistence.
  Fresh activation signatures now use the governed timeout at the activation cut,
  matching the actual voting window. Shorter and longer governed windows pass.
  Those checks did not cover compact runtime height rotation; iterations 243–246
  add the compact coordinator, checked installation and the started-node journey
  described above. The
  separate four-boundary compact crash test and strict node/daemon Clippy passed.
  The TRST suite passed 87 tests with
  two ignored. Strict TRST/storage/node/daemon Clippy passes after iteration 192.
  Iteration 193's startup cache-clock change passes ten installed-recovery tests,
  fourteen rollback tests and strict node/daemon Clippy.
  Iteration 196 also passes the ordering regression, ten installed-recovery
  tests and strict node/daemon Clippy.
  Three new compact installed-metadata checks pass, including sparse-store
  reopen, provisional exclusion, stale observations and nine corruption cases.
  Ten installed-recovery tests also pass, including exact target selection,
  retained-history corruption, provisional economics, persisted engines, signer
  journals, later expiry-cache normalization and changes during proof delivery.
  The current two-node journey covers
  recovery of installed metadata and older retained rows at two epochs, multi-account proof fetching and
  certified successor recovery into a separate suffix-only store. Recovered
  economic state matches full replay, with no old ledger bodies or metadata
  writes in the recovery store. The independent protobuf schema check passed
  in the preceding transport iteration; the schema has not changed since.
  Current storage validation passes 96 LMDB tests. The token-index projection
  change passed the focused recovery suite and the subsequent fast run. The
  focused TCP archive-transfer/full-node restart test passed again after the
  iteration 204 compact publication coordinator.
  Two process-crash publication tests also pass against
  the shared full-history installer, including the running admission worker.
  A rebuilt debug daemon passes version-5 attest/export/verify, and three
  snapshot peer-transfer/restart tests pass with the new body format.
  Earlier evidence includes 123 verification tests and two running-node
  settlement/deadline journeys with restart and fresh-node catch-up. These
  checks do not cover the full excluded slow/network suite,
  production timing or packaged releases.
- The iteration 161 debug-daemon build and RPC/restart smoke checks passed.
  The optimized build was interrupted by a desktop reboot; it did not pass.

## Work that still blocks the requested release

1. Finish full-node snapshot bootstrap and recovery validation.
   Successor installation now shares the gated atomic cache publisher.
   Fresh downloaded-base installation now passes admission and restart. Complete
   crashes during base replacement, broader lifecycle archives and replacement
   with running workers or additional provisional-tail cases.
2. Replace full-prefix replay dependencies with authenticated retained state and
   bounded tails across certified boundaries before enabling pruning. Selection
   receipts and the ordering inventory retain historical inputs conservatively.
   On-demand archival witness retrieval is wired into peers; automatic compact
   startup selects the compact coordinator. Switching a running full-history
   worker after base installation remains unfinished. Current history
   and checkpoint-chain bounds still stop long-running progress.
3. Review selection recovery under adversarial message scheduling and selective
   abort. Test actual production clock bounds and beacon timing. Consensus lock
   safety alone does not establish unbiased verifier selection.
4. Complete the recorded competing-branch, partition and mixed lifecycle/
   governance checks, including overlapping economic fork replacement. Preserve
   cemented history, provenance, revocation and appeal behavior.
5. Finish the remaining wallet/node native journeys and packaged-platform
   validation, then validate the actual release artifacts and signing. Existing
   Linux tests and older Windows service results are not current macOS/Windows
   package evidence.
6. Complete requirement-by-requirement sign-off and the publishing workflow's
   release check. The inventory has 388 source sections and 380 unsigned entries.
   Those counts are a review backlog, not a missing-feature count or a completion
   percentage.

`REFERENCE_IMPLEMENTATION_REVIEW.md` contains the detailed queue and evidence.
`coverage.json` indexes the authoritative whitepaper and implementation decisions.
Neither this summary nor a green test permits removing an intended feature.

## Desktop validation limits

Use `bash tools/limited-run.sh <command> ...` for local validation. The wrapper
limits the whole process group to 3 GiB RAM, no swap and one CPU equivalent.
Only one job can run at a time. It defers below 4 GiB available memory and stops
after 20 minutes. Optimized release builds remain deferred on this desktop after
the reboot. A resource-limited failure must remain recorded as a failure.
