# Structural audit fixes — current handoff

This record covers its dated source batch. Earlier stages below are historical;
consult the final disposition before reopening a finding. Later source edits require
input-scope review before reusing its receipts.

Scope: F01–F12 in the original folder audit and R1–R8 in the
[post-fix re-audit](../evidence/structure-reaudit-20261030.txt). Earlier findings describe their
inspection snapshots; this file records current disposition. Source structure is
not a release/security certification.

| Finding | Current disposition |
|---|---|
| F01 / R1 | Dashboard sibling checkout, full-SHA pin selection and preflight implemented. Source publication and clean remote CI qualification remain pending. Both apps need a published coherent protocol snapshot containing http_client. |
| F02 / R2 | Listener composition moved to daemon library. Node starts core workers, invokes the supplied application factory, and supervises returned tasks through existing shutdown/draining. Node-facing service state compiles without server features. |
| F03 / R3 | Live envelope, codec, schema, wire tests and pure transfer bounds belong to messages. PeerAddress, TrafficClass and ProtocolError belong to types; network no longer depends on messages/protocol/ledger. Legacy Bincode code is explicitly isolated. |
| F04 | Block admission/retention belongs to node; low-level network has no ledger/work dependency. |
| F05 / R7,R8 | Live block/hash ownership retained to preserve delegation encodings. Unused ledger/pruning interfaces isolated under legacy with deprecated compatibility exports. A wholesale ledger rewrite is not justified by this structural task. |
| F06 / R6,R7 | Generic collections moved to utils; direct consumers updated and TRST aliases retained. Persistent-map semantics/serialization preserved. Backend hydration stays with pinned views and generation-tagged caches. |
| F07 / R4 | Peer and checkpoint service families grouped. Immutable retirement catalogue and worker now have a direct confirmed_replay/retirement owner. Checked closure operations deliberately remain beside private reader/proof fields; those fields were not exposed to flatten paths. |
| F08 / R5 | Implemented: rpc/messages owns bidirectional contracts/pagination and wallet_core consumes its balance contract. Account/governance handlers have explicit imports and common validation/conversion helpers. Service state remains separate from optional listeners; compatibility imports retained. |
| F09 | Current documentation index and architecture/startup guide updated. Historical evidence paths retained. |
| F10 | Production in-memory delegation store belongs to store; node nullables dependency is test-only. No new durability behavior is claimed. |
| F11 | Independent applications and small UI copies retained. There is no demonstrated benefit from adding a design-system package solely to remove these copies. |
| F12 | Duplicate node logging initializer removed; navigation policy isolated. Utility logging/runtime features are optional for economic consumers. |

## Compatibility and deliberate boundaries

Applications with public listeners use
`start_with_services(burst_daemon::start_public_services)`; all repository startup
callers were migrated. Bare start() is core-only and rejects requested public
listeners instead of silently omitting them. The daemon library can be built with
default features disabled, so listener tests and node test helpers do not pull the
CLI's optional node-runtime dependency back into their build.

The internal block-processing result includes GapSource and describes pipeline
processing; RPC results describe payload-keyed asynchronous submission receipts.
Those receipts are not finality. Retaining separate result types and narrow callbacks
is intentional; merging their meanings to eliminate an adapter would be incorrect.
RPC/WebSocket default server features preserve their existing standalone users;
node normal dependencies use only service state/interfaces.

Context construction from a checked checkpoint anchor remains node-owned through
checkpoint_transport::context_from_anchor. Message parsing/pure bounds do not
replace authentication, replay or stateful admission. The wire namespace remains
burst.wire.v33 while the explicit protocol generation is 38. No schema tags,
signature bytes, block version or consensus constants changed in this refactor.

The catalogue has explicit data dependencies and is no longer under the former
13-component extraction path. Proof-borrowed retirement operations remain at the
checked closure, preserving encapsulation of deletion authority. Matching rsnano's
maximum path depth everywhere is not an acceptance criterion.

## Validation and reuse

Private evidence: `/tmp/burst-structure-20261030`, copied under
`~/.local/state/burst/audits/structure-20261030`.

| Latest evidence | Proven scope | Limits / invalidation |
|---|---|---|
| receipt10.json / check10.log | Final node/RPC/daemon production targets compile, 15.40s | Relevant source/manifests/toolchain changes; no runtime/release claim |
| core-receipt.json / core.log | Core node builds without listener server features, 25.99s | Normal dependency tree contains no axum; network tree contains no messages, ledger or protocol |
| callers-receipt2.json / callers2.log | Named integration_tests target compiles with migrated factory callers; cached receipt correction explicitly includes node/tests | Type check only; no full node libtest compilation or runtime journey |
| services-receipt.json / services.log | 77 tests: 2 daemon listener lifecycle, 57 RPC, 2 utility, 16 WebSocket | Includes occupied-port failure and shutdown requested before listener startup |
| test-receipt4.json / test4.log | 29 message tests passed; one stale golden failed | Preserve failure. Actual message bodies unchanged since this run, apart from schema-path comment and corrected golden fixture; see message-reuse-review.json |
| golden-receipt.json / golden.log | Previously failed hardcoded wire38 golden and unknown-field evolution passed | Only failed case rerun; do not describe test4 as a full passing run |
| schema-receipt.json / schema.log | Independent protoc36.1 agrees byte-for-byte for every existing message fixture | Schema, codec and protoc executable are declared inputs |
| schema-normal-receipt.json / schema-normal.log | Two ordinary integration regressions reject malformed/truncated headers and obsolete protocol | Separate from protoc-only selector; latest receipt in that chain |

Together, 110 focused test cases executed successfully across these records, without
repeating the 29 unaffected wire cases. Current production, service-state and latest
schema receipts were checked against declared inputs. Earlier 68/121-test batches
remain historical evidence only. Source hashes and passing unit tests do not qualify
an unidentified release binary.

No full node library test build or broad live-network acceptance was run on the
resource-limited desktop. Existing node lifecycle tests were migrated, but their
full runtime qualification remains a host/CI gate. No Azure VM was used.

## Remaining external step

Publish a reviewed coherent protocol snapshot, update both application pins to its
published SHA, and qualify clean remote checkouts. The public repositories contain
substantial pre-existing uncommitted work; publishing it is wider than the individual
file moves. Prepare isolated local review snapshots rather than committing unrelated
work into the user's current main/index. Do not label unpublished pins or local
sibling builds as passing remote CI. Publication is not a release/tag/merge approval.

## Retained failure history

Initial compile changed Cargo.lock while resolving dependencies; its receipt remains
incomplete. Earlier extraction failures (doc-comment boundary, private collection
import, restricted module visibility) were corrected and followed by passing checks.
During wire relocation, property tests first lacked their proptest dev dependency;
then the golden expected generation37 while existing code used38. The decision record
and receipt7 source hash (excluding only newly added export lines) prove38 predates
this refactor. Only two hardcoded golden bytes were corrected. The encoder was not
changed to satisfy the fixture. Failed receipts and logs are preserved.

Receipt coverage correction: the first named-integration check inherited production
inputs and omitted node/tests. Its compiler pass is retained, but a cheap cached
check now includes that directory explicitly. This reruns only the affected check,
not runtime acceptance or the passing unit tests.


## Second structural review correction

See the second-pass section of [the structural re-audit](../evidence/structure-reaudit-20261030.txt).
S2/S3 identify unfinished RPC ownership work; the earlier completion wording was
too broad. S4 is a bounded node adapter/worker organization opportunity, not a
mandate to match rsnano file counts. S1 remains external qualification: both app
pins now name local review commit562a68c103a75739c2486721e4028a214fd362f2,
but GitHub's exact commit lookup returned422 during the review. Publication and
clean remote CI have not occurred. No source or validation inputs changed in this
audit; only review documents were updated. Prior test results retain their scope.


## Third structural review additions

The third pass in evidence/structure-reaudit-20261030.txt adds S5 (shared network-test
lifecycle/cleanup helper, medium) and S6 (native-only reqwest declaration, low).
Neither is implemented by this audit. S1–S4 retain their second-pass disposition;
no existing result was rerun or broadened. Storage hydration, vendor ownership,
private proof tests and separate app composition were reviewed and retained.
The useful structural backlog is bounded; these findings do not authorize a
wholesale ledger rewrite or a new test framework.


## Implementation of second/third-pass findings — current handoff

| Item | Implementation | Qualification |
|---|---|---|
| S1 | Final snapshots and exact app pins are prepared locally | User explicitly forbids publication. Private Azure source transfer/E2E qualification replaces public CI as the next requested step. No public push occurred. |
| S2 | rpc/messages owns all existing contracts and pagination; serde works in both directions; wallet_core balance request/result use these types | 57 RPC/messages tests and real native HTTP contract regression passed; wasm browser example compiles |
| S3 | Explicit account/governance imports, direct state imports, shared pure helpers | Server-enabled RPC regressions passed |
| S4 | RPC adapter/state assembly and online-weight worker extracted to private node children, startup order retained | Final production check passed (boundary-receipt4) |
| S5 | Shared RunningNode guard cleans up after startup failure and preserves both errors; Drop requests shutdown; seven integration targets share port helper | Seven final targets type-check; three guard tests passed, including actual node RPC bind failure; not full network acceptance |
| S6 | reqwest moved to wallet_core native dependencies; existing native example retains it | Native consumer test, native example and browser compilation passed |

Only registry-independent local crate edges were added to Cargo.lock. Registry
versions/checksums match the preceding review snapshot. An initial full lock
regeneration was discarded before validation; offline metadata's uncached
cross-target dependency failure is not a passing test. Two compile failures from
missing moved imports are preserved in boundary-receipt and boundary-receipt2;
the diagnosis and successful follow-up are in boundary-impact.md and
boundary-receipt3. No behavior assertion or resource limit was relaxed.


### Final local validation

61 distinct focused tests passed:50 RPC,7 pagination,1 native HTTP contract,
3 fixture cleanup. Earlier repeated fixture cases are not counted twice. Final
production and seven integration-target checks passed, as did the native journey
example and wasm browser example. No full network/release qualification is claimed.
The structural batch alone did not qualify warnings-as-errors CI. The subsequent
A1–A5 fixes and current strict-build results are in FINAL_CODE_FIXES_20261031.md.

Latest evidence under ~/.local/state/burst/audits/structure-20261030:

| Latest receipt/artifact | Proven scope | Next action / invalidation |
|---|---|---|
| boundary-receipt4 | Final node/RPC/daemon/wallet-core production check | Reuse unless declared source/build inputs change |
| contracts-receipt |50 RPC and7 shared pagination tests | Reuse within recorded source scope |
| client-contract-receipt | Real HTTP request/response contract preserves large integer strings | Client/contract/transport inputs |
| fixture-receipt2 | Seven affected integration targets compile | Node/tests and declared dependencies; not network runtime |
| fixture-runtime-receipt2 | Drop, joined cleanup and real occupied-RPC startup failure | Helper/source inputs; broader journeys separate |
| browser-boundary-receipt | wasm browser example compiles | Browser/client inputs; reqwest still occurs transitively |
| native-example-boundary-receipt | Native journey example compiles | Native client/example inputs |
| wallet-boundary-metadata.json | Wallet workspace resolves shared contracts | Metadata only; clean native CI remains open |
| contracts-normal-dependencies.json | Resolved contract graph excludes node/RPC runtime/network/tokio/axum/reqwest | Resolve inputs, not binary-size evidence |

The wallet lockfile was refreshed without registry version/checksum changes.
Dashboard does not consume wallet_core, so it needs no lockfile change for this
contract edge. Private Azure validation must supply the exact selected protocol
source and matching local app pins; publication is not required or authorized.
Earlier local review branches remain untouched; final review snapshots supersede
them for private review. User main branches/indexes are not modified by snapshots.
No public push, CI dispatch, merge, tag or release has occurred.

VerificationStatusResponse now owns its optional reason String to support client
deserialization. JSON is unchanged; direct Rust constructors must use owned strings.
Existing RPC module paths reexport shared types. App-specific views keep their
compatibility defaults. This is not a wholesale replacement of their decoding.


## Latest user instruction and final audit fixes

Do not publish. A1–A5 from evidence/final-code-reconciliation-20261030.txt are now
implemented. FINAL_CODE_FIXES_20261031.md owns the current validation handoff:
strict production and node-test compilation and 85 focused regressions passed,
including capped Azure VM1 execution. Full end-to-end qualification remains separate. The S2–S6 receipts above
retain their original source scopes; they are not upgraded to release evidence.

The fixes preserve recovery boxing, separate graph write/replay contracts,
historical genesis bytes, test proof algorithms and submission/finality distinctions.
The cancellation convenience wrapper was found to have test callers and remains
under cfg(test). Private source bundles provision the host without publishing.
