# BURST completion plan

Requirements reconciled 2026-10-13; execution scope updated 2026-10-19.
This is the full requirements roadmap, not a queue to rerun after every change.
The active deliverable is the identified public-testnet candidate in
[TESTNET_PILOT.md](TESTNET_PILOT.md). Its current evidence and remaining work are
in [COMPLETION_LEDGER.md](COMPLETION_LEDGER.md). Dated investigations below retain
their original observations; later terminal results supersede their open-work
wording, not their failed evidence. No capability is signed off merely by appearing
in this document. Do not use a percentage or test count as a conformance claim.

## Authority and scope

1. The owner's explicit decisions govern, including equal-human consensus,
   test-only convenience issuance, restoration of expired/revoked rights,
   consumer-first wallet design and the desktop resource limits.
2. `BURST_WHITEPAPER.md` defines the thesis and intended behavior.
   `IMPLEMENTATION_DECISIONS.md` supplies selected implementation requirements.
   `DECISION_RESOLUTIONS.md` records delegated resolutions, not implementation proof.
3. On 2026-10-13 the owner explicitly chose **1 BRN/hour at initial public launch**,
   with governance able to choose zero later. Initial TRST expiry remains infinite.
   This supersedes the whitepaper roadmap's rate-zero launch wording. No premine,
   privileged accrual, or public-network faucet is authorized.
4. The earlier MVP sequencing answer deferring governance does not defer it from
   the later full-release request. Likewise, the later explicit NAT-testing request
   places self-hosting behind NAT/firewalls in scope despite decision 23.8's MVP note.
5. Later randomness phases, privacy research, social key recovery and code-level
   self-amendment retain their explicit future status. They remain tracked, not
   represented as shipped. No other difficult requirement is implicitly deferred.

### C18 local-history implementation boundary (2026-10-16)

Decision 31.4 selects the last N transactions locally, with full history on-chain.
It does not select a numerical N. Choose and document the retention default as an
implementation policy, not as a number mandated by the whitepaper.

The initial inspection found two distinct growth paths:

- `wallet_core/src/offline/storage.rs::archive_payment` retains every completed
  signed journal; `record_submission` leaves a hash-named attempt marker. Online
  signing and native offline recovery both use this storage. No retention pass
  exists. These records include signed ancestry and proposal content, so treating
  the entire directory as a disposable history cache would break recovery.
- `wallet/src/pages/history.tsx::loadMore` appends every fetched page to React
  state. The 25-entry RPC page size does not bound a long browsing session.

Implement completed-receipt retention through the shared storage path, then bound
the displayed history window without removing access to older network history.
Never delete an active draft, its attempt marker, or ancestry needed to reconcile
an uncertain action. Cleanup must not turn a confirmed operation into a reported
submission failure or permit duplicate signing after an interrupted archive.
The storage API accepts arbitrary draft paths; account-named UI paths alone are
not proof that a hash-named attempt marker has no other live draft referencing it.

Acceptance requires normal online signing and offline resolution past the chosen
retention limit, bounded completed records/markers, preserved unresolved records,
interruption/reopen recovery and multi-account isolation. The app must still
browse older history, retry unavailable pages, and retain bounded memory. Existing
110-entry Activity acceptance proves correctness of enumeration, not retention.

Implementation now stores newly completed receipts in `completed/<account>/`
and retains at most 100 per account. This is the selected implementation default,
not a protocol constant. The newest completion is retained; other receipts are
ordered by file modification time and name. The scan retains only the bounded
selection in memory. Active drafts remain in the parent directory. A submission
marker is removed only when no readable parent draft or its ancestry references
that hash; unknown records preserve the marker. Cleanup errors are reported on
stderr after resolution, without changing a confirmed action into a submission
failure. Later maintenance retries deferred markers backed by validated completed
receipts before pruning those receipts; references in active drafts or their
ancestry still protect markers. Legacy flat receipts are checked for conflicts but not automatically
classified or removed. Their migration and installed-app acceptance remain open.
Activity now replaces each page with at most 25 entries and provides Previous
and Next controls. Focused browser checks cover 110 entries in both directions,
failed-page retry without dropping the current page, and truthful block labels.
The 390-pixel screenshot shows the controls fit without horizontal clipping.
These checks mock the native RPC bridge. The first installed run exposed an
incorrect decimal-cursor assumption shared by the UI and mocks. Both now follow
the actual base64 offset contract in `rpc/src/pagination.rs`. The corrected
browser checks and rebuild passed. Installed Activity then matched the retained
archival ledger in both directions without changing account state. Installed
online and offline recovery also preserved byte-exact receipts after restart and
removed the resolved journal and marker without resubmission. The installed
over-limit run then confirmed 105 additional payments, retained exactly 100
receipts, removed resolved markers, and matched exact ledger deltas. Remaining
C18 contexts still need installed acceptance.

Legacy migration is now implemented explicitly for the app's account-named
journal layout. Startup calls it only with the profile signing lease. It accepts
the known completed-receipt filename forms, validates signed contents and the
filename hash, and publishes a no-clobber hard link before removing the old name.
This preserves bytes and modification time. Conflicting copies and unknown names
are retained. Active account-named drafts are never classified as receipts.
Startup also retries completed-directory maintenance if a crash removed the last
legacy name before cleanup. The generic arbitrary-path storage API does not
automatically infer legacy receipts. Component checks cover ordering, preservation,
conflicts and interrupted publication. The rebuilt installed Linux wallet passes
first-startup migration and a second process launch against a disposable profile
containing 100 real wallet receipts, an account-named saved draft and an unknown
file. Receipt bytes and mtimes remain exact; only the pending draft's marker
survives, and native commands still refuse to discard that draft. Evidence is in
`target/validation-evidence/wallet-legacy-migration-20261016/`. Other supported
platforms and the remaining offline/signer contexts are not signed off by this run.
Evidence is in
`target/validation-evidence/activity-pages-20261016/`. The original desktop
Chromium process-limit failure is retained alongside the successful VM check.
Component and signed-client checks are supporting
evidence in `target/validation-evidence/receipt-retention-20261016/`.

The whitepaper was read in full for this reconciliation. Every numbered decision's
selected answer and attached clarification was reviewed. Both current source hashes
match `coverage.json`. No separate requirements document was found in this checkout;
the 388 indexed source sections remain the requirements inventory. A separately
supplied requirements document must be reconciled before sign-off, not presumed
identical. The local pitch deck supports the consumer/product interpretation.
The remote website and every rendered app screen have not been freshly audited here.

## What the evidence actually says

The coverage record has 295 unverified, 85 partial and 8 verified entries. These
are section statuses, not missing-feature counts. The eight verified entries are
encoding, on-demand snapshot frequency, PoW default/uniformity, TCP, Protobuf,
Tokio and WASM support. Their historical pass records are retained; they do not
verify the full capabilities that depend on them. Some reproduction commands use
temporary paths or external hosts and need durable replacement before release.

Fresh source inspection confirms the following boundaries:

- Installed snapshots now stream through transactional byte pages. Export can
  replay an installed base and locally provable successors without the full
  account prefix. Archive v2 retains its 16 MiB header bound; larger authority
  histories use framed v3 and disk-backed certificate verification. The terminal
  metadata frame and aggregate state/boundary limits still need qualification.
  Snapshot installation now moves covered active records into a byte-preserving
  archive and retains late beacon proofs in the same transaction.
  `checkpoint_store/compact.rs` still limits active checkpoint records to 128.
  Its new acceptance path retires oldest records into the original-byte archive
  within the same transaction. A 130-checkpoint component journey passes without
  snapshots, including economic installation, archive-conflict rollback and
  reopen recovery. A live receiver also advances from 128 to 130, retires original
  records, restarts with its source and admits payments without snapshots.
  The production storage step also passes actual byte pressure, conflicting
  archives and SIGKILL around its commit boundary. Those opaque-payload tests
  verify storage atomicity, not large-ledger economic replay. Decision 11.3 selects operator-triggered snapshots, so automatic
  snapshot creation is not the remedy. `chain.rs` still contains full-prefix
  height limits for runtime replay. Historical export now traverses archived
  records incrementally and can reproduce an old epoch above height 128.
  Recovery and export retain fingerprints of active records and load one body at
  a time. Recovery successor references reject changed or missing bodies before
  replay; publication still rechecks the observations under the writer. This
  removes bulk body retention, not the count/byte limits or dense authority path.
  Snapshot reconciliation and retirement use the same borrowed scanning approach;
  retirement stages fingerprints and archives individual bodies transactionally.
  Genesis-backed startup can now replay original archived checkpoint records,
  including recovery at height 130 without a snapshot base. The active-record
  ceiling now has internal retirement with running-node catch-up and restarted
  payments verified. Delayed-beacon refresh has separate live evidence. These do not
  establish indefinite progress or bounded lifetime authority memory.
  Late proofs for retired selections can be stored separately, replayed after
  restart, published and exported without changing original certificate bytes.
  Transfer import now reuses genesis-backed recovery on installed nodes without
  snapshots, including duplicate decisions and delayed proofs. Archived refresh
  scheduling uses a one-record cursor. The live startup timeout came from the
  source treating a snapshot transition as corruption and shutting down while
  the receiver still needed its witnesses. Sync and beacon workers now share
  current-tip replay and retry explicitly stale read sets by authenticating the
  new view. Forced concurrent-change checks, live delayed proofs with pruned
  records and restart, and live snapshot replacement using peer proofs pass.
  This closes that startup race, not lifetime qualification. Installed receivers can request record-only transfers even when
  the source has never installed a snapshot. Live catch-up, restarted payments
  and old/new protobuf compatibility pass. Legacy full-history transfer
  export still has a cumulative byte limit. A genesis-only receiver exposed
  the missing prerequisite: compact import expected new suffix blocks locally.
  Existing installed-receiver tests did not prove cold-start bootstrap.
  Compact import now consumes legacy transfer bodies through the same suffix
  validator used by local replay. It validates them in memory, retrieves old
  dependencies through the proof service, and commits the replayed suffix with
  its certificate. A genesis-only component receiver installs the first epoch;
  missing, duplicate and forged suffix blocks leave storage unchanged. This
  fixes discarded legacy bodies, not lifetime transfer bounds.
  Record-only import now retrieves new suffix blocks through the existing witness
  transport. Its cursors verify the parent certificate and the manifest's head
  commitment, stop at parent account heights and enforce aggregate tail budgets.
  Suffix responses do not populate the old-proof cache. A genesis-only component
  receiver fetches its first checkpoint's blocks from a peer and installs it.
  Missing proposal content now uses a separate temporary recovery pool in the
  existing content transport. The service starts before state recovery and does
  not queue historical proposals for live admission. Content length is reserved
  against the suffix budget before retrieval. Fresh receivers now request
  record-only transfers and hand completed bytes to the same asynchronous replay
  importer as installed receivers. The runtime authority mode is unchanged.
  Live genesis-only catch-up without snapshots now passes through roster,
  selection and activation cuts, with payments before and after receiver restart.
  A second live journey adds a full-content proposal under a certified roster
  cut and verifies its archived bytes after restart. This does not prove a full
  governance voting lifecycle. Actual SIGKILL at suffix receipt, proposal-content
  receipt and checkpoint acceptance now passes snapshot-free restart, content
  preservation and payment admission. Live genesis-to-130 catch-up now passes
  without increasing its readiness window, including original archive bytes,
  restart and payments. Earlier failures exposed retry delays and missing handoff
  between certificate acceptance and economic installation. Sync now wakes the
  installer and waits for payment authority to leave the parent before another
  download. Failed-peer backoff, refresh alternation and authority checks remain.
  The importer and installer now share checked replay reuse. Every reuse verifies
  prior bodies and beacons; changed snapshot commitments invalidate the cache.
  Retirement may relocate a record only when its archived bytes match exactly.
  Provisional economics and signing journals are still recovered on each install.
  Focused replay-equivalence, relocation/corruption checks and Clippy pass. Shared
  cache integration passes fresh genesis-to-130 catch-up, both mixed-history cold
  starts, snapshot replacement and five SIGKILL publication boundaries with
  recovered payment admission. Authority arrays, observation sets,
  whole-state snapshots and header bounds remain lifetime C02/C03 work. This is
  not evidence of indefinite progress or bounded lifetime memory.
  Export authenticates from genesis or an older snapshot when needed. Peer
  refresh scheduling has bounded cursor coverage. Local witness progress no longer
  waits for peer retry ticks after each chunk; the live height-130 startup check
  caught that delay. Retirement now has count, byte-pressure and transaction-crash
  evidence. Whole-state and authority-path scaling remain separate work.
  Installed-selection publication now searches monotonically increasing cutoffs
  and verifies the installed descriptor against the matching certificate. This
  removes its cumulative suffix-byte refusal without a new index or format.
  Bounded lookup, archived publication and live delayed-proof/restart checks pass;
  initial path authentication still belongs to replay, not this lookup.
  The runtime ceilings remain genuine lifetime
  progress risks, not merely throughput optimizations.
- `fork_resolution.rs::require_supported_branch` now accepts supported identity
  and governance branches behind their existing gates, using the shared
  transition executors. Governance activation still requires its certified path.
  The installer locks global economic engines. The block-kind and overlapping
  dependency matrix remains incomplete; selected positive journeys are recorded
  under boundary B below.
- `parallel_processor.rs` provides per-account locking, but that helper is not
  evidence of concurrent economic application through the running node.
- The former pruning task in `node.rs` used fixed settings and deleted TRST index
  entries without proving recovery dependencies disposable. That task is removed;
  ordinary expiry and bounded expiry-lookup cleanup remain. There is still no
  configurable, dependency-preserving ledger compactor. The former
  four-hour unchecked timeout now follows decision 26.2's 15 minutes, including
  expiry checks when dependencies arrive between cleanup sweeps.
- Database open now checks schema and index versions before rebuilding indexes.
  Compatible unversioned stores are pinned atomically; unsupported schemas are
  refused without relabelling their contents. No schema-1 conversion or complete
  supported public upgrade path is implemented. Historical permission to discard
  development data is not permission to discard future users' ledgers.
- Recent sparse recovery tests exercise an active committee, isolated relay
  restart, observer bootstrap and payment. Delayed-beacon tests use historical
  beacon fixtures and controlled time. They do not establish production timing,
  all lifecycle combinations, or history compaction.
- The native payment fixture drives a real form and confirmed Dev ledger. Browser
  governance helpers mock IPC. Neither proves all native/platform wallet flows.
- Four feature gates are still false: checkpoint selection, meta-challenges,
  scoped ledger delegation and full governance content. They remain closed.

## Active architectural slice: lifetime authority and recovery

Priority update, 2026-10-17: daemon `a41e26c9...` now has installed governance,
recovered onboarding and fraud/appeal acceptance, four-node retained-state
restart, and independent-host combined-state snapshot recovery. Exact scopes
and failed attempts remain in `COMPLETION_LEDGER.md` and `TESTNET_PILOT.md`.
These results provide a behavioral baseline for the lifetime-history migration;
they do not complete the pilot or the full goal. Creator identity/custody still
blocks distribution of a public network. The published-key fixture cannot be
distributed, but that decision does not block C02/C03/C16 implementation.

Resume the coupled history/index migration below. Do not introduce another
history-sized in-memory offset map, serialize `CheckpointAnchor` as trusted
input, or discard old panels to reduce memory. `SelectionReceipt::restore`
requires the authenticated historical parent; retain that boundary. Scratch
integrity and quorum authority are separate checks. Missing or changed index
entries must not turn an old lifecycle into an apparent first attempt.

Batch storage, replay-index and consumer changes before the next node test
compilation. The shared fallible selection visitor and fork/staging guards pass
in `target/validation-evidence/selection-stream-contract-20261017/`. They are
preparation, not the disk-backed implementation. Completion still requires the
mixed-lifecycle longevity, recovery and resource evidence specified below.

The 2026-10-16 source-manifest comparison found no wallet application-source or
dashboard-source mismatch against the wallet migration build. Its generated
`wallet/dist` entries are stale and are not source evidence. Node recovery source
has changed since that build. VM1 currently has wallet ELF `38c004a5...` and
dashboard ELF `c045ed0f...`. Reuse unchanged GUI packages after payload checks;
build and qualify the changed daemon instead of rebuilding both applications.

Current source check, 2026-10-16: the disk-backed `AuthorityHistory` reader is
already implemented. Do not rebuild it from an older handoff. The next retained
history dependency is `RecoveredSnapshot::selections` in
`snapshot/installed/recovery.rs`. `apply_tail` appends every selection;
`fork` clones the collection; activation mutates a matching historical selection.
Replay also builds reference vectors from the collection. Replacing only the
vector with shared ownership would leave lifetime memory growth intact.

Validation-only snapshot consumers now use a shared one-selection-at-a-time
visitor. Archive import/export, installed-base reads and installation
reconciliation no longer collect a complete restored selection list just to
discard it. Runtime recovery still collects through that same visitor. Focused
restoration, reconciliation, archived committee reopen and live two-epoch
restart/payment checks pass in `target/validation-evidence/selection-stream-20261016/`.
Update, 2026-10-17: decoded receipts now stream into disk-backed indexed records.
Ordered validation and canonical encoding read one receipt at a time; fork
writes remain private. The runtime selection collection still remains resident.
The receipt batch verifies encoding parity, corruption propagation, independent
forks, installed recovery and mixed-history TCP transfer/restart. Evidence is in
`target/validation-evidence/selection-receipts-20261017/`. Long-history resource
qualification remains open.

Single-selection snapshot consumers now use `recover_selection(height)`, which
retains the requested committee while exhausting the same authenticated visitor.
Checkpoint export, archived committee publication and historical transfer checks
no longer collect all restored committees just to find one. Existing epoch,
certificate, manifest and beacon comparisons remain at those call sites. This
did not bound the decoded receipt body; the subsequent receipt migration above
addresses that buffer. The full history scan and runtime recovery collection
still remain. The bounded local node library check passes.
VM checks pass restoration without the source ledger, missing/forged historical
authority rejection for both present and absent lookup heights, compact selection,
deadline and roster agreement/recovery over TCP, and archived committee/beacon
reopen without checkpoint records. Source hashes match the worktree; evidence is
in `target/validation-evidence/selection-lookup-20261016/`. These checks do not
establish lifetime memory bounds or qualify a new release daemon.

Runtime migration also changes `confirmed_replay/selections.rs::build`, not
just `RecoveredSnapshot::selections`. Disk-staged replay now stores panels and
lifecycle, vote-source and target indexes in digest-checked private files.
The certificate source is still a resident slice. Later
receive/reward claims, challenges and deadline defaults can refer to old panels.
The replacement must preserve ordered-attempt validation and resolve those
dependencies from authenticated durable records without rebuilding a history-wide
panel map for each tail. Dropping old panels or retaining only current requests
would remove required replay behavior.

The implementation boundary includes the following concrete contracts, confirmed
by the 2026-10-16 source trace:

| Boundary | Behavior the replacement must preserve |
| --- | --- |
| `compact::read_after` | Match selections to contiguous authenticated anchors, observe every stored beacon key, and retain those observations for commit-time checks. Its `beacons` vector must not become the next history-sized buffer. |
| `SnapshotContinuation::check_selection_receipts` | Compare count, order and every frozen receipt field. A lookup-only interface cannot replace this complete scan. |
| `ConfirmedReplay::append_input_observed` | Verify exact checkpoint sequence before replay. A disk-backed source must expose fallible ordered iteration, not silently skip unreadable records. |
| `Selections::build` and `include_deadlines` | Validate consecutive attempts and duplicate contexts; resolve old vote sources, target panels and deadline defaults without dropping historical dependencies. |
| `Selections::dependencies` and `install_for` | Fetch the requested committee, its heads, votes and defaults on demand, preserving reward, challenge and historical installation checks. |
| `RecoveredSnapshot::fork` and activation | Keep appended selections and late beacon attachments private to the fork, without cloning the full history or mutating its parent's view. |

Reuse the existing authenticated `AuthorityHistory` and digest-checked scratch
rows for ordered proof material. Decoded scratch rows are not new protocol
authority. Random-access indexes must remain disk-backed as well; replacing
committee bodies with an ever-growing in-memory offset map does not meet this
boundary. Do not bridge the new storage back to existing slice APIs by collecting
the whole history before each replay. The existing mixed-lifecycle acceptance
requirements below apply to this complete change.

The recovery beacon buffer now uses the existing digest-checked `ScratchRows`
spool instead of a vector. Recovery consumes it in order and still authenticates
each attachment. The frame limit is the existing 4096-byte beacon limit plus its
32-byte checkpoint hash; there is no new history-count limit. The local library
check passes. The local test build was stopped after repeated soft-memory
throttling, without a test result. VM validation passed spool ordering at the
existing beacon limit, scratch corruption rejection, atomic beacon persistence,
pruned-record recovery, and selection/deadline/roster agreement and recovery over
TCP. The unit `burst-recovery-beacons-20261016` completed successfully. Source
hashes match; logs and the driver are in
`target/validation-evidence/recovery-beacons-20261016/`. This removes one buffer, not the retained
selection collection or replay indexes; C02/C03/C16 remain open.

The selection-history change must cover these consumers together:

- `SnapshotContinuation::check_selection_receipts` must still check completeness,
  order and every receipt field against authenticated selections.
- `attach_activation_beacon` must authenticate a late beacon for the exact old
  selection, including after retirement and restart.
- Candidate replay and vote coverage must resolve historical committees without
  trusting a decoded index entry or losing pre-activation votes.
- `installed/publication.rs::materialize` must reconstruct published committees
  and compare the complete observed verification state before publication.
- Forked recovery must isolate new selections and beacon attachments from its
  parent while preserving observations used at commit.

Use the existing authenticated authority reader and original durable records.
Acceptance needs a mixed selection/activation/revote history, late beacon
recovery, independent fork advancement, reopen and final spending. Measure the
resident selection-history cost as history grows. A passing authority-reader
test alone cannot close this work. This is part of C02/C03/C16, not a new release
requirement.

This work belongs to C02/C03/C16, not a new feature or a relaxation of release
scope. Decision 9.8 selects unlimited account-chain depth; decisions 11.2 and
11.3 select quorum-authenticated, operator-triggered snapshots. The whitepaper's
pruning claim must coexist with the owner's restoration requirements. Old signing
promises and restorable rights cannot be discarded merely because they are old.

Source inspection confirms that `HumanRoster` already shares unchanged roster
data through `Arc`, and `CheckpointAnchor::advance_certified_roster` reuses it.
Selection replay and receipt restoration now reuse that same parent allocation
when the electorate is identical. Changed delegations retain separate weights;
restoration checks the next-roster commitment. No additional cache or wire format
is involved. The remaining dense history includes changed rosters, selection
requests and receipt heads. Sharing unchanged electorates does not
complete the bounded historical-selection reader.

Compact signing now compares each retained receipt's fields directly against
the authenticated replay, without a cloned receipt or two encoded buffers.
Completeness and ordering checks remain. This removes transient comparison copies;
the full retained-selection collection still needs the historical reader below.

Record observations now stream through an anonymous temporary file beside the
ledger, with a fixed per-row decode limit and an in-memory digest checked on each
complete read. Export, cached recovery, installation and journal guards use this
same storage. Target filtering still verifies discarded rows. Cache refresh
verifies relocation and retains retired pre-base body checks and active-key
absences without building a historical key set. No durable record is deleted,
and restart rebuilds observations from durable history.

Successor references now use the same checked scratch-row implementation as
observations. Target selection checks the complete stream before retaining its
requested prefix. Replay consumes one reference at a time and reloads the exact
observed body; the cursor checks its final digest before yielding the last row.
This removes the lifetime successor vector without changing certificate or
economic replay rules. Active inventories and retained selection inputs remain
separate memory work.

| Consumer | What historical authority protects | Required replacement behavior |
| --- | --- | --- |
| `checkpoint_store/compact.rs::read_after` | Retained certificates, successor order and late selection proofs | Authenticate requested historical parents and bind each observed record to its epoch without retaining every full anchor. |
| `snapshot/installed/metadata.rs` | Monotonic handoff from the previously installed branch | Prove the previous epoch is an ancestor of the new installed epoch; do not substitute the latest accepted state. |
| `snapshot/installed/journals*` and `checkpoint_agreement/reconciliation.rs` | Round decisions, finality promises, counters, closed epochs and snapshot attestations | Preserve old electorate checks and local promises even when their authority is no longer resident. |
| `snapshot/reconciliation*` and `checkpoint_store/snapshot_path.rs` | Accepted local history during snapshot replacement | Reject omitted/conflicting accepted cuts and retain original certificate bytes. |
| `snapshot/archive.rs` and sync snapshot transport | Independent genesis-to-snapshot quorum trust | Authenticate streamed authority before installation; the snapshot must not supply its own trusted electorate. |

Implementation order:

1. Replace the dense-anchor dependency across these consumers with one bounded
   resident history reader backed by certificate/roster witnesses. Reuse
   `SnapshotAuthorityStep::authenticate` and `AuthorityCursor`. Index entries are
   lookup hints, not authority. Do not add peer deserialization for
   `CheckpointAnchor`, trust a locally decoded anchor, or replace a memory ceiling
   with a larger hardcoded history ceiling. Cold lookups must reconstruct and
   validate authority; sequential validation must avoid restarting at genesis for
   every journal row. This is an internal storage design, not a new trust model.
2. Stream the snapshot authority representation through that reader. Change the
   header format and transport together, preserving signed-manifest validation,
   truncation/conflict rejection, cancellation and crash recovery. Removing the
   16 MiB header check while retaining a whole-path allocation is not completion.
3. Define the retained dependencies for authority receipts, journals and economic
   state before physical pruning. Join C16 to this work: pending claims, later
   expiry extensions and appeal restoration remain positive requirements. Neither
   deleting old promises nor automatic snapshots is an acceptable shortcut.

Acceptance requires mixed lifecycle histories beyond the former byte boundary,
bounded resident memory, old-journal/electorate validation, snapshot replacement,
fresh-observer recovery, and restored spending after compaction. Tests must cover
positive historical lookups as well as forged/missing proofs. The current
height-130 evidence remains useful but does not satisfy these lifetime criteria.

The round-journal verifier now accepts a fallible sequential authority stream.
It checks each journal against its parent electorate, preserves unfinished rounds,
and requires locally observed decisions in the next authenticated epoch. Snapshot
reconciliation passes its existing preservation checks with this interface.
Installed recovery now supplies a certificate-file iterator. Framing v3 now uses
it for large authority headers; aggregate archive/state limits remain. Round keys
now stream from LMDB. Indexed lookups for each distinct historical representative
check their contents, and a remaining-row count rejects uncovered keys. This avoids
a lifetime journal map but still requires traversal of historical electorates.
Accepted-record reconciliation and snapshot-head staging also consume sequential
authority. They verify each retained certificate against its parent, check the
installed base on the path, and finish the stream before staging a new head.
Snapshot installation now supplies an authenticated iterator and no longer retains
the derived authority array. It checks configured genesis before traversing the
path and consumes each validation pass through the final snapshot certificate.
Archive authentication shares this iterator. This uses a fixed number of replay
passes instead of retaining historical anchors. Live snapshot replacement passes.
The serialized authority header, record fingerprints and
active-key inventories remain separate resident data, not solved by this change.
The compact recovery record reader also accepts sequential authority. It matches
historical selections in order and retains parent/child authorities only for the
bounded active inventory. Original archived successors still pass semantic replay.
Recovery no longer owns the full authority array. It stores certificates and changed
roster witnesses in an anonymous temporary file beside the database. Unchanged
rosters reuse the authenticated parent. Each reader verifies from configured genesis
to the pinned tip, with independent offsets. Frame bounds come from the largest
successfully written proof, not a lifetime height/byte ceiling. This replaces the
planned random-access index with sequential passes required by the actual consumers;
no decoded anchor is trusted. The file is rebuilt from durable history after restart
and removed by the OS when its last handle closes, including after process death.
Replay reuse and live snapshot-free catch-up pass with the new reader.
The existing genesis-to-130 live journey also passes with its original readiness
window, retained archive bytes, restart and payment admission. This is integration
evidence for the array replacement, not full lifetime-state memory qualification.
Vote-counter and snapshot-attestation validation now consumes sequential authority
as well. It resolves referenced epochs and checks each attestation against its
historical electorate without retaining those anchors. Counter, sequence and
attestation records now stream from LMDB, with indexed counter lookups and
strictly ordered authority coverage checks. The recovery change guard streams
a domain-separated, length-framed digest rather than keeping every journal key
and hash. That digest detects local mutations; it grants no consensus authority.
A 131,073-signer counter fixture passes the complete journal validator and
reopen, including counter-change detection. Recovery now also streams finality
promises and fences through shared validation; callers collecting installation
targets retain their operation budgets. Closed-epoch validation now streams its
records and resolves endpoints through a ledger-local anonymous scratch index.
Authenticated installed epochs are sorted in 4,096-row runs and merged on disk;
binary lookups replace the lifetime source map and destination set. Scratch
digests and ordering are checked before accepting the results. Closure targets
must still be later in the installed path; skipped epochs and overlapping forward
links retain their previous meaning. The index grants no authority and changes
no durable format. Metadata
ancestry uses the same certificate reader and checks its terminal tip. This removes
resident historical anchors, not all lifetime resident data: selection receipts,
observation sets, bounded installation inventories and whole-state/header formats remain.
Snapshot transfer now stages served and downloaded archives in anonymous files.
Serving uses the node database directory, not the source archive directory.
Downloads keep only the bounded header prefix until it authenticates, then release
that buffer; chunk digests and final archive validation still precede no-overwrite
publication. TCP/restart and actual download-crash journeys pass. This removes
whole-archive transfer buffering, but the v2 header, aggregate archive bound and
32-bit transfer lengths still need a coordinated format/transport replacement.

### Streamed archive migration boundary

Fresh source inspection identifies the complete migration surface below. These
are dependencies of C02, not additional scope or completed work.

| Surface | Current dependency | Required change and proof |
| --- | --- | --- |
| `snapshot/archive.rs` | v2 retains its 16 MiB header bound; v3 decodes each authority step into the shared certificate file. Checked total lengths have no fixed lifetime ceiling; body and terminal metadata bounds remain | Complete mixed-lifecycle, crash/replacement and large-state acceptance; replace remaining aggregate lifetime ceilings with bounded processing. |
| `snapshot/export.rs`, `installed/recovery.rs` export helpers | Export reuses replay's certificate file. Headers fitting v2 retain that encoding; larger headers select framed v3 | Verify large real-lifecycle named-epoch and installed-base-plus-successor exports with independent attestations, not only synthetic quorum-valid authority paths. |
| `snapshot/installed/pages.rs` | Descriptor reads and writes stream pages with checked lengths, independent of the operator admission budget | Verify large archive page replacement, rollback, digest validation and restart. Legacy descriptors remain explicitly decoded. |
| `node/snapshot_bootstrap.rs`, `snapshot_installation.rs` | Both use the same budget-limited file reader, with opened-file metadata and length-change checks | Verify chosen-budget admission and retain configured genesis, finality reconciliation, closed startup admission and runtime publication checks. |
| `checkpoint_sync/snapshot*`, `wire.proto`, protobuf model/conversion/tests | File-backed v2/v3 prefix authentication; generation 31 uses `u64` offsets/totals. Local admission uses `max_archive_bytes`; progress uses peer inactivity | Verify new-format transfer above the former total ceiling, peer failover and restart. Keep timeout, cancellation, digest/epoch/freshness binding and serving fairness. Account for staging/proof storage beyond archive input bytes. |

Implementation choices for this migration:

- Decisions 11.1, 11.2 and 26.3 require quorum-trusted snapshot catch-up followed
  by suffix replay; 11.3 makes creation operator-triggered. They do not require
  retention of an incomplete download across process death. Safe crash recovery
  and eventual completion remain required. Persistent partial-download resume is
  an optional bandwidth/startup-time optimization, not an additional protocol
  requirement. Earlier updates incorrectly described that optimization as a
  mandatory unfinished capability. In-process peer failover is already implemented.
- Reuse the certificate-file reader for imported and exported authority. Move or
  generalize that implementation instead of adding a second trusted-history cache.
  Support an independently authenticated non-genesis base where the existing
  archive API permits one; installation still requires configured-genesis ancestry.
- Keep v2 decoding explicit during migration. Do not reinterpret existing bytes
  or silently widen legacy fields and rely on older decoders to truncate safely.
  New format support must be wired through export, pages, installation and peer
  transfer before it becomes the default writer.
- Authenticate authority incrementally and finish snapshot-certificate validation
  before accepting the economic body. A peer-supplied roster, claimed height or
  archive digest cannot establish its own voting authority.
- Separate bounded individual decoding and operator storage/backpressure policy
  from lifetime protocol validity. Neither an unbounded allocation nor a larger
  aggregate constant satisfies the requirement. Handle disk-full/read errors and
  cancellation without publishing partial state or silently falling back to trust.
- Preserve the existing 64 MiB economic-body boundary as an explicitly unfinished
  whole-state requirement during the authority migration. Do not claim full C02
  completion merely because long authority now fits.

Acceptance spans v2 migration, new-format export/import beyond the old header
boundary, independently signed attestations, old journals and selection beacons,
page replacement, peer interruption/resumption, actual process death, fresh-node
recovery and restored payment admission. Corrupted, reordered, truncated and
wrong-electorate frames must fail without ledger changes. Existing native archive
and network journeys should exercise the same production path, not a parallel
test-only codec. Framing is implemented, but this full acceptance boundary remains
unfinished; current gates stay closed.
The export prerequisite is implemented: full and installed-base exports reuse
recovery's certificate file, and the separate authority-vector collector is gone.
The v2 sequence serializer preserves existing decoding and enforces its current
header bound before output. Historical beacon validation uses the same authority
source without making another complete vector. Attestation, peer transfer/restart,
sparse export and reader-corruption checks pass; strict Clippy passes. The ignored
daemon CLI journey subsequently passed against a freshly built debug daemon in
`/tmp/burst-streamed-export-daemon-cli.log`. It exercised independent signatures,
minority refusal, quorum export, offline verification and minimum-height refusal.
This closes that CLI gap, not format migration or production-artifact validation.
The import prerequisite now uses seeded v2 decoding to authenticate and store one
authority step at a time. Startup, live installation, stored-base reads and peer
archive validation use this path. Recovery takes ownership of the same file.
The legacy in-memory reader remains available; peer prefix authentication now
reads the staged file through the same seeded decoder. The aggregate v2 header
bound remains. `/tmp/burst-staged-authority-integration.log` passes
encoding, state restoration, journal preservation, sparse export, peer restart,
live replacement, five process-death boundaries with restored payment admission
and strict node Clippy. `/tmp/burst-staged-authority-daemon-cli.log` passes the real
CLI journey against a freshly rebuilt debug daemon. The versioned format and
long-transfer acceptance boundary above remain unfinished. The transfer state
machine now completes a quorum-authenticated archive of replayed accounts beyond
120 seconds on a deterministic clock. It also resumes from its retained prefix
after peer loss, pins the original digest and verifies the final bytes. Evidence
is in `/tmp/burst-snapshot-peer-resume.log`, including existing TCP/restart checks
and strict Clippy. This is
not a real-time bandwidth benchmark. Persistent partial-file resume is the optional
optimization distinguished above, not a prerequisite for safe crash recovery.

`/tmp/burst-framed-snapshot-integration.log` adds v3 export, file-backed import,
peer transfer and restored-state equality beyond the old 16 MiB header boundary.
The test uses synthetic quorum-valid authority transitions, not a claim of replayed
identity history. Count/length overflow, reordered/unsigned steps, truncated and
trailing frames, invalid final certificates and body-read separation are checked.
Existing sparse export, reconciliation, TCP/restart and startup checks also pass.
Fresh daemon CLI validation is tracked separately in the ledger. The retained
64 MiB body and terminal metadata frame remain open work. The fixed 80 MiB total
archive ceiling has been replaced by operator admission policy. Focused checks
pass for transfer/restored state above the old ceiling, rejection before staging
at a smaller chosen budget, and stored-page restart above 80 MiB. Evidence is in
`/tmp/burst-archive-budget-focused.log` and
`/tmp/burst-archive-budget-integration.log`. Memory admission initially deferred
validation; it ran only after the desktop had enough available memory.
`/tmp/burst-archive-budget-live.log` verifies exact-budget startup download,
live refusal/replacement and payment recovery with a lowered admission budget.
Economic-body staging now uses the scratch directory supplied to `read_in`.
Legacy `read` retains its process-temporary-directory behavior. This removes
implicit RAM-backed staging from the production import paths, not whole-state
resident allocations.
Mixed
ledger-derived lifecycle history and large-archive crash/replacement acceptance
remain required. No C02 completion or public-release claim follows from these
format tests.

## Prioritized capability plan

### Remaining economic-state migration within C02

#### Next implementation boundary: transition-owned recovery deltas

The October 19 source review identified the per-block bottleneck below before
the transition-owned migrations.
`events.rs::append_input_observed` calls its observer after every authenticated
transition. `installed/changes.rs::derive` then called `Rows::capture` over the
entire account, pending, BRN, mint-index and lifecycle projection and charged
the full encoded size against the 256 MiB work budget. Disk-backed rows remove
resident copies but leave work proportional to state size times tail length.
The fixed work budget can therefore reject a tail of individually valid small
operations as the network grows. This is a source-derived availability risk,
not a newly measured large-network failure. Do not raise the budget to hide it.

Current source records account, frontier, pending-claim and lifecycle changes
through transition adapters. Their full projection is now a test-only oracle;
final publication still stages the complete projection. TRST deltas also identify
holders whose normalized caches change without an account write. The cache and
unrelated-account regressions pass. Installed-recovery qualification remains
incomplete. The expanded rejection/returned-receipt cases now pass in both
storage formats, as do the affected publication and download crash cases
(`recovery-fixture-contracts-20261019` on VM1). The later BRN batch described
below removes its per-block wallet scan. Mint index recording is now connected
to the authenticated burn path and existing mutation journal. Eleven current
node tests pass, including independent encoding, full-projection equivalence,
historical-mint omission, payment journals and crash recovery. Evidence is under
`/home/burst/mint-recovery-20261019` on VM1. It preserves origin keys, expiry saturation and reverse
keys, with full projection retained for final publication and the test oracle.
Global engine serialization still needs transition-owned deltas.
Do not redo the completed adapter wiring or treat this partial migration as
completion of the whole-state requirement.

Verification storage is a remaining scale boundary. Under the October 19 public
testnet goal, further migration is deferred unless release-impact evidence
establishes it as a prerequisite. See `TESTNET_PILOT.md` release priorities.
Both ordinary endorsement
publication and recovery encode `verification_orchestrator` as one value.
`OrchestratorSnapshot::rewind` decodes three complete values and rejects values
above 64 MiB. Omitting the value from recovery would lose required rollback;
caching its serialization would retain the durable size ceiling.

The record implementation in `verification/src/orchestrator/records.rs` groups
each target's state, challenge, appeal, selection, prior votes and signed attempt
bindings together. Each verifier's rolling Neither history and penalty share
another record. Governed Neither rules have one separate record. The existing
snapshot encoding remains unchanged. The record builder reuses appeal and
rolling-history validation; it does not grant authentication or publication
authority. This format alone does not complete the storage migration.

Remaining integration, in order:

1. Track record changes at mutation ownership, including timeout votes, revotes,
   penalties and governed window changes. Preserve first-before/final-after and
   the existing target-level conflict checks.
2. Publish records through the caller's LMDB transaction. Convert the legacy
   snapshot and provisional undo together. Startup must reject partial or mixed
   schemas; do not retain a dual-written aggregate copy.
3. Use the same records for replay-derived journals and certified installation.
   Rule rollback must retain its dependent-state guard. Final publication must
   not require encoding all records into one bounded byte string.
4. Stream the records through authenticated snapshot export/import. Keep legacy
   readers and deterministic commitments; format changes require an explicit
   version, not silently different bytes under an existing version.

Evidence must cover exact legacy-state equivalence with certified selections,
appeals and attempt contexts; unchanged unrelated rows; disjoint/overlapping
rollback; aborted migration and crash/reopen; installed recovery and an observer.
Record codec tests do not qualify any of these durable integration steps.

BRN mutation ownership is a recovery optimization boundary, not missing
burn/stake functionality. Live candidate publication already calls
`ledger_updater::persist_brn_delta` from `economic_candidate/staging.rs`; it writes
changed wallets but discovers them by scanning both maps. Recovery separately
previously encoded every BRN wallet in `Rows::capture_projection`, including when
`include_recorded` was false. The local mutation batch now omits those rows from
partial capture and connects recorded changes. The current node-test batch passes
full-projection equivalence, identity arrival order, both payment journal formats,
publication crash and download crash recovery. This does not qualify a daemon
release or close the other whole-state limits.
These are different paths; do not build another
live persistence mechanism or describe all BRN integration as absent.

The prior per-block BRN budget defect can be stated without a benchmark: if the unchanged
BRN projection alone occupies P encoded bytes, replaying N blocks charges at
least (N + 2) * P bytes (initial observation, each block, final publication),
before other tables and changed rows. The work limit is 256 MiB. Thus a 1 MiB
unchanged BRN projection alone exhausts it beyond 254 blocks. This is an
illustrative bound from `derive` and `capture_projection`, not a measured
network size or a new economic limit. The fix must remove repeated unchanged
wallet encoding; raising the budget or changing BRN accrual does not fix it.
The connected mutation batch removes that repeated BRN-wallet charge. Final
full-state projection bounds are separate and remain unresolved.

`BrnEngine` exposed
its raw wallet map publicly; `ledger_bridge` removes and reinserts wallets for burns
and stakes, and edits balances directly for compensation. Instrumenting only
`track_wallet` or `get_wallet_mut` would miss those writes. The local `WalletStates`
batch centralizes mutable map access, preserves read-only access and connects
before/after rows to the existing recovery journal. Its BRN crate checks and
eight focused node integration cases pass; see `brn-recovery-20261019` on VM1.
Preserve failed-operation
behavior, burn/mint compensation, stake sequencing and accrual intervals. The
existing full-projection oracle must compare these effects by table/key. A
standalone tracker without that replay connection does not complete this item.

Implement mutation capture at the existing transition adapters, not by guessing
affected wallets from block fields. `verification_commit::ledger::ReplayedLedger`
and `governance_commit::ledger::derived::DerivedLedger` already centralize
account/metadata writes. Payment replay also removes claims, creates returns on
rejection and adds outgoing pending entries directly in `ConfirmedReplay::apply`.
All these writes must participate. Endorsement effects and engine changes must
remain part of the same authenticated block boundary.

1. Capture first-before and final-after values for each changed table/key through
   those shared writes. Repeated writes collapse; insert-then-remove leaves no
   delta. Capture only replay-derived state, never local undo or afterimages.
2. Connect the recorder to `replay_applied_ordered` and consume it in `derive`.
   Reuse the existing authenticated scratch sequence, deterministic journal sort,
   TRST/graph record delta visitors, finality reconciliation and atomic installer.
   Do not add an independent replay implementation or another storage format.
3. Keep final complete projection staging separate from per-block changes.
   Expiry-normalized caches must use one recovery clock; signed balances, pending
   returns and lifecycle deadlines retain their existing clocks and authority.
4. Remove per-block whole-projection capture only after every mutation owner is
   covered. Keep the existing full-diff path as a test oracle during migration,
   then remove it from production rather than retaining two runtime strategies.

Evidence required: exact delta equality with the full-diff oracle for payment,
reject/return, merge, endorsement, verification and governance histories; repeated
key writes and deletion; expiry extension and revocation; equal state after
rewind and reopen; live publication crash recovery. Add an unrelated-account
growth case proving unchanged rows are not encoded per block. Finally qualify
large mixed histories and interruption recovery, rather than substituting the
existing open-account-only resource probe. This work blocks C02/C03 initial
release as well as scalability. Replay-owned maps and snapshot-size limits remain
separate obligations and are not closed by a journal optimization.

Until this connected change is ready, do not repeat the completed copy-reduction
tests or rebuild applications. No new ambiguity in protocol intent was found.

**Next priority: large-boundary recovery and lifetime qualification.** The original
real-account probe passed export/transfer/import at 4,000 and 8,000 opens, but a
16,000-open checkpoint was accepted and could not be exported. After removing
the test fixture's 32 MiB database-map constraint, the failure was
`snapshot tail exceeds block budget` in `SnapshotTailPlan::read`, reached through
`RecoveredSnapshot::recover_genesis_locally`. This is a C02 correctness/availability
gap, not proof of database corruption or a memory-limit failure.
Replace whole-boundary tail accumulation with bounded staged dependency replay.
Authenticate the complete boundary and preserve cross-account dependencies,
proposal bodies and economic ordering; publish only the final certified state.
Reuse this path for local export and peer catch-up. Do not just raise 8,192 or
fall back to unchecked state. Completion evidence must include the 16,000-open
positive journey, mixed dependent payments/lifecycle work and interruption recovery.
Logs: `/tmp/burst-snapshot-resource-{4000,8000}.log` and
`/tmp/burst-snapshot-resource-expanded-16000.log`. The latter is a failing test;
no larger-state or under-1-GB qualification follows from those measurements.
After staged replay, installation and retained-proof migration, compact paths now
share full-history's 131,072-block / 64 MiB boundary budgets. The expanded
16,000-account probe passes export, the transfer state machine, authenticated
installation, database reopen and indexed startup checks in
`/tmp/burst-aligned-boundary-16000-checked.log`. Its process high-water mark is
300,856 KiB. The same journey at 32,000 accounts passes in
`/tmp/burst-aligned-boundary-32000.log` with a 546,992 KiB high-water mark.
These measurements include fixtures and archives, not a production workload.
Large live-network bootstrap pacing, mixed lifecycle boundaries, interruption
recovery at this size and budget-edge combinations remain unqualified.

Payment replay and its verification/governance ledger adapters now read applied
blocks through a shared fallible interface. It supports borrowed or owned blocks
without making engine execution depend on references into a complete history map.
Compact replay now uses an anonymous applied-block file beside the ledger, for
local, transferred and indexed tails. Each indexed body has a bounded length and
digest; reads check the bytes, failed appends poison the candidate, and dropping
the candidate releases the scratch file. Full-history replay retains its in-memory
reader. The staged hash index still grows with the boundary. The shared causal
ordering engine now orders hashes and loads dependencies through a fallible callback,
preserving the existing sorted-root depth-first order. Its compatibility adapter
still indexes in-memory blocks. Epoch replay now calls hash ordering directly.
Selection and governance preparation scan fallible input readers; governance keeps
dependency metadata instead of bodies. Snapshot continuation stages witnesses and
new bodies directly, without cloning them into a combined vector. Deadline,
challenge-outcome and governance activation use a read-only view of that same
staged input after replay validates the complete boundary.
Tail planning now stages signed bodies beside the ledger and orders only hashes.
Finished tails expose fallible body iteration. Confirmed cuts, membership coverage,
local ordering and governance startup consume that reader. Peer suffix collection
also stages account batches before planning; legacy attached transfers are staged
after decoding. Finished historical proofs also use pinned files and preserve
their authenticated context and height. Discovery and recovery share that storage
across cuts. Witness requests and proposal bodies still retain collections.
Signing coverage shares the checked tail and
streams its archive rows; it no longer builds a second serialized body vector.
Hash indexes and ordering metadata still grow. Read errors
must abort the candidate, and only already applied authenticated blocks may be
exposed to economic lookup. Only internal boundary budgets changed; wire batches,
desktop resource limits and release gates did not.

Suffix transport now requests at most 128 blocks at a time from a cloned
authenticated cursor. It checks the returned positions and context before
advancing the caller's cursor, then stages the batch before requesting more.
Request failure cannot partially advance that cursor. This separates individual
network requests from account-chain length; aggregate boundary limits remain.

The remaining collection owners must be handled together before claiming the
large-boundary capability complete:

Checkpoint replacement no longer copies whole old projection tables merely to
clear them. Whole owned tables use transactional clearing; owned metadata
namespaces use 256-key deletion batches in the same writer. A 131,073-row
lifecycle namespace now clears atomically, with abort/reopen and unrelated-key
preservation checked. This is replacement of replay-owned projections, not
physical ledger pruning. `compact.rs` now streams both old height indexes and
retains cemented bodies/indexes in place. It scans physical block keys in batches
of 256 instead of collecting all old hashes and retained positions. Supplied
witnesses and provisional retirement retain their operation budgets; cemented
history no longer consumes those budgets. Storage replacement passes with
131,073 retained bodies totaling more than 64 MiB, including abort and reopen.
That fixture uses opaque storage bodies, not a large signed protocol lifecycle.
Whole-state account projections, witness budgets and dependency-preserving
physical compaction remain C02/C16 work.

Compact snapshot and successor installation now borrow the replay state's account
map. They no longer clone all account records into a vector and build another
storage lookup map. Storage checks map-key/account identity and keeps the same
head-witness, finality and atomic-write rules as the slice API. Successor
publication consumes the original account map for runtime frontiers. This removes
redundant copies; the replay engine's whole-state map and operation limits remain.

| Owner | Current boundary | Required completion evidence |
| --- | --- | --- |
| `SnapshotTailPlan::read` and transfer `suffix::fetch` | Disk-backed suffixes use the full-history 131,072-block / 64 MiB budgets. | Large live-network catch-up, mixed dependencies and budget-edge combinations. |
| Historical witness service | Requests retain up to 131,072 bodies / 64 MiB before staging. Committed roots are looked up in one receipt pass and fetched in groups of up to 64 exact heads. Each response respects the existing 1 MiB raw-body budget and serving limits. | Large live-network pacing, multi-boundary recovery and aggregate budget qualification. Small live bootstrap/replacement and partial exact-head continuation pass; these do not establish large-network performance. |
| `tail/cemented.rs::plan_indexed` | Positions, lengths and digests use a shared 131,072-row / 64 MiB budget for old heads and new suffixes. | Large reopened-ledger payment admission and head-plus-suffix budget edges. |
| Snapshot reconciliation and projection | Rows are staged and consumed once in the uncommitted writer. Input proof collections remain bounded at 131,072 / 64 MiB. | Large interrupted installation preserves atomic old/new state. |
| Compact signing coverage | Coverage shares staged suffix and historical-proof files and streams archive rows. Proposal content remains in memory. | Exact promises, changed-byte rejection, atomic persistence and lifecycle recovery at the larger boundary. |

These are existing C02/C03/C16 work, not new product requirements. Raising only
the export limit would leave installation and startup unable to use its result.

The staged-transfer crash journey has mixed evidence: the first run timed out
before content receipt; a diagnostic rerun passed all three interruption points
and restarted payments. Same-IP handshake throttling delayed the second run by
about a minute. Keep the failed result and qualify consecutive-crash reconnect
scheduling under C02/C15. Do not raise resource limits or treat the later pass as
proof that recovery timing is reliable.

The handshake limiter now bounds tracked IP windows as well as pending cookies.
Consuming a challenge no longer permits an unbounded accounting table. At the
tracking cap, unknown IPs wait for a completed rate window; existing IPs remain
subject to their unchanged per-minute limit. Cookie expiry and rate windows use
monotonic elapsed time. This is C15 resource protection, not a higher handshake
rate or a localhost exemption. Consecutive-crash timing still needs evidence;
the earlier timeout is not resolved by this accounting change.

**C15 mutual peer authentication.** The endpoint-impersonation regression in
`/tmp/burst-mutual-handshake-before.log` reproduced outbound registration of an
unproved listener identity. Wire generation 33 now requires both peers to sign
both identities, fresh challenges and peer contexts with separate roles before
registration. This implements decision 23.17's node-ID authentication without
adding encryption or consensus authority. The obsolete one-way read-loop branch
is removed. `/tmp/burst-mutual-handshake-integration.log` passes transcript
binding, inbound legacy/altered-identity/cross-network/replay rejection followed
by valid recovery traffic, outbound missing-proof rejection, stalled-frame retry,
registry deduplication, wire checks, live sparse-source bootstrap, both live
replacements and strict node Clippy. Full real-router/NAT and hostile network
qualification remain C15 work; these results do not sign off the capability.
`/tmp/burst-mutual-handshake-final.log` also passes forged/changed listener proofs,
the three-stage SIGKILL/content/payment recovery journey, independent schema
conformance, cookie-manager tests and final strict node Clippy.

The current wire-33 binaries also pass the isolated Linux NAT suite on the Azure
validation host. `target/validation-evidence/nat-20261014/attempt2` contains the
terminal journal, binary hashes and signed-transaction reports. The failed first
attempt is retained beside it. Tests now distinguish early recovery peering from
application RPC readiness and stop dependent scenarios after topology failure.
Outbound-only and double NAT, same/different local ports, firewall reconnection,
manual forwarding and UPnP lifecycle/discovery pass. These are isolated namespace
and IGD-emulator results with one Dev representative, not physical-router or
distributed-quorum qualification. Those boundaries remain in C15's acceptance
scope; no release gate changes.

The current source still has coupled memory dependencies. `snapshot_state()` now
borrows replay maps, rates, parameters, historical selection receipts and
lifecycle/proposal payloads. Format-2/body-6 exports borrow TRST wallets and
merger graphs and emit versioned storage records; legacy format-1/body-5 imports
remain supported. Canonical preflight checks the field writer against a bounded
sink. Record visitors still allocate sorted reference indexes, and imported
state remains owned.
State encoding and import comparison now normalize one top-level field at a time,
with the field list generated from the serde schema. Top-level maps now sort
encoded keys while borrowing values, then normalize one entry at a time. Top-level
sequences normalize one item at a time. Maps above 4,096 entries sort temporary
runs with bounded raw-key batches. Small maps retain an in-memory ordering index.
Nested individual records still build trees. Disk-backed export carries its ledger
staging directory through map sorting and archive revalidation. Staged imports
reuse their selected directory during canonical comparisons. An inaccessible
chosen directory fails without fallback. Byte-backed APIs and manifest-only
calculations still use the OS temporary directory, which may be memory-backed.
A complete scratch storage budget and whole-process memory bound remain open.
Operator exports and installed-base persistence now prepare canonical state in
an anonymous file beside the ledger. Signing and archive writing use the same
manifest and format as the byte-backed API, with body authentication before output.
Installation streams that file into pages under the existing projection transaction;
it does not commit or publish independently. The byte-backed API still allocates
a complete encoded body. Checkpoint
continuation hashes canonical output into a manifest without retaining a vector,
both at genesis and after accepted boundaries. Import decodes a complete
`State`; BRN takes its decoded map/rates through shared validation without another
encode/decode copy. New-format TRST import feeds individual records into validating
builders; legacy imports still decode aggregate blobs. Merger canonical validation and wallet undo checks compare
streamed bytes rather than retaining another encoded body. Removing only the manifest's
64 MiB check would expose the remaining allocations without bounded recovery.

Replace these copies together with ordered record iteration, streamed canonical
output and bounded staged restoration into the existing transaction/publication
path. Preserve canonical bytes where possible; any changed signed encoding needs
an explicit version and migration checks. Required positive evidence includes
payments, pending receipts, BRN stake/rate state, TRST provenance, verification
and appeals, governance, and old selection receipts across export, import and
restart. Measure resident memory on growing real state, not just archive length.
Authentication, noncanonical-input rejection, disk failure and pre-publication
crash recovery must remain enforced. This is unfinished C02/C03 work, not a new
optional feature or permission to discard dormant restoration rights.

Account afterimage and runtime validation now share a transaction-bound iterator.
Frontier validation uses direct primary/index comparisons and an exact count,
without retaining the account inventory or frontier map. The storage check passes
131,073 accounts, malformed/index-corrupt cases and uncommitted-writer behavior.
Live snapshot replacements, sparse-source observer bootstrap and strict Clippy
pass in `/tmp/burst-streamed-account-integration.log`. Projection/undo and signing
coverage now use the same iterator and direct account lookups under the writer.
`/tmp/burst-streamed-account-projection.log` passes storage rollback/projection,
large-inventory undo, live replacement and strict Clippy. The corrected signing
filter in `/tmp/burst-streamed-account-signing.log` exercises actual selection
and finality promises; the earlier zero-test filter is not evidence.
Retained-history and suffix change detection now keep a streamed account count
and hash, not full serialized inventories. Both compare against the exclusive
writer before publication. This local fingerprint is not a consensus commitment.
Finality reconciliation and suffix planning now stream validated rows too. The
planner counts matching base identities using unique validated LMDB keys; a
same-count substitution test rejects loss of a required identity.
`/tmp/burst-account-inventory-recovery.log` and
`/tmp/burst-streamed-suffix-planning-final.log` pass finality/change detection,
live replacement/bootstrap and strict Clippy. Pending afterimage validation now
borrows one stored row at a time and compares it with the replay-owned claim.
Exact primary/sender-index counts detect missing or extra entries without copying
either inventory. `/tmp/burst-streamed-pending-recovery.log` passes large-inventory,
claim mutation, payment recovery and live replacement tests; the signature-only
lint correction passes in `/tmp/burst-streamed-pending-clippy.log`. This preserves
decision 9.7's pending rights, including return-credit state. Individual row bounds
remain, and the replay-owned pending map still occupies memory. Head/body/proof
inventories retain their count/byte bounds. Economic afterimage/runtime comparisons
now compare the persisted BRN/TRST fields directly without whole-state encoded
buffers. Equality tests cover the seven merger-graph fields and wallet snapshot
fields; derived expiry caches still normalize to the same clock before comparison.
Restoration borrows encoded TRST state from the exclusive writer and streams BRN
rows through shared validation, retaining the original inventory limits. Evidence
is in `/tmp/burst-economic-state-comparison.log` and
`/tmp/burst-borrowed-economic-restoration.log`, including payment recovery, live
replacement/restart and strict Clippy. Decoded economic engines, the normalized
TRST comparison copy and runtime frontiers remain separate allocations. The
trait-based BRN startup loader now reads metadata and borrowed wallet rows from
one immutable snapshot instead of separate transactions and an encoded inventory.
`/tmp/burst-brn-startup-snapshot.log` checks consistent reads across a commit and
malformed rows. `/tmp/burst-brn-startup-integration.log` passes startup/recovery,
live replacement/restart and strict Clippy. The decoded BRN wallet map still grows
with the inventory. No growing-state memory measurement or full longevity sign-off
follows from these checks.
The shared restore path also passes all six fork-replacement journeys in
`/tmp/burst-borrowed-economic-forks.log` and five actual SIGKILL publication stages
in `/tmp/burst-borrowed-economic-crash.log`. Reopened nodes preserve signing
promises and admit payments after each tested crash stage.
Retained-history planning now scans borrowed block rows and pins exact bodies in
the existing integrity-checked scratch file. Publication streams the same indexed
rows against the saved metadata observations. Local signatures and ancestry still
require authentication; scratch hashes do not establish consensus authority.
`/tmp/burst-staged-retained-storage.log` checks sparse/index-corrupt inventories,
transaction views and original limits. `/tmp/burst-staged-retained-integration.log`
passes staged corruption, local/peer-backed retained ancestry, forged signatures,
changed inventories, live replacement/restart and strict Clippy. The retained
metadata and scratch index still grow with row count, so their existing budgets
remain. This removes body copies without claiming bounded total recovery memory.
Continue replacing those coupled consumers
rather than raising limits or calling this full recovery qualification.

Lifecycle engine-cache comparison now traverses borrowed replay-owned metadata
under the existing writer and compares it directly with replay state. It no
longer collects a second inventory or imposes the old aggregate row/byte cap.
Exact counts and values reject missing, extra and changed entries; the separately
validated orchestrator and local signer journals remain outside that traversal.
The existing storage replacement test passes 131,073 rows, callback failure,
abort and reopen. VM1 evidence at
`/home/burst/lifecycle-streamed-recovery-20261018/` passes the large comparison
regression and existing authenticated engine-cache recovery integration.
This does not qualify large signed lifecycle history or remove the replay-owned
map. Replayed undo records now use an ordered sequence over the existing
authenticated scratch records/index. Finality reconciliation, format conversion
and installation consume these records individually, with source errors aborting
the uncommitted writer. The existing storage rollback test passes late-read
failure and unchanged complete-inventory checks. VM1 evidence under
`/home/burst/undo-streamed-recovery-20261018/` passes scratch integrity, finality,
both payment formats, merge-journal rewind and live publication crashes.
Arrival-order recovery passes separately under
`/home/burst/undo-arrival-ready-20261018/` after correcting an obsolete aggregate
storage-key assertion, also reproduced with the pre-change executable. All
three arrival orders, lifecycle equality, confirmation heights and rewind remain
checked. The first failed batch is retained, not relabeled as passing.
`installed/changes.rs::Rows::capture` now stages before/after projection rows in
the existing authenticated scratch records. Delta comparison and final cache
publication visit those records without retaining complete row maps. Final
publication writes the replayed verified count after accounts, whose writes
can otherwise change the count. VM1 evidence under
`/home/burst/projection-streamed-recovery-20261018/` passes row integrity,
count ordering, limits, finality, both payment formats, merge and arrival-order
rollback, and live publication crash recovery. The runtime receipt verifies
with fingerprint `9025aa937fbea950121a6ca6030f7e99a6e87f02941b1084d006b3fb336ab328`.
This removes duplicate projection maps, not whole-state recovery work.
Replay-owned maps, normalized TRST copies, whole-state scans per applied block,
per-record allocations, aggregate work limits and the storage duplicate-hash
inventory remain. Review their coupled ownership before another patch; large
mixed-history and interrupted-installation acceptance remain required.
Expiry normalization now preserves shared unchanged portfolios rather than
detaching every portfolio. The focused TRST and full crate checks pass, including
equivalence with the old algorithm and token revival. The combined node batch
passes nine normalization/payment/graph/rollback cases. Both live publication
cases initially aborted with Tokio worker stack overflow under the unoptimized
profile. A debugger located the oversized metadata-recovery poll frame; heap-pinning
that shared future fixed both cases under unchanged limits. Verified evidence is
`/home/burst/token-stack-fix-20261019/receipt.json`, fingerprint
`a7d46bf21b46a0726a8119ad60cb7652011fbbb7911fcda38aeae148c23c786a`.
This is functional integration, not release-artifact qualification.
Changed portfolios can still be copied and normalization still
scans all holdings. This does not close whole-state recovery or lifetime limits.
Certified token publication also now uses storage's bounded key deletion batches
instead of collecting old holding keys and graph ordering maps. Ordinary writes
retain their existing ordering/undo path. The scoped storage test passes abort,
reopen and namespace/undo protection. Nine node passes and the original failed
live cases remain in `/home/burst/token-recovery-batch-20261019/` on VM1; the
subsequent two-case pass above completes this scoped batch, not C02/C03 overall.

Classes: **R** blocks the requested safe initial public release; **S** is long-term
scalability; **P** is optional polish beyond required usability; **F** is explicitly
future/out of initial-release scope. R+S means it is unsafe to ship a known lifetime
ceiling and call it a future optimization. A row's acceptance evidence is a minimum;
its mapped source sections remain binding in full.

The payment-retirement acceptance journey now carries pending burns across
checkpoints 32/33, 64/65, 96/97 and 128/129. Fresh and height-128 live receivers
recover through height 130, compare BRN and TRST rights at one expiry clock,
restart and admit another payment. Evidence is in
`/tmp/burst-payment-rollover-live-checked.log`, including strict consensus/node
Clippy. The earlier timeout and unequal-clock comparison are retained in
`/tmp/burst-payment-rollover-live.log`. A bounded checkpoint-signature success
cache removes repeated strict verification for identical signing inputs without
caching membership, quorum or authority. Its mutation/eviction checks pass in
`/tmp/burst-checkpoint-signature-cache-checked.log`. The readiness deadline and
desktop limits are unchanged. These in-process node fixtures supply source
finality directly; they do not establish independent-process latency, distributed
quorum formation, unlimited history or mixed verification/governance longevity.
The cold-start journey now runs its first receiver in an independent process.
`/tmp/burst-isolated-retirement.log` fails the original 90-second readiness
deadline despite a connected peer. Progress diagnostics show no accepted records.
The test setup serialized `NodeConfig` without the fixture protocol parameters,
which that type intentionally skips. The receiver therefore used defaults.
The corrected subprocess handoff carries parameters separately with a round-trip
regression check. `/tmp/burst-isolated-retirement-corrected.log` passes that check,
independent-process catch-up in 87.4 seconds, reopened economic state comparison,
archive retention, payment admission/restart and strict node Clippy. The failed
setup is not evidence of a production scheduling bug. One pass close to the
unchanged 90-second deadline does not qualify timing margin, mixed lifecycle
longevity or the remaining state limits. C02 remains incomplete. All four release
gates remain closed.

| ID / priority / class | Requirement and intended behavior | Current implementation and concrete gap | Evidence required for completion |
| --- | --- | --- | --- |
| C01 / first / R | One authoritative requirement/evidence ledger. Preserve the full thesis and explicit decisions. | Source inventory exists, but long iteration logs mix historical progress and current gaps. This plan consolidates them; most behavior is not signed off. | Every indexed section assigned below; every release claim linked to inspected implementation, positive acceptance journey, failure/recovery checks and reproducible revision-bound evidence. No unmapped additions. |
| C02 / first / R+S | Account chains continue indefinitely; quorum-authenticated snapshots, offline catch-up and recovery work without a lifetime replay ceiling. | Automatic active-record retirement, genesis-backed recovery and live catch-up cross height 128 without operator snapshots. Authority history has a file-backed representation. Full mixed-lifecycle longevity, whole decoded state, total snapshot-size and remaining transfer budgets are not qualified. Historical passes do not qualify current release artifacts. | Independent nodes cross multiple compaction boundaries and the former 128-record limit with real selection, governance and payments; bounded resident memory; restart mid-transfer/install; fresh observer from sparse peers; conflicting/truncated proofs rejected without state loss. |
| C03 / first / R | One deterministic state transition and atomic durable publication for payments, lifecycle and governance. Same history gives the same state after replay, fork repair, restart and snapshot install. | Shared replay exists, but live publication/cache reconstruction, full/compact installation and rollback have separate reconciliation paths. Whole-state snapshots and duplicated projections complicate reasoning. | Differential comparison of full replay, incremental replay, running admission and recovered state for the same signed histories; crash at each commit/publication boundary; no admitted state before durable authority; no invented repair of corrupted economics. |
| C04 / next / R | Equal verified-human representative voting resolves all eligible forks; no token weight, genesis bonus, timer unlock or cemented rollback. Revocation/delegation changes authority only through agreed transitions. | Human roster/epoch journals and gated identity/governance fork replacement exist. Signed-network endorsement, proposal, delegation and ballot journeys pass; the full block-kind and overlapping dependency matrix remains incomplete. Independent adversarial handoff evidence is incomplete. | Signed competing branches for every admitted block kind, including overlapping receives/merges, endorsements, votes and delegations; independent-signer partitions/restarts; honest-quorum progress and minority non-finality; old promises and cemented dependencies preserved. |
| C05 / next / R | BRN is computed equal income, never transferable or expiring. Rate segments, concurrent stakes, unspent funds and returned credits remain spendable exactly once. Launch rate is 1/hour. | BRN engine, pause/resume and signed credit mechanisms exist. Positive integration coverage is distributed; constants/activation and recovery need one acceptance matrix. | Wallet-to-ledger accrual and spending across multiple rate changes including zero, concurrent stakes, timeout/appeal settlement, identity pauses and restart; exact raw arithmetic, overflow failure and no extra minting. |
| C06 / next / R+S | Burn 1:1 into TRST; accept/reject, partial transfers, self-send, 256-input merges, provenance, proportional fraud revocation, reputation and later restoration all work. Expiry zero and infinity are valid. | Economic engines and many journeys exist. Pruning/restoration and cross-lifecycle/fork interactions are not complete. Expired quantity must not disappear or be treated as worthless by protocol fiat. | End-to-end matrix covering merge-of-merges, harsh rounding, pending expiry return, expiry extension/re-expiry, origin/holder revocation and re-verification, appeal correction, spendable clean remainder, all-node restart and fresh observer. Repeat after compaction. |
| C07 / next / R | Humanity onboarding: permanent endorsement burns, eligible random committees, three vote choices, nonvoter Neither, revotes, returned/forfeited stakes and correct rewards. No endorser subsidy. Bootstrap exits at 50 humans. | Certified lifecycle and bootstrap code exist but production selection is gated. Controlled-clock tests do not prove a usable public verification flow or all coincident events. | Non-genesis users complete the entire wallet onboarding journey, including exhausted/inconclusive rounds, paused endorsements and resumed eligibility; spend resulting funds; bootstrap exit removes creator privilege; restart/observer equivalence. |
| C08 / next / R | Challenges, fraud versus inactivity, meta-challenges and re-verification preserve the correct rights and economic corrections. Challenge itself does not suspend an innocent target. | Certified settlement and repeated-appeal fixtures exist; meta-challenge gate is closed. Native journeys and overlapping rule/deadline/fork cases remain incompletely verified. | Successful and unsuccessful fraud/inactivity challenges and repeated reversals through signed app flows; correct origin-only invalidation/restoration, no duplicate bounty, stake availability and notifications; partition, crash, restart and observer recovery. |
| C09 / first with C02-C03 / R | Phase-one drand fixes request, pool and future round before disclosure; authenticated clock bounds; relay failure waits for the exact certified round; expired unagreed proposals recover without erasing locks. | Pinned BLS chain, clock/service and durable recovery exist. Historical proof fixtures and honest partitions do not establish adversarial scheduling resistance or production availability. | Production-clock/relay run plus deterministic scheduler tests for delayed prepare certificates, disclosure-before-signing, clock loss, restart and partitions; both safety and eventual progress under stated quorum/timing assumptions; document residual selection-bias bounds. |
| C10 / after state boundary / R | Four-phase numeric governance, all economic parameters, concurrent proposals, emergency participation/approval, reversible changes and constitutional self-threshold protection. | Catalog maps 41 stored fields; mixed replay and native journeys exist. Other constants, conflicting activations, ballots across revocation and production-size replay remain open. | Every active parameter exercised from proposal/support through counting and activation, with old/new rule boundaries and restart; concurrent/withdrawn/emergency cases; full electorate accounting. Representation/resource constants classified explicitly, never silently hardcoded economic policy. |
| C11 / after C10 / R | UTF-8 50,000-character constitutional diffs, versioned history, independent amendments and readable outcomes. | Content transport, conflict outcomes and native fixtures exist behind full-content gate. All supported-platform and concurrent/recovery evidence remains incomplete. | Create/review/vote/activate large multilingual amendments through the app; deterministic conflicting patches and self-threshold changes; recover complete content and version history on restart and a new node. |
| C12 / after C03-C04 / R | Owner-revocable encrypted secondary-key delegation, proposal/parameter scopes and transitive delegation; no spending authority transferred. | Scoped-key engine, wallet flows and ledger operations exist behind a gate. Current three-human partitioned counting verifies transitive/scoped grants, ordered revocations, owner precedence, API records, restart and fresh-observer recovery. Elected ballot replacement, opposite-arrival revocation and post-cut old-epoch revocation with successful retry survive certified counting and recovery. The remaining deadline/fault matrix and native interactions are not qualified. | Owner and delegates on separate nodes/devices vote in every scope, redelegate and revoke during active ballots; stale/cyclic/over-scoped keys cannot act; direct owner rights, recovery and UI explanations remain correct. |
| C13 / alongside integration / R | Cryptography, addresses, block formats, uniform precomputable PoW, message bounds and signed network/epoch separation. | Implemented; a few exact format choices have scoped historical sign-off. Current release-revision vectors, interoperability and fuzz evidence need consolidation. | Independent vectors for derivation/address/signature/work, maximum valid merge/release/content messages, Protobuf compatibility and malformed input; signed offline submission; no test keys/tools accepted on Live. |
| C14 / after C03 / R+S | Parallel work for independent accounts with correct cross-account dependencies; sub-second ordinary confirmation, 100+ TPS and under-1-GB node target on Pi-4-class hardware. | Bounded authentication workers and copy-on-write preparation exist; globally locked economic state remains. No current evidence proves the selected hardware/throughput targets. | Instrument actual admission/preparation/write stages; independent account progress with dependency conflicts serialized; sustained meaningful load and recovery on provisioned hardware, including memory/disk growth. A single atomic LMDB writer is acceptable; serialized economic preparation is not proof of decision 25.7. |
| C15 / alongside C02 / R | Authenticated TCP/Protobuf networking, discovery, bounded peers/spam, clock handling, reconnect and useful self-hosting behind NAT/firewalls. | Current daemon `2afb7a79...` passes isolated NAT/firewall/UPnP scenarios on VM1; receipt is indexed in the completion ledger. Physical-router, distributed-quorum NAT and broader hostile-network qualification remain open. | Independent nodes exchange blocks/votes behind outbound-only NAT, manual forwarding, UPnP unavailable and reconnect; bounded abuse and invalid peers; explicit diagnostics when no route exists. Do not promise universal firewall bypass. |
| C16 / after C02-C06 / R+S | Configurable pruning, recoverable history, origin/expiry indexes and no loss of restoration, contribution history, pending claims or finality evidence. | Fixed index-cleanup task is not ledger compaction. Active receipts/authority and historical bodies accumulate; expired/revoked tokens can later regain rights. | Operator policy selects pruning; atomic checkpoint advancement defines a dependency-closed retained set; archive and sparse modes recover it; expiry extension and appeal restoration after pruning pass; disk growth measured; no live dependency or signer evidence deleted. |
| C17 / alongside app integration / R | Local-safe RPC with unsafe-method control, batch/pagination; filtered WebSocket events and RPC recovery after missed events; explorer data and useful operational errors. | APIs exist. Whole API contract, pagination under mutation, actual notification paths and reconnect behavior have no capability-wide sign-off. | Wallet and node app complete positive operations against a real ledger; pagination neither invents nor loses entries under documented consistency; filters and reconnect reconcile balances; remote/unsafe/test-only controls tested through server entry points. |
| C18 / after C05-C12 / R | Consumer wallet: mnemonic/keystore, backup, one-human identity, controlled signer access, offline signing, payments, trust-based auto-receive, configurable auto-merge and limited local history. | Installed Linux evidence covers offline burn/open/receive, disconnected signing, lost responses, restart, confirmed forks, sparse canonical-index recovery and certified epoch exclusion. Bounded Activity matches real ledger history; 105 additional installed payments retain 100 receipts with exact ledger deltas. Legacy migration/reopen preserves receipt bytes, timestamps and unresolved drafts. See the local-history section for artifact-specific evidence. Current v7 development evidence additionally covers device grants, two-host native takeover/restart, stale-device refusal, concurrent handoffs resolving to one grant, offline receipt and losing-ancestor recovery. Affected certified contexts, pruning/restoration review and final changed-package qualification remain open; old installed evidence does not qualify v7. Other platform qualification remains open. | Create/restore/lock/unlock/pay/receive/reject and offline-sign-submit from real apps; competing devices cannot accidentally sign concurrently under the documented coordination model; lost connection/stale frontier/epoch gives recoverable UX; no seed in logs. Every protocol lifecycle above usable without test-only commands. |
| C19 / after C18 / R, extra scoring P | Off-chain origin-based group trust, live queries, explicit setup policy, configurable outage behavior and optional group-to-group trust. Groups never change protocol votes or balances. | Any/all/graph checks, directory exporter, bounded HTTP(S) discovery and explicit wallet selection/save are implemented. Historical installed-directory acceptance targets older artifacts; its cited local folder lacks terminal logs. Do not rebuild this functionality as missing. | Recover and inspect historical terminal evidence, then qualify the final wallet/daemon artifacts using the existing groups driver: publish/discover/configure, merged-origin review and auto-receive, outages versus negative/malformed replies, cyclic graph bounds and manual override. Public hosting/IPFS deployment is not established by loopback fixtures. Additional weighted scoring is an optional design extension, not a release blocker unless explicitly selected. |
| C20 / after functional flows / R usability, P decoration | Minimal consumer wallet and more technical node UI; clear label/value hierarchy, consistent dividers, compact spacing, meaningful icons, small contextual explanations, accessible settings. | Design systems and tests exist; screenshots in conversation identify repeated layout failures. No fresh all-screen visual sign-off. | Render every route in light/dark and narrow/wide layouts, keyboard/touch/reduced motion, empty/loading/error/disconnected states; real transaction review and confirmation. Use relevant Emil/design skills then. Branding experiments or extra animation cannot displace functional work. |
| C21 / final integration / R | Durable storage, shutdown, corruption recovery, versioned upgrades, operational CLI/config, safe installers and reproducible signed/platform release artifacts. | Tooling and platform evidence exist; migration steps and format refusals do not establish public upgrades. Resource-limited local builds are not target-device qualification. | Backup/upgrade/rollback on supported versions without silent data loss; killed-process and storage-fault boundaries; Linux/macOS/Windows/ARM app and daemon lifecycle, installer integrity, configs and documented resync. Verify exact artifacts/revision before opening publication. |
| C22 / before public-network claims / R research | Economic simulation, incentive/security analysis and bounded-community testing support the thesis honestly. No claim of perfect Sybil prevention or established equilibrium. | No economic-equilibrium/formal-incentive proof was found in the inspected paths; lifecycle simulation is a protocol fixture, not economic evidence. | Reproducible growth/contraction/expiry/rate/Sybil/collusion scenarios with stated assumptions; adversarial review and any impossibility/counterexample reported; bounded-community observations before claiming public economic readiness. Do not manufacture a proof from unit tests. |
| C23 / explicit later scope / F | Phase-two commit/reveal, phase-three threshold randomness, privacy, key-loss recovery, full code self-amendment, multisig/escrow/timelocks/interop as selected future work. | Partial primitives or no implementation; no replacement for phase-one requirements. | Separate specifications and security reviews before enabling. Do not silently implement unsafe fallback randomness or privileged recovery. These do not block initial release solely by being future features. |

## Source assignment: nothing disappears between capabilities

All 291 numbered decisions are assigned by chapter below; cross-cutting rows may
apply too. This is routing, not sign-off. The individual selected answers remain
the acceptance checklist, including positive functionality and numeric defaults.

| Decision chapters | Capability owners |
| --- | --- |
| 1 | C01, C21; the full-release request supersedes early MVP sequencing |
| 2 | C14 |
| 3 | C21, C22; MIT and general-purpose protocol, no invented compliance mechanism |
| 4 | C13, C18 |
| 5 | C05, C17 |
| 6-7 | C06, C08, C16, C20 |
| 8-9 | C02, C06, C13; account balance and two-phase receipt remain mandatory |
| 10-11 | C02, C04, C16, C21; on-demand snapshot choice preserved |
| 12-13 | C04; C03 resolution supersedes token/local-online quorum wording |
| 14 | C13, C14 |
| 15-18 | C07-C09, C17-C18 |
| 19 | C09, C23 |
| 20 | C10, C12 |
| 21 | C11 |
| 22 | C19 |
| 23 | C15, C13 |
| 24 | C02, C16, C21 |
| 25 | C14, C17, C21 |
| 26 | C02, C04, C13, C21 |
| 27 | C07, C13, C21 |
| 28-30 | C17; a standalone explorer and GraphQL are not selected initial requirements |
| 31-32 | C18-C20, C23 |
| 33 | C05-C08, C19, C22; economic transparency and no node/voter subsidy preserved |
| 34 | C13-C15, C18, C21 |
| 35 | C04, C21-C23; no post-bootstrap creator override |
| 36 | C05, C08, C23 |
| 37 | C05-C15, C21; explicit answers/resolutions override contradictory summary defaults |

Whitepaper routing covers every W entry: W-001..014 C01/C22;
W-015..032 C05/C06/C16/C18; W-033..040 C07/C09/C22;
W-041..043 C19; W-044..050 C07/C08/C22; W-051 C22;
W-052..055 C04/C13/C14/C15; W-056..057 C09/C23;
W-058..064 C10/C11/C12; W-065 C23; W-066..068 C17/C18/C20/C21;
W-069..077 C05/C06/C22; W-078 C16; W-079..083 C01/C22;
W-084..089 C01/C22; W-090..091 C23; W-092..096 C02..C21;
W-097 C01/C21/C22/C23. Narrative comparisons are claims to qualify, not new
currency mechanisms or evidence that the claims are true.

## Architectural decisions and implementation sequence

These are engineering choices under the owner's delegated authority, not additions
to the monetary rules. Revisit them if implementation evidence contradicts them.

### Boundary A: durable authenticated state (C02, C03, C09, C16)

Build one state/authority boundary used by startup, live installation and sync.
Persist authority transitions as bounded authenticated pages/records, with an
explicit committed head, rather than one complete archive blob or dense in-memory
anchor vector. Keep the configured genesis as the trust root. Page boundaries
must not turn a peer's claimed roster into trusted authority. Authenticate each
transition under its predecessor; economic snapshots retain quorum commitments.

Use the existing `AuthorityCursor`, snapshot certificate verification, replay
executor, witness cursors and coordinated LMDB writer. Replace the storage and
callers together. Do not add another standalone proof helper and call this done.
An archive protocol revision must stream authority pages and state with explicit
bounds, cancellation and restart; compatibility and format changes must be explicit.
Total historical verification work can grow; bounded working memory does not mean
constant-time verification or that all authority history may be forgotten.
Decision 11.3 keeps snapshot creation on demand. Internal replay-state maintenance
must not require operators to create snapshots merely to keep the network moving.
Do not turn automatic snapshot creation into a release requirement.

Define a retained dependency set from the committed economic state, open lifecycle
receipts, provisional tail, pending transfers and finality journals. Preserve
restorable provenance and reputation. Old physical bodies can become optional only
after replacement state/proofs are durable and the configured archive/recovery
policy can supply required history. Never prune by age or token status alone.

Acceptance boundary: a long-lived network crosses multiple state-base replacements,
performs positive lifecycle/governance/payment operations, loses its archival peer,
restarts and bootstraps an observer. Inject interruption around page persistence,
base replacement and admission publication. Compare all economic/lifecycle rights,
not just checkpoint hashes. This boundary must close the current lifetime ceilings.

The first implementation batch must change these consumers together:

| Existing boundary | Required replacement / reuse |
| --- | --- |
| `consensus/src/snapshot/authority.rs` | Reuse predecessor-quorum authentication; do not introduce a second trust algorithm. |
| `snapshot/archive.rs`, `snapshot/installed.rs` | Separate bounded economic-state framing from paged authority persistence/transfer; an installed head references committed data, not a peer-trusted anchor. |
| `snapshot/installed/recovery.rs`, `checkpoint_store/compact.rs` | Replace dense `anchors[height]` and complete-prefix assumptions with authenticated indexed/page reads and a bounded successor stream. |
| `checkpoint_store/snapshot_path.rs`, `snapshot/reconciliation.rs` | Reconcile accepted cuts and local finality through the same authority view, under the existing exclusive commit boundary. |
| `checkpoint_store/archive_selection.rs`, snapshot receipt recovery | Retrieve the actual authenticated parent for historical selection inputs; retain exact beacon commitments and delayed-refresh behavior. |
| `node/snapshot_bootstrap.rs`, `snapshot_installation.rs`, checkpoint sync | Share the new install/recovery route; interrupted staging cannot publish a head or enable admission. |

Before deleting the old storage path, exercise fresh bootstrap, existing-base live
replacement, offline restart, missing-history peer recovery and delayed beacon
publication against this same batch. Keep old-format handling explicit. Do not
claim the boundary complete after only changing the archive encoder or cursor.

### Boundary B: complete deterministic transitions and conflicts (C03-C08, C10-C12)

Payment-fork planning now streams the account inventory. Publication updates only
the dependency closure's account heads instead of rebuilding the whole runtime
frontier. Large empty-account inventory and live replacement/restart evidence is
in `/tmp/burst-scoped-fork-publication.log`. This removes the whole-account-list
ceiling and copy; economic/undo limits and lifecycle branch support remain open.

Inventory live versus replay executors and eliminate divergent business rules.
Ordinary lifecycle admission and non-compact startup now share strict restoration
of the verification snapshot from the writer. Fresh genesis persists it atomically;
missing established state requires authenticated recovery. Genesis and replay
also use the same configured verification rules. This removes the live-cache
source for admission, not the remaining lifecycle fork/publication work.
Live challenge recovery exposed arrival-order-sensitive vote snapshots.
Unordered vote/endorsement evidence now serializes in a deterministic order;
selected committee ranking and the chronological Neither window retain theirs.
Applied recovery also needs to reproduce the local settlement trigger, which
depends on vote arrival order. Its existing local identity-order reconstruction
now includes checked transition dependencies. These only order signed replay;
full economic and lifecycle comparisons remain mandatory. The controlled-time
live re-verification and pending-TRST restoration journey now passes, including
new payments and recovery on restarted nodes and a fresh observer. Real-clock,
adversarial scheduling and lifecycle-fork qualification remain open.
Existing snapshot commitments and upgrade paths still need compatibility
qualification before release, even though the sequence wire shape is unchanged.
Use typed transition inputs and dependency/read/write sets; prepare from a stable
view, recheck dependencies in the single writer, commit effects plus authority,
then publish runtime state. Use those same effects for dependency-closed fork
replacement. Preserve final promises and cemented positions. Rejected unsupported
forks do not satisfy this boundary. Avoid introducing another global coordinator
for ordinary independent payments; checkpoints coordinate shared rule changes.

The next fork boundary is concrete. Behind the existing certified-verification
gate, `fork_resolution::evaluate/install` now shares endorsement, challenge and
vote effects with ordinary admission and publishes the restored verification
engine after commit. The public gated-off path still permits only payment
branches. The gated path now restores governance, delegation and constitution
from the rollback writer and runs compact governance operations through the
ordinary `CandidateGovernance` executor. It publishes all three engines after
commit under admission's lock order, including when a payment replaces governance.
Full-content proposals now use an immutable content record committed with the
candidate block. Admission, restart and installation authenticate the signed
manifest and apply the content through ordinary governance staging. The chunk
service can serve cached candidates before ledger installation. The existing
256-candidate bound also bounds content records, each at most 512 KiB; there is no
silent eviction. Finalization removes the candidate/content pair, not admitted
proposal content. Secondary-key payloads now use the same executor behind the
existing key-delegation gate. Ordinary fork activation remains excluded and
retains its certified path. `rollback_plan::SnapshotUndo` reconciles
disjoint TRST and merger entries. It now also reconciles verification snapshots
with coupled target and verifier-history conflict scopes. Signed-endorsement
replacement preserves independent progress and BRN burns, including abort and
reopen checks. Governance-engine snapshots now reconcile disjoint proposals with
coupled vote/timing/secondary-ballot scopes. Signed proposal replacement preserves
another proposal through abort and reopen checks. Constitution snapshots now
reconcile amendment/vote/conflict scopes; persisted delegation graphs reconcile
each owner's global/scoped entries together. Global document/depth changes require
the complete dependent after-state. Signed legacy delegation and revocation
replacement preserve other owners through abort and reopen, with writer-restored
governance state and the bootstrap constitution. The delegation worker now covers
both replacement directions and restart followed by a signed delegate/revoke
operation. A two-node signed-TCP journey also passes both delegation replacement
directions, forged-vote rejection and signed follow-up operations after each node
restarts (`/tmp/burst-governance-fork-wire-recovery.log`). It uses the genesis
one-human roster and does not qualify multi-human partitions.
Compact proposal/vote/support fork journeys, multi-human full-content and secondary-key
fork recovery, and overlapping dependency closure remain unqualified. Endorsement worker tests cover both directions,
dependent account removal, runtime publication and restart with a subsequent
signed operation. They inject election authorization and do not prove signed
network quorum for lifecycle forks.
The separate signed-TCP endorsement test now covers both replacement directions,
forged-vote rejection, restart and a subsequent signed operation on both nodes
without injecting election outcomes. It uses the genesis one-human roster.
The three-human connected-network journey now observes sub-quorum votes without
replacement, then installs both endorsement fork directions at quorum. Governance
activation, restarted sources and a fresh observer preserve the winning
endorsement exactly once. Partition recovery, signed challenge/vote branch
replacement and overlapping lifecycle dependencies remain unqualified.
Ordinary admission and payment replacement now share `economic_candidate::stage`
for writer-bound predecessor/balance checks, pending reads and economic ledger
writes. The former ordinary path no longer supplies a pre-writer balance read.
Complete governance coverage through the shared execution/publication path.
The encrypted parameter-key worker now covers grant replacement in both
directions, restart, owner revocation and rejection of a stale grant generation
(`/tmp/burst-encrypted-delegation-fork-recovery.log`). It injects election
authorization. Qualify proposal scopes, recipient/index views, dependent ballots,
concurrent revocations and signed-network recovery before enabling the key gate.
The parameter-grant signed-TCP journey now also passes both replacement
directions, forged-vote rejection, restart on each node, subsequent owner
revocation/new grant and matching incoming/outgoing index records
(`/tmp/burst-encrypted-key-network.log`). It uses the genesis one-human roster.
It does not qualify dependent ballots, proposal scopes, revocation forks,
multi-human partitions or app-level index views.
The signed writer fixture now verifies grant-to-ballot dependency closure in
both selected key scopes, with and without prior revocation. A historical ballot
remains admissible after revocation but contributes no delegated vote at the
count cut. Grant replacement removes the ballot and revocation descendant while
preserving the proposal, including abort and database reopen
(`/tmp/burst-key-ballot-revocation-fork.log`). Carry these exact positive semantics
into live fork publication and signed-network recovery; do not reject historical
ballots merely because revocation arrived first on another account chain.
The running worker now verifies live dependency removal and restart for both key
scopes, using signed bootstrap onboarding and signed governance operations. After
grant replacement removes the ballot and restores the recipient head, restart
accepts a fresh grant/ballot pair (`/tmp/burst-key-ballot-worker.log`). Election
authorization remains injected and governance timing is shortened. The next
acceptance boundary is signed-network installation/recovery with dependent
ballots, including revocation ordering, multi-human quorum and crash boundaries.
The two-node signed-network variant now passes for both selected key scopes
(`/tmp/burst-key-ballot-network.log`). Prerequisites and the winning branch are
confirmed through signed TCP votes, not injected election results. Both nodes
publish dependency removal; each restart accepts a fresh grant/ballot pair.
The installed roster is still the genesis one-human roster. Remaining acceptance
work includes direct ballot forks, concurrent revocation ordering, multi-human
quorum/partitions and crash-boundary recovery.
Direct ballot replacement now passes over signed TCP votes for both scopes,
with the grant confirmed before election. The elected Nay choice survives both
nodes' restarts and losing-ballot resubmission. Subsequent owner revocation
suppresses its delegated weight in the recovered count-cut projection without
erasing the historical ballot (`/tmp/burst-direct-ballot-network.log`). Complete
certified counting under concurrent revocation, multi-human partitions and crash
boundaries remains required.
The existing three-human partitioned secondary-ballot/counting journey has now
been inspected and verified on this worktree
(`/tmp/burst-current-partitioned-key-count.log`, 146.14 seconds). It covers
quorum-dependent ballot/count progress, transitive parameter grants, proposal
scope revocation before/after ballot admission, owner vote precedence, exact
certified totals, activation, restarted sources, a fresh observer and delegation
API records. Do not rebuild or label those selected cases missing. Extend that
journey with concurrent revocation/fault schedules; its ordered revocations and
controlled clock do not qualify those combinations. The non-partitioned variant
now replaces a provisional secondary Nay ballot with an elected Yea ballot using
three signed human votes. One and two received votes cannot finalize. Exact
certified totals, activation, restarted sources and fresh-observer recovery pass
(`/tmp/burst-certified-ballot-fork.log`, 127.66 seconds). The signed-quorum helper
is shared with the endorsement-fork journey. Both pass after consolidation
(`/tmp/burst-certified-forks-shared.log`, 123.24 and 66.10 seconds), followed by
strict node/test Clippy (`/tmp/burst-certified-forks-shared-lint.log`). These
controlled-clock development checks do not open the public gates.
The partitioned variant also passes opposite-arrival ballot/revocation delivery
(`/tmp/burst-concurrent-key-revocation.log`, 139.67 seconds). Two nodes receive
the ballot first; the isolated owner receives its proposal-scope revocation
first. Neither operation finalizes in the partition. Both confirm after healing,
and certified counting excludes the revoked delegated weight. Exact outcomes
and scoped authority survive all-source restart and fresh-observer recovery.
This qualifies that concurrent arrival case, not races at the counting deadline
or crashes between durable commit and publication. Historical ballot admission
must remain independent of revocation arrival order.
The same partitioned journey also verifies a parameter-scope revocation issued
after the pending counting cut but before its certificate installs
(`/tmp/burst-count-boundary-key-revocation.log`, 141.60 seconds). The old-epoch
operation is provisionally admitted without finality, removed when the certified
count installs, then successfully re-signed and confirmed under the new epoch.
It cannot rewrite the earlier tally. Activation and restarted/fresh-observer
records remain correct. This is one selected boundary schedule, not qualification
of every deadline race or crash between durable commit and runtime publication.
The actual Linux wallet now passes the scoped-delegation, parameter-authoring
and constitution-authoring journeys on a bounded Azure host. Each includes
certified counting and activation, source restarts and fresh-observer recovery.
Evidence is under `target/validation-evidence/native-20261014`. Startup now
honors the accelerated fixture clock in test builds; production timing and
cutoff rejection are unchanged. These selected app journeys do not qualify
the remaining fault schedules, full parameter catalog or supported platforms.
The full-content worker now tests both directions using a 50,000-character
constitutional amendment, restarts after candidate admission without another
content offer, and verifies the winning proposal's exact content. Election
authorization is injected, so this is not distributed-quorum evidence.
The combined signed-TCP full-proposal journey now passes both directions, peer
content fetch, forged-vote rejection, exact restart recovery and subsequent
withdrawal/new proposal admission (`/tmp/burst-full-proposal-network.log`). Only
the submitting node receives the content directly. This uses two nodes with the
genesis one-human roster; qualify multi-human partitions and crash boundaries
before release. A signed block alone does not supply its content. Keep that requirement and secondary-key recovery
inside C03/C04/C10-C12; neither is optional polish.
Qualify verification fork replacement through signed network elections, including
challenge/vote branches, before treating the gated path as release-ready.
Prove that unrelated verification/governance progress survives replacement and
that cemented blocks and final-signing promises cannot be removed. Do not remove
the current refusal before those positive replacement and recovery paths exist.

Run positive capability journeys as a matrix: normal and deadline verification,
challenge/inactivity/appeal/re-verification, governance and delegation, concurrent
rules, pending returns and restoration. Finish each through signing/UI, economic
effects, restart and observer reconstruction. Scheduler and storage faults are
systematic axes of those journeys, not an endless list of unrelated tests.

C02/C10 live governance catch-up must cross the 60-second ordinary admission
window. The legacy RPC/P2P test starts its observer near that boundary and has
both passed and failed with `proposal not found`. Its nodes do not produce
certified checkpoints while the release gate is closed. Complete the live
certified proposal/count/activation path, transfer its authenticated history to
an observer joining after the live window, and verify the same proposal state,
parameters and economic effects after restart. Keep rejection of uncertified
backdated blocks. Do not shorten the history, increase timestamp tolerance,
trust a bootstrap peer label or treat a near-real-time pass as this evidence.
The existing three-human certified journey now advances its controlled clock to
61 seconds after proposal creation before restarting sources and adding the
observer. `/tmp/burst-certified-governance-aged-recovery.log` verifies certificate
installation, proposal/ballot state, governed parameters and the fresh payment
tail. Real-clock qualification is not covered by this pass. The subsequent
final-vote recovery path covers a current-epoch tail older than the admission
window in `/tmp/burst-historical-final-votes-batched.log`. It requests signed
final votes in the existing confirmation loop, requires the installed human
quorum, and requeues only the exact authorized hash through ordinary validation.
Its bounded ephemeral queue is not a durable finality journal and cannot override
epoch changes. A live one-source/observer phase now proves minority non-admission
before the other representatives return. The diagnostic run passes subsequent
quorum recovery; an earlier timeout remains unexplained. Full admission queues
now leave the handoff retryable without counting duplicate votes twice, with a
focused pressure test. Initial frontier discovery stays pending if no peer was
ready at the first tick, avoiding the observed 30-second polling delay. The
combined live journey and strict lint pass in
`/tmp/burst-historical-tail-initial-discovery.log` under unchanged deadlines.
Graceful interruption of partial proof collection now passes in
`/tmp/burst-historical-tail-interrupted-proof.log`: the observer reopens its
database without inherited vote authority, reacquires minority proof without
admitting the payment, then confirms after quorum returns. Sustained large
tails, abrupt-crash/disk-fault interruption, hostile partition scheduling and
resource qualification remain acceptance work.

### Boundary C: usable applications and measured operations (C13-C21)

Connect the completed protocol paths to wallet/node APIs and real app flows.
Refactor account-scoped economic preparation only after read/write dependencies
are explicit; retain atomic cross-account commits. Benchmark meaningful workloads
on an appropriate host, never by bypassing desktop caps. Batch UI corrections by
shared component/layout pattern, then inspect all affected routes and states.

Complete native packaging, migration/recovery and network topology checks at an
integration boundary. A mocked IPC browser test is a UI contract test, not proof
of a signed ledger action. Fuzz and broad regression runs follow meaningful changes
to codecs, storage or transition boundaries, not every local edit.

### Boundary D: release audit (C01, C22 and all R rows)

Reproduce evidence against the release revision and exact artifacts. Store commands,
environment, source/artifact hashes, assertions and logs under durable evidence
paths; `/tmp` notes are discovery aids, not release attestations. Have a complete
positive-flow and recovery matrix, measured targets and qualified security/economic
claims. Only then reconcile source gates and publication checks. A gate test proving
that functionality is rejected cannot prove that functionality is implemented.

## Ambiguities and corrections to old queues

- Resolved now: initial rate is 1 BRN/hour, not zero. Rate-zero operation remains required.
- Preserve C01-C10 and recorded activation/appeal/credit choices unless contradicted
  by a later owner decision. They are design resolutions, not successful tests.
- Decision 31.3 expresses concern about multiple identities rather than selecting
  unrestricted independently verified accounts. Do not add that behavior. Single
  identity is the current consumer target; any future linked-account model needs
  a separate decision. Cross-device use of that same identity remains required.
- Decision 31.7 requires a single active signer. A local mutex does not coordinate
  separate devices. Design that coordination and state its cloned-key limits; do
  not claim cryptographic prevention of a key owner signing outside the app.
  Initial source review on 2026-10-19 confirmed that `ProfileAccess` locks a file in one
  local profile. Its process/crash tests cover that file, not separate devices.
  `wallet_core/src/offline.rs` signs using the account key without contacting a
  node; decision 31.9 explicitly selects fully offline signing. State block
  version 6 had no distinct signing-device authority or handoff operation.
  Existing payment epochs and fork rejection must not be relabeled as device
  coordination: they do not tell an offline signer that another device took over.
  The current v7 implementation adds explicit confirmed grants and local binding.
  The completion ledger now records positive current-grant operation, independent
  native profiles on two physical hosts, node/app restart recovery, stale-draft
  refusal and competing handoffs resolving to one canonical owner. These close
  the development coordination assertions, not final package qualification or
  cryptographic prevention of malicious root-key use.

  This question was already raised on October 15, not discovered on October 19.
  After the owner identified the repeated question and directed continuation,
  the implementation direction stated on October 19 is explicit,
  network-confirmed takeover with a warning about unsent transactions, while
  retaining offline signing on the designated device. This is the assistant's
  stated interpretation of that direction, not a quotation of a new questionnaire
  answer. Do not re-ask the same question or mark the capability complete from
  this choice. Network authority and recovery still require implementation.
  A disconnected old device could still produce signature bytes; the guarantee
  would concern which device's transactions the network accepts after takeover.
  Refusing takeover without the old device preserves exclusive offline ownership
  but prevents recovery when that device is lost, even with the mnemonic.
  Do not add a central lease service, disable offline signing, or treat a local
  RPC lease as network-wide authority to avoid this decision.

  Once resolved, the coherent capability needs device enrollment/read-only use,
  explicit handoff, restored-device takeover, pending-action reconciliation and
  restart-safe authority. Its acceptance must exercise separate profiles on
  separate devices, offline old-device behavior and concurrent takeover attempts.
  Reuse unrelated lifecycle and networking evidence; determine affected protocol
  checks from the chosen implementation before scheduling another build.

  Implementation boundary for network-confirmed handoff:
  - Preserve the root public key as account identity and the mnemonic derivation.
    Device control must not create a second verified identity or move balances.
  - Use an explicit account-chain signer-change operation, ordered by existing
    consensus. A node-local lease or an unconfirmed RPC response is not takeover.
    Bind subsequent signed actions to the confirmed grant so an old signed draft
    cannot be reused after a handoff. Do not repurpose representative selection
    or governance delegation fields as signing authority.
  - Keep the device's local binding separate from the mnemonic/keystore. Restoring
    root material must not silently copy the active device's local role. Read-only
    access uses the public address and needs no private key.
  - Offline contexts retain their grant. Never refresh a grant inside an already
    signed action or automatically re-sign it after rejection. A disconnected
    device can still construct bytes; stale authority prevents their acceptance.
  - An accepted payment before takeover remains accepted. Competing takeover and
    payment branches follow existing finality; takeover cannot undo cemented
    history. Concurrent takeovers must not create two active grants.
  - Persist and reconstruct authority through the same account transitions used
    by live admission, fork replacement, replay and authenticated snapshots.
    Include grant dependencies in retained history/pruning. Bump affected formats
    and reject incompatible state explicitly; serde defaults are not migrations
    for existing Bincode rows.
  - Stage the local pending takeover before submission and activate the local
    signer only after confirmed network state agrees. Lost responses, app restart
    and node restart must reconcile the original action before another signature.
  - The root-key holder can use a modified client to reclaim control. State this
    limit; this requirement coordinates consumer devices, not ownership of a
    deliberately copied recovery secret.

  Batch the protocol transition, wire/storage compatibility, client/offline
  binding, takeover UI and recovery assertions before creating a replacement
  release candidate. The new view-only source flow is not proof of this boundary.
- A 90% threshold is not 6/7. Explicit percentage and exact integer arithmetic win
  over the questionnaire's incorrect parenthetical example; retain the threshold.
- Fixed consensus intersection and wire/representation limits need explicit
  classification separate from democratically adjustable economic rules. Do not
  call an arbitrary economic constant a safety limit to avoid governance.
- Weighted group scoring, a standalone browser wallet, light-node mode, a separate
  explorer and extra animation are not automatically required by the existing
  choices. WASM support, native apps, group interoperability, APIs and requested
  minimal UI are required. Record any newly selected expansion explicitly.

## Execution and ledger rules

### Next storage boundary: C02, C03, C14 and C16

The 2026-10-15 source review confirms a shared blocker behind these four rows.
The original wallet writer encoded all portfolios, origin ownership and outcome
invalidations into one `trst_engine_wallets` value. Startup now migrates that
value and provisional journals to versioned rows. Normal admission and expiry
now encode only changed records and check their stored before-values. Detached
account/origin maps still require key scans, but unchanged portfolios are not
encoded and the ordinary path no longer builds a whole-state old-key set. The
merger graph now migrates to rows as well, with provisional journals converted
in the same graph migration transaction. Candidate admission and expiry now
emit only changed graph rows. Detached maps still need comparisons, and new
reverse edges scan the extended parent's rows for its highest ordering slot.
Full replacement still scans all rows. This is not account-scoped preparation.

Certified recovery now retains shared graph and holdings views and derives
record-level journal changes directly. It no longer serializes those complete
economic states after each replayed block. Recovery stages both row formats and
their journals atomically, including when replacing a legacy-format database.
Signed merge installation/rollback and live bootstrap/restart checks pass;
remaining replay maps, final full-state replacement and aggregate export still
require work. The completion ledger records the evidence and its scope.

Snapshot manifest format 2/body version 6 now streams holdings and graph records
through the existing validating builders. Format 1/body version 5 remains
readable. The signed format must match the body version; authority still comes
from the authenticated checkpoint chain. This removes aggregate economic byte
strings, not the complete snapshot-size or memory ceiling. Keep the 64 MiB
ceiling until remaining field encoding/decoding, canonical map preflights and
transfer budgets support larger certified state together. Record envelopes
can increase wire size, so small-fixture passes do not establish capacity.

The legacy/export read/write contract is still asymmetric. `TrstEngine::save_wallets` has no
encoded size check, while `load_wallets` rejects more than 64 MiB. The block undo
journal and payment replacement have their own 64 MiB bounds. These are source
findings, not a reproduced large-state outage. Raising the limits would retain
the whole-state allocation and lifetime ceiling. Rejecting larger legitimate
state would not satisfy the indefinite-operation requirement either.

Implement this as one storage-format change with these consumers in scope:

| Consumer | Current dependency | Required replacement behavior |
| --- | --- | --- |
| Ordinary admission | `economic_candidate/staging.rs`, `ledger_updater.rs` | Write affected holdings, origin ownership and invalidation records in the same transaction as the block; unchanged accounts produce no economic writes. |
| Expiry and returns | `expiry_commit.rs` uses shared holdings and graph writers | Retain expired and revoked rights needed by later extensions and appeals. |
| Fork replacement | `payment_transition.rs`, `rollback_plan.rs`, `store_lmdb/src/block_undo.rs` | Journal affected records and verify current versions/dependencies; preserve disjoint changes without decoding whole-network before/after images. |
| Startup | `node.rs` restores through `trst_storage` and migrates after authenticated recovery | Restore one consistent schema; malformed or mixed-format state fails closed. |
| Certified install and replay | `confirmed_replay/snapshot/installed/changes.rs` and installation economics | Publish economic rows, authority and indexes atomically; preserve the existing crash/reopen contract. |
| Export and import | `TrstEngine::write_wallets`, `confirmed_replay/snapshot.rs` | Keep authenticated export contents deterministic and separate transport chunk bounds from total durable state. |

Use typed durable records for holdings, origin membership, invalidation causes
and merger edges/nodes. Reuse the existing LMDB merger tables only after their
writes participate in the caller's transaction and undo journal. Their current
standalone methods commit their own writers and cannot be called from admission.
Schema migration must be atomic and versioned. A second, dual-written aggregate
copy would preserve the bottleneck and is not completion.

Graph rows must also preserve independent consumers during fork replacement.
Reverse edges now use consumer/occurrence keys with separate ordering slots.
The ordinary writer preserves surviving slots; the reader accepts gaps while
checking multiplicity and the complete graph. A real LMDB journal test removes
an earlier merge, reopens, inserts another consumer, then rolls back the survivors
without rewriting their journals. Canonical graph bytes retain their old order.
Ordinary writes reject attempts to reorder existing consumers.

Startup now activates graph rows after authenticated recovery. It converts the
graph and provisional journals atomically, following decision 34.14. Fresh
snapshot projections replace their provisional journals in the installation
transaction, translating edge afterimages to the installed slots. Historical
positions left by an unrelated rollback do not become new edge identities.
The live-node journey verifies activation, restart and subsequent admission.
Do not treat ordinary publication as snapshot installation.
The replacement writer still scans all rows and retains an old-key set.
Candidate admission and expiry use graph deltas, checking changed before-values
and retaining existing reverse-edge slots. Ordinary deltas append ancestry;
fork removal and replay use checked replacement. Detached-map comparisons and
extended-parent edge scans remain. Reconstruction still loads graph state.
These are not bounded per-transition work or parallel preparation. The
C02/C03/C14/C16 boundary stays open.

Acceptance requires full-replay equivalence, disjoint and overlapping fork
replacement, expiry extension and appeal restoration, aborted writes, abrupt
restart, certified installation and a fresh observer. Add a large-state fixture
that crosses the old aggregate limit with bounded per-record work. Measure memory
and write amplification; passing small fixtures alone does not close C02/C14.
Account-scoped preparation and locking follow this storage boundary. They must
track cross-account provenance and lifecycle dependencies before publication;
independent signature checks do not establish economic concurrency.

The first implementation step is in `trst/src/engine/records.rs`. The legacy
snapshot decoder now uses the same record builder intended for row storage.
Records separate holdings, portfolio caches, origin ownership and each outcome
invalidation cause. Empty portfolios and owner registries remain representable.
The builder validates holder identity, ordered positions, duplicate ownership,
portfolio caches and governed expiry without applying a local-clock transition.
The visitor stops on the caller's write error. The incremental reader is poisoned
by any rejected record and cannot return an engine afterward. Version-one row
encoding binds each value to its metadata key, separates causes from invalidation
headers, and orders keys so a pinned database scan can feed the reader directly.
Limits apply to each encoded row, not the number of holdings or causes.
`node/src/trst_storage.rs` now
loads either explicitly marked rows or the legacy snapshot from the caller's
transaction, and rejects mixed/unmarked state. Ordinary economic persistence,
expiry, payment recovery and startup use that interface. Row writes use the
existing per-key undo journal. Full replacement for replay still visits the
whole engine and retains an old-key set. Ordinary admission and expiry use the
record delta visitor instead. This does not establish account-scoped parallel
preparation: global engine locks remain, and changed-row before-value checks are
not a complete read-dependency validation protocol.
Startup activates row mode after authenticated recovery and before admission
workers start, following decision 34.14. Current holdings and every provisional
journal convert in one transaction. Certified recovery translates its derived
afterimages and journals to the same format. The marker is not reversible by
a block journal. Abort, reopen, historical expiry-period rollback and live
two-epoch restart/payment checks pass. This does not finish the boundary: the
export remains aggregate, full graph replacement still scans all rows, journal installation has its
existing operation budgets. The 70,000-merge storage fixture crosses the old
64 MiB bound and verifies reopen, four-row delta journaling and rollback;
measurements are in the completion ledger. Full mixed-lifecycle large-state
recovery and production memory targets remain unqualified. No release gate
opens from these checks.

The C02 lifetime-progress change must cover admission and restart together.
Compact acceptance now calls `compact/retirement.rs::make_room` under the same
writer that publishes the certificate and head. It moves the oldest active
records to byte-preserving archive keys. `inventory.rs` therefore bounds an
active working set, not the lifetime checkpoint height. Do not remove its
count/byte checks as a purported longevity fix. The legacy full-history path
in `checkpoint_store/chain.rs` still has height limits; its existence alone does
not establish that the installed-state runtime stops at that height.

Genesis-backed recovery, checkpoint service handoff and historical export use
incremental replay without requiring an operator snapshot. The retained
`automatic_record_retirement_advances_without_snapshots_and_recovers` fixture
and `/tmp/burst-automatic-record-retirement-integration.log` cover 130 installed
roster checkpoints, recovery without a snapshot archive and historical export.
`/tmp/burst-live-retirement-local-progress.log` covers a live receiver crossing
128, both-node restart and subsequent payments. These are historical runs, not
qualification of today's release artifacts. Full mixed-lifecycle longevity,
total snapshot-size limits, whole decoded state and remaining authority/transfer
budgets still need qualification. Snapshot creation stays on-demand under
decision 11.3; record retirement is not physical ledger pruning.

For the next deliverable, follow `TESTNET_PILOT.md` P2-P5. Do not start another
storage optimization merely because this older section describes it. Complete
the rendered onboarding/verification/payment journey and exact-artifact recovery
and installation checks first. A fault in those journeys takes priority; the
full C02/C03/C14/C16 requirements remain open and still block full release.

Use `COMPLETION_LEDGER.md` for current status, not new iteration essays. Every
change names a capability and source decision; discoveries amend that row. At a
capability boundary record the exact positive behavior completed, evidence and
remaining blockers. Mark complete only after its entire acceptance scope passes.

During implementation run focused checks for changed behavior. Run broader checks
once at coherent integration boundaries or for a named regression uncertainty.
All heavy work uses `tools/limited-run.sh`; one capped job, no optimized desktop
release build, no duplicate sessions or user-app termination. Ordinary in-scope
implementation proceeds without approval. Ask only about unresolved product or
protocol choices, not routine engineering steps.
