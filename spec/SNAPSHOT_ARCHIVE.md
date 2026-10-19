# Authenticated snapshot archives

The archive holds a human-quorum certificate, the certified authority path from
a locally trusted base, and the economic state body. The body retains the
continuation state described in `node/src/confirmed_replay/snapshot.rs`.
It contains no private keys or local signer journals.

The economic body is version 5. It binds the ordered selection-checkpoint IDs,
their frozen execution inputs and historical vote/challenge/governance ordering
references needed for compact continuation. It also retains each replayed burn's
hash and timestamp so installation can rebuild mint-origin and expiry indexes
after the minted value has been sent or merged. Experimental economic body versions
1 through 4 are not accepted by this build. Export and attest version 5 at a new
certified epoch. Never clear a
signer's journal to attest different bytes for an epoch it already signed. The
archive framing is version 2. Embedded verifier
maps now serialize in deterministic order without changing their bincode shape.

Verifier execution state retains the agreed checkpoint and beacon after checking
the certificate. It does not retain the variable quorum signer subset or consensus
round. Equivalent proofs of the same decision therefore produce identical state
bytes. The node still retains and validates certificates in its authority chain.
Local verification persistence is version 8; its decoder migrates older local
verification records. That local migration does not migrate signed economic
archives or permit signing replacement bytes for an already-attested epoch.

`PreparedSnapshot::write_archive` exports an independently replayed body after
checking the supplied quorum signatures. `AuthenticatedSnapshot::read` checks
each authority handoff and the snapshot quorum before reading the state body.
The body hash, canonical encoding and restored state are then checked. The
reader rejects truncation, appended data, invalid signatures and wrong contexts.

The format starts with the 16-byte `BURST-SNAPSHOT\0\x02` marker, followed by an
eight-byte big-endian header length, the fixed-integer bincode header and the
canonical CBOR state. The header contains the authority steps and snapshot
certificate, followed by exact beacon supplements in selection order. Each
supplement names a selection checkpoint and is verified against its frozen
request and pinned drand configuration. Duplicate, unknown and out-of-order
selection IDs fail. Supplements do not change the signed economic body or its
state hash. Installed archives retain them for restart. Framing version 1 is
not accepted; re-export from authenticated history without clearing signer journals.
The v2 header is limited to 16 MiB, with at most one
4096-byte encoded beacon per step; the authenticated
body length is limited to 64 MiB. Readers reject unsupported versions. Network
callers must frame one archive and enforce an I/O deadline.

Exports whose v2 authority header would exceed 16 MiB use framing version 3.
Its marker is `BURST-SNAPSHOT\0\x03`, followed by an eight-byte big-endian header
length. The header contains an eight-byte big-endian authority count, then one
length-prefixed fixed-integer Bincode record per `SnapshotAuthorityStep`. A final
length-prefixed record contains `(SnapshotCertificate, beacon supplements)`.
Record lengths are eight-byte big-endian integers. Each record is bounded to
16 MiB and decoded separately. Authority is authenticated from the preceding
electorate and stored in the existing anonymous certificate file. The final
certificate must verify before the canonical economic body is read.

Small exports retain their v2 encoding. `AuthenticatedSnapshot::read_in` accepts
both versions and requires an existing scratch directory; the in-memory `read`
API remains v2-only. Node startup, installation, transfer and CLI verification
use `read_in`. Snapshot manifest signatures and economic-body encoding do not
change between formats. Unsupported framing versions remain errors.

The current implementation replaces the fixed total archive ceiling with
`snapshot_transfer.max_archive_bytes`, a local operator admission budget. Its
default is 83,886,104 bytes, preserving existing configuration behavior. The same
budget applies to serving, fetching, startup installation and live installation.
It is not a consensus parameter. Stored page recovery does not apply a new
operator budget to previously installed state. Export and offline verification
retain checked lengths and bounded frames without a fixed total history ceiling.

Focused budget, large-archive transfer and stored-page restart checks pass in
`/tmp/burst-archive-budget-focused.log` and
`/tmp/burst-archive-budget-integration.log`. The large archive uses synthetic
quorum-valid authority transitions, not ledger-replayed identity history.
`/tmp/burst-archive-budget-live.log` adds exact-budget startup download and live
replacement, refusal of over-budget files, and payment recovery after lowering
the admission budget below the installed archive size. Strict node Clippy passes.
The budget bounds archive input bytes, not total
disk usage: staging, authority proofs and database installation need additional
space. The body remains capped at 64 MiB and the final certificate/beacon record
remains bounded. Bounded state processing, total resource accounting and complete
large-history recovery acceptance remain unfinished. `read_in` stages economic
bodies above 1 MiB in anonymous files in the supplied scratch directory, alongside
authority proof storage. Invalid directories fail without falling back to the
process temporary directory. The legacy `read` API retains its default temporary
directory behavior. Small bodies remain in memory under the existing threshold.

## Attest and export an epoch

The economic-state export view borrows account, pending, BRN, parameter,
governance and historical-selection data from replay. These fields serialize with
the same version-5 schema as their owned equivalents. Import owns decoded fields
and moves BRN wallets/rates into the engine through the same validation used by
persisted-state loading. This avoids an intermediate BRN encode/decode copy.
Canonical encoding and both import consistency comparisons process one top-level
field at a time. A single schema declaration generates serde fields and the field
writer. Encoded keys retain length-first ordering, and field normalization counts
the enclosing map toward the existing depth limit. Top-level map values and
sequence items are encoded individually. Map ordering retains encoded keys and
references to values, not a value tree for the collection. Key indexes still grow
with map size. Lifecycle/proposal payloads are borrowed for export; byte-string
fields and blob-map values write their existing bytes directly. Import owns the
decoded payloads. TRST wallets and merger graphs stream their existing binary
formats into byte strings, with a bounded counting pass for the byte-string length.
Record preflight uses the same canonical field writers, so it does not recreate
those binary buffers. Sorted TRST/merger ordering indexes, nested individual
records, the byte-backed preparation API and decoded state still allocate.
This is not a bounded-memory state format yet.

Import releases each decoded TRST/merger byte payload after rebuilding its engine.
Merger canonical validation compares the binary writer against the input slice
without collecting another encoded graph. Wallet undo uses the same comparison;
encoder errors and length/content mismatches still reject the input.

Operator export prepares the canonical body in an anonymous file beside the
ledger, not in the process temporary directory. It derives the manifest from the
file's exact bytes. The shared archive writer authenticates the body and validates
any beacon selection path before writing the header. A lock covers authentication,
rewinds and output, so repeated exports cannot race over a shared file offset.
Dropping the prepared export releases the temporary file. Archive bytes and
attestation payloads are unchanged; the 64 MiB state limit still applies.

Installed-base persistence uses the same ledger-local file preparation and archive
writer. It streams into the existing transactional pages alongside economic and
ledger projection. Temporary preparation does not commit or publish anything;
the installation transaction and force-sync/publication rules are unchanged.

Checkpoint continuation computes its state commitment from canonical output as a
stream. It does not prepare an export byte vector when it needs only the state
hash. Byte-backed and streamed manifests share context and size validation.
Constructing a manifest from a supplied commitment does not authenticate that
commitment or authorize signing; replay remains responsible for deriving it.

Each representative independently replays the same explicitly chosen checkpoint
from its own retained ledger. Use the node's existing network and data directory:

```sh
burst-daemon --network live --data-dir ./burst_data \
  --representative-key-file ./representative.key snapshot attest \
  --epoch CHECKPOINT_HASH --output representative.attestation
```

The signer commits its choice to a local journal and synchronizes it to disk
before returning the signature. Repeating the same choice is allowed. A different
state for that epoch is refused. The journal and key are never exported.

Collect the attestation files, then export from a node that independently replays
that checkpoint. Repeat `--attestation` for each representative:

```sh
burst-daemon --network live --data-dir ./burst_data snapshot export \
  --epoch CHECKPOINT_HASH \
  --attestation representative-a.attestation \
  --attestation representative-b.attestation \
  --genesis-params-output original-genesis.json --output economy.snapshot
```

The number of files alone does not establish quorum. The exporter verifies the
epoch's human-weighted electorate, signatures and exact state commitment.
Minorities, duplicates, unknown signers and mismatched manifests fail. Outputs
are synchronized and published without replacing existing files. If the optional
parameter export fails after the archive is published, the error says so; the
archive remains available. Snapshot creation is on demand, per decision 11.3.

## Verify a file

```sh
burst-daemon --network live snapshot verify --input economy.snapshot \
  --genesis-params original-genesis.json \
  --minimum-height 100 --expected-epoch CHECKPOINT_HASH
```

Obtain the original genesis parameters independently, not from an untrusted peer
alongside the archive. The exporter can supply a copy for trusted distribution.
Without `--genesis-params`, verification uses the build's configured defaults.
TOML node settings do not override genesis parameters. Do not use current governed
parameters to derive the original trust anchor.
Replace the height and hash with values obtained independently of the archive.
`--expected-epoch` is optional. `--minimum-height` defaults to 1. Without an
independent freshness reference, a valid old snapshot can still verify.

Verification opens no ledger, starts no listener and writes no state. A valid
archive is not an installed or synchronized node.

## Transfer over connected peers

Serving and fetching are opt-in node settings. No extra port is opened. Existing
authenticated P2P connections carry protobuf snapshot requests and chunks.
On the serving node, configure an archive that was already exported:

```toml
[snapshot_transfer]
serve = "/absolute/path/economy.snapshot"
```

On the receiving node, choose the epoch independently and an unused destination
in an existing directory:

```toml
[snapshot_transfer.fetch]
epoch = "REPLACE_WITH_64_CHARACTER_CHECKPOINT_HASH"
output = "/absolute/path/download.snapshot"
```

To discover an available archive, omit `epoch` and set a minimum checkpoint
height obtained independently of the serving peer:

```toml
[snapshot_transfer.fetch]
minimum_height = 100
output = "/absolute/path/download.snapshot"
```

The default minimum is 1. The receiver queries one connected peer at a time and
downloads the first acceptable offer. It does not establish the latest network
checkpoint. An offer carries no authority; the downloaded header must authenticate
its epoch and exact advertised height, meet the local minimum, and bind the
archive length. A stale or false offer cannot lower the configured minimum.
Operators who require one exact epoch should retain the `epoch` pin.

The server authenticates the file against its original genesis parameters before
serving it. It keeps those exact bytes in an anonymous file until restart. Requests never
name filesystem paths. The receiver rotates through connected peers, retries
after one second and abandons a peer after five seconds without progress.
Progressing downloads have no absolute 120-second cutoff. After header
authentication, another connected peer can continue the same digest at the saved
offset. If the available peers cannot continue, the receiver can start a fresh
attempt. Unanswered queries also time out after
five seconds and move to another peer. Discovery replies and chunks share a
64-response-per-second serving cap; chunks are at most 16 KiB. Only one query or
snapshot download is active per node. Unsolicited offers cannot start a download.

Before requesting more body chunks, the receiver authenticates the completed
header against its local genesis anchor and requested epoch. One chunk may
contain the end of the header and the start of the body. The complete archive's
transport digest, signed state hash, canonical encoding and economic state must
pass before publication. Download bytes go to an anonymous staged file. Header
authentication reads that file and decodes authority one step at a time into
temporary certificate storage. It does not retain a whole header buffer.
The v2 header and economic-body size limits remain; this is not a claim that
all economic state or lifecycle history uses constant memory.

The destination is published without replacement. Restart rechecks an existing
file and skips the download only if it authenticates, meets the minimum height
and matches the optional epoch pin. Discovery does not continuously replace a
completed download with newer state.
An interrupted download starts again; partial state is not installed. Without
the explicit matching `snapshot_bootstrap` path described below, these settings
stage a file only. They do not activate payment authority, replace local
signer promises, switch cached engines or authorize pruning. Ordinary block and
checkpoint synchronization continue separately.

## Historical witnesses and bounded suffixes

`AuthenticatedSnapshot::history_cursor` starts at an account head committed by
the authenticated body. It verifies newest-to-oldest chunks of at most 128 blocks
and 1 MiB. Hash links, account ownership, heights, timestamps and signatures must
match. A rejected chunk leaves the cursor unchanged. A partial archival source
can supply a prefix, then another source can continue at the verified next hash.
`checkpoint_sync::witness::fetch` now requests selected ancestors through the
running node's authenticated peer connections. The caller supplies the trusted
cursor and requested hashes; the peer cannot choose the starting authority.
The service returns authenticated witnesses without writing ledger rows or
changing payment authority. Automatic compact-startup orchestration is unfinished.

`checkpoint_sync::witness::fetch_plan` accepts the complete suffix plan. It
first requests a missing hash to discover its account, checks that body's hash,
then walks from the account's independently authenticated head. The hint body
is not a membership proof. A signed body outside the authenticated prefix is
rejected even if it matches the requested hash. The worker retains only requested
ancestors and visits each selected account prefix at most once. Account hints
and ancestry walks share the same request deadline and work budgets; these limits
do not reset for each account. Plans from another authenticated base are rejected
before transport. The existing wire messages need no new fields for discovery.

One local proof request is active at a time, with one queued command. Each
request retains at most 131,072 witnesses and 64 MiB of witness bodies. Its ancestry
walk is limited to 131,072 blocks, 64 MiB of received data and two minutes.
Chunks are 16 KiB; each ancestry batch has at most 128 blocks and 1 MiB of raw
block bytes. The receiver retries after one second and rotates peers after five
seconds without progress. Wrong-peer and unsolicited replies do not advance the
cursor. Invalid digests or membership proofs leave the cursor at the last
authenticated position. Cancellation releases the request at the next poll.
Servers limit responses to 64 chunks and four batch loads per second.

Exact committed roots use generation-32 head requests in groups of at most 64.
Each response contains a whole-head prefix within the same 1 MiB body budget.
Every body must match its independent authenticated cursor. A late invalid body
rejects the entire response prefix without advancing any cursor. The client
requests the remaining roots after accepting a valid prefix. This avoids one
server batch load per committed root without raising serving limits. Unknown
ancestors still use account discovery and authenticated ancestry walks.

The cursor returns witnesses bound to the snapshot's network, genesis, epoch
and state hash. Neither a peer-provided head nor a serialized trust flag can
construct one. Work nonces are excluded from block hashes by decision 4.9, so
historical membership does not authenticate them or replace admission checks.

`AuthenticatedSnapshot::read_tail` reads only account suffixes after that base.
Its bounds match full-history replay: 131,072 new blocks and 64 MiB including
retained witness bodies and proposal content. Finished witness bodies are staged
beside the ledger; reconstruction checks their pinned bytes and retains the
original authenticated context and height.
Every opened base account must remain in the proposed heads. Each suffix must
extend the exact base head, use its payment epoch, and pass signature and time
bounds. An older non-head dependency needs a witness from the same snapshot.
Unrelated or duplicate witnesses fail. Dependency ordering stops at authenticated
heads and witnesses instead of walking their ancestors.

Tests read suffixes from a separate store containing no old blocks and compare
the resulting commitment with full history. They cover cross-account sources,
rollback, missing accounts, wrong epochs, bad signatures and unrelated witnesses.
The reader also loads and authenticates full governance proposal bodies within
the same byte budget. The reader alone checks candidate structure, not economic
validity or finality. It cannot authorize admission, installation or deletion.

`plan_tail` reads and checks the signed suffix before fetching historical
witnesses. Its sorted request list includes old direct dependencies, retained
ordering records and selection heads from the authenticated state. It reads no
pre-base ledger bodies to discover those requests. The request inventory is
conservative and capped at 131,072 hashes; long-running inventory compaction is
still required.

`SnapshotTailPlan::finish` requires every requested body as a snapshot-bound
membership witness. It rejects missing, duplicate or foreign witnesses, then
orders the suffix within the same shared byte budget. The plan retains the
checked suffix bytes, so later store changes cannot substitute different bytes
during this step. Installation still needs its writer-owned rechecks. The
existing `read_tail` API remains a structural reader; semantic replay checks its
execution dependencies separately.

`AuthenticatedSnapshot::replay_tail` consumes that candidate and runs the same
economic executor as full history. Required ordering witnesses must all be
present, and selection certificates must match the snapshot's exact ordered
checkpoint IDs. Direct dependencies, including predecessor balance records and
historical appeal sources, still need their authenticated bodies. Missing
witnesses fail rather than falling back to provisional database state.

This API executes within the snapshot's payment epoch. It retains the existing
checks for pending expiry, completed panel deadlines and due governance cuts.
It cannot invent the next lifecycle certificate or jump over a due transition.
Failure consumes the private replay state without publishing partial changes.

## Certified continuation

`AuthenticatedSnapshot::into_continuation` carries the authenticated state across
certified checkpoint boundaries. Each `replay_checkpoint` call checks the exact
successor against the previous human roster, matches its heads and cut to the
bounded suffix, then uses the full replay path's lifecycle executor. Selection,
activation, deadline, outcome, roster and governance effects must reproduce the
certified commitments. A skipped boundary or mismatched cut fails.

The resulting state supplies new authenticated account-head cursors. Witnesses
from the previous base cannot be reused under the new context. This derived
state is not an installed ledger or a quorum signature over a new archive.

Differential tests compare complete canonical state bytes with full replay
through panel activation and deadlines, expired pending returns, fraud and
inactivity appeals, roster changes, governance counting and rate activation.
Each suffix store contains only new blocks and proposal bodies. The source
store remains archival for independent comparison and witness delivery.

`SnapshotContinuation::replay_manifest` accepts an untrusted manifest and a
checkpoint certificate for roster, activation, deadline, challenge outcome and
governance events. It verifies the previous human quorum before executing the
tail and derives the next roster from the result. The manifest must match the
exact heads, cutoff, ledger root, event commitment and parameters. Governance
proposal inventories must match too. A roster event must change authority; a
manifest cannot substitute another event type or change the quorum threshold.
Manifest size and activation proofs retain the existing byte limits.

This path does not construct a trusted event by reading the old prefix. It still
needs the previously reconstructed selection records. Selection manifests use
`SnapshotContinuation::replay_selection`, which also returns the reconstructed
selection record for later cuts. It derives the request, pool, attempt, frozen
endorsements, lifecycle history and roster from the compact state through the
same constructor as full replay. The resulting checkpoint must match the
old-roster certificate exactly.

`CertifiedCheckpoint::attach_compact_beacon` checks that recovered record's exact
round and BLS proof without writing a store or starting a panel. Missing
randomness still leaves selection pending. A new cut cannot replace an already
frozen attempt. Recovery creates no endorsement or revote by itself.

Compact selection records deliberately have no complete historical inclusion
inventory. Local agreement/signing rejects them, including before touching its
journal. A valid recovered decision cannot be reused as proof that a proposed
cut includes every local signer's durable promise. The wire formats are unchanged.

`AuthenticatedSnapshot::recover_selections` restores earlier frozen selection
inputs from the authenticated body. It matches each checkpoint to its certificate
and actual parent authority in the authenticated archive path. It reads no
ledger. The restored records have the same execution-only restrictions as
compact successor records. Beacons must still pass their exact-round BLS checks.

An archive pinned directly to its tip can authenticate economic state without
carrying older authority steps. It cannot restore selections whose parent path
it omits. Recovery fails without returning a partial record set. Operator exports
currently carry the path from configured genesis.

## Stored certified-tail recovery

`AuthenticatedSnapshot::recover_stored_successors` reads retained checkpoint
records after the authenticated base. It does not need pre-base ledger bodies or
a complete set of pre-base checkpoint records. Any older records still present
must match the archive's authority path. Missing successors, misplaced records,
altered head commitments and invalid certificates fail before their execution.
The existing checkpoint-count and record-byte limits still apply.

Recovery uses the same compact economic replay for each successor and derives
each new electorate before checking the next certificate. It returns the accepted
tip, which may be ahead of the node's installed payment epoch. It does not update
that epoch, publish engines or check the provisional payment tail.

The synchronous API accepts a caller-supplied proof provider. It supplies that
provider with the checked suffix plan and its request list, without replaying
full history to discover dependencies. `recover_stored_successors_from_peers`
connects the same planning and replay path to the multi-account peer fetcher.
Planning and replay run on a blocking worker; no database writer or runtime lock
is held while waiting for peers. Cancellation cannot publish partial state.
Both paths require membership witnesses from the authenticated account heads.
The peer recovery API requires an already-running transport service. Gated compact
startup starts the recovery listener and proof service before calling it, while
normal admission remains closed.
A snapshot taken before a selection beacon arrived cannot supply that later
proof. Recovery can read it from a retained selection record, activation proof or
separately persisted archive selection beacon. Version-2 archives also carry
proofs available at export time. Each path rechecks the exact round and BLS
signature. Missing required proofs still fail.
Recovered selections remain ineligible for local checkpoint signing.

`recover_stored_epoch` and `recover_stored_epoch_from_peers` recover an exact
epoch instead of the accepted tip. The requested hash must name the authenticated
base or a retained successor. Recovery replays every required predecessor and
does not apply later records. Unknown or pre-base targets fail before requesting
historical witnesses; selecting a hash does not bypass certificate validation.

`installed::recover_metadata_from_peers` reads the stored archive and installed
epoch from one database view. It authenticates the archive against independently
configured genesis, recovers that exact epoch, then checks the local cemented
suffix and installed metadata described below. Missing compact state returns
`None`; malformed state or an invalid installed target returns an error. It does
not silently fall back to another epoch.

The result keeps an observation of the archive bytes, the checkpoint records
used by recovery and the accepted-tip marker. Its writer-owned `verify_unchanged`
rechecks those inputs along with the installed metadata, cemented suffix and
complete physical block inventory.
Changes while proofs are fetched cause rejection without repair. The same
recheck is available before later publication; no database lock crosses a peer
wait. This result does not authorize admission. A live compact startup phase
still needs to consume it and validate the remaining secondary indexes.

Applied suffixes have a separate type from cemented suffixes. They check signed
contents, work, parameter and epoch binding, then run the shared economic
executor without advancing confirmation heights or executing deadline cuts.
Recovery compares account afterimages, BRN state, pending transfers, TRST
provenance and the merger graph with storage. Private comparison copies use the
same expiry clock; ownership and revocation must still match. Account expiry
counters remain local derived data and do not establish a spendable balance.
Verification, bootstrap, protocol parameters, governance, constitution,
persisted delegation graphs and replay-owned lifecycle records also must match.
Secondary indexes, undo reconstruction and runtime publication remain separate
startup work.

Frontier and pending-sender lookups now have writer-owned checks against their
primary rows. Missing, extra, wrong-key and wrong-value entries reject recovery
without repair. Both scans have row and byte limits. Token provenance indexes
and undo recovery still need integration before startup can authorize admission.

`RecoveredInstalledMetadata::project_token_indexes` now stages origin, expiry
and reverse lookups from the authenticated applied mint inventory. It uses the
replayed governed expiry period, including mints no longer present in live
holdings. The method takes a fresh writer and rechecks the recovery observations
before replacing any index rows. Its result owns the uncommitted writer and has
no commit method. Dropping it aborts reconstruction. Undo handling and runtime
publication must consume this boundary before compact startup can use it.

The projection also collects replay-derived per-block changes before taking its
writer. These cover accounts, pending transfers, economic/lifecycle engines,
block bytes, both height indexes, frontiers, verified counts and mint lookups.
Replay records bootstrap identity sources and binds subsequent account blocks
through the same helpers as admission. The collector bounds each row view and
retained delta payload to 64 MiB, retained changes to 131072 entries, and
serialized row work to 256 MiB. It does not retain a full replay state per block.
Before staging index writes, the writer rechecks both height indexes and drops
changes for already-finalized blocks. It preserves actual confirmation heights
in provisional account images and rejects rollback across confirmed history.
Expiry rows use a private TRST view with account counters recomputed from that
view. The default clock is the authenticated cutoff. Startup callers can supply
a later validated clock through `project_token_indexes_at`; clocks before the
cutoff reject before opening a writer. The same clock normalizes every journal
image and the final cache rows. It does not change signed replay, execute
deadlines or synthesize pending returns. Revocation and signed balances remain
unchanged.

`RecoveredIndexProjection::stage_undo` rechecks the inputs and stages the final
replay-derived account/engine bytes with the reconstructed provisional journals.
Storage requires a complete provisional inventory, matching block/height
insertions, canonical records and no signing-journal changes. The resulting
`RecoveredUndoProjection` owns the writer and exposes no production commit.
Dropping it aborts both cache and journal replacement. A disposable test fixture
can publish through a test-only method with expired account and portfolio caches.
The fixture force-syncs, closes all store references, reopens the database and
reauthenticates it through peer recovery. Reconstructing at the same cache clock
produces identical journals; dropping that second projection leaves the saved
journals unchanged. Rewind after reopening preserves the confirmed prefix.
This is a clean database reopen, not a process-crash or live-worker publication
test. Live runtime publication, subsequent expiry refresh
handling and arrival-order/lifecycle comparisons remain required.

Local signing journals are validated and pinned, never reset. The checks cover
checkpoint rounds, payment promises and fences, closed epochs, sequence and
timestamp records, governance deadline promises and snapshot attestations.
A local checkpoint decision ahead of the installed epoch requires a separately
authenticated accepted path. That path does not replace installed authority.
Malformed, foreign or unsupported journal records reject recovery. The final
writer-owned check also pins the accepted records used for this validation.

Recovery also inventories local undo records under the same aggregate journal
limits. Keys, versions, block identities and canonical encoding are checked,
and any change before publication invalidates the observation. Undo recording
and decoding both forbid snapshot-attestation promises, as they already forbid
payment and checkpoint signing journals. These checks do not authenticate
economic before-images or recreate missing undo. That work is still required
before the startup transaction can commit.

The retained-history check inventories physical block rows, not every logical
height. It requires a bijection with both height indexes and rejects rows without
an account or above its local head. The inventory is limited to 131,072 rows and
64 MiB, including index bytes. Bodies at or below the authenticated snapshot
height must prove membership in that account's history. Contiguous local bodies
advance the authenticated cursor directly; gaps use the existing peer witness
service. Only opaque, cursor-produced proofs can advance past a missing span.
The local body's signature and height are checked even when a peer supplied the
membership proof. Historical proof-of-work nonces remain outside the signed
block hash, as specified in decision 4.9.

The final writer-owned check compares account observations and every retained
row's hash, account, height and body digest. Insertions, deletions and body changes
invalidate the observation. Rows above the snapshot are pinned by this inventory;
the inventory alone does not authenticate their economics.

## Reconcile local finality before installation

### Compact installed-state checks

`SnapshotContinuation::plan_cemented_tail` discovers the local cemented suffix
from one read transaction. It checks retained snapshot-head bodies against
authenticated account cursors and validates both height indexes. It reads only
post-base cemented blocks, not heights 1 through the snapshot. Provisional blocks
above confirmation height are excluded. The retained head and suffix rows share
a 131,072-row, 64 MiB observation budget. Observations retain lengths and digests,
not body copies. Account discovery is bounded separately.

The returned plan uses the existing historical-witness provider. After those
proofs arrive, `verify_installed_metadata` takes one writer lock and checks that
the observed accounts, indexed rows and body bytes have not changed. It compares
the installed authority, cutoff and vote limit with the authenticated continuation
and rejects a closed installed epoch. The governance schedule and counting limit
must match the certified baseline plus cemented governance operations. Full and
compact validation share the same schedule promotion and comparison code.
Nothing is repaired or committed by this check.

This is not yet a node-startup authorization. Other retained historical rows,
provisional economics, cached engines and local signer journals still require
validation. An accepted checkpoint tip is not a substitute for the installed
epoch; callers must supply the continuation for that exact installed state.

### Installation boundary

`AuthenticatedSnapshot::installation_needs` reads local cemented heads and
pending confirmation targets from one database view. It identifies historical
membership proofs to fetch before acquiring the database writer. These are
download hints, not permission to replace state. It also lists the authenticated
account heads whose bodies the compact projection needs.

`reconcile_installation` repeats the checks while owning a clean, exclusive
write transaction. The archive must authenticate from the configured genesis,
extend the installed epoch and preserve accepted checkpoint decisions, including
decisions still present only in local agreement journals. Every local cemented
head and pending confirmation needs matching snapshot membership. Witnesses from
another snapshot, conflicting account positions and inconsistent local authority
fail without writing anything.

The returned guard keeps the writer until dropped. It cannot commit. Ordinary
final-vote promises, checkpoint locks and signing fences remain byte-for-byte
unchanged. A promise absent from supplied witnesses is not classified as an
excluded minority vote. Missing membership does not establish exclusion.

Ten tests cover fresh and used stores, reopen, competing confirmation work,
checkpoint decisions and locks, corrupted metadata, and stale or conflicting
snapshots. This is the checked installation boundary, not a live installation.

`ReconciledSnapshot::project_ledger` now constructs sparse ledger rows under that
same writer. It accepts only witnesses bound to the authenticated snapshot and
requires a body for each opened account head. Reconciliation witnesses and all
already-retained cemented bodies remain indexed. Missing intermediate bodies
stay missing; the implementation scans retained rows instead of looping through
every logical height. Logical block count and physical retention are reported
separately. Witness input and storage scans use 131,072-row and 64 MiB bounds.
Authenticated installation rows are staged and consumed once by the uncommitted
writer. A source error poisons that writer and prevents partial commit.

Unproven provisional bodies move to local orphan evidence without being called
excluded by the snapshot. Pending confirmations are acknowledged only when their
bodies remain in the certified projection. Signing journals and the current
epoch are unchanged. Projection errors poison the writer, including an error
after an earlier body was archived. Dropping the returned guard aborts all rows.

`ProjectedSnapshot::project_economics` adds the economic tables, governed
parameters, verification/governance state, pending transfers, mint indexes and
authority descriptor to the same writer. Full-history installation uses that
same projection routine. Lifecycle metadata may contain only the owned lifecycle
namespaces and the verification-state record. It cannot target local signer
journals or the persisted recovery base.

The transaction also stores the original authenticated archive at
`checkpoint_snapshot/v1/base` and closes the previous payment epoch without
clearing promises or claiming unproven exclusions. The base is protected from
ordinary block undo. `installed::load_base` reauthenticates it against configured
genesis and checks historical selection recovery. Reading the base alone does
not validate a newer tail or establish permission to start signing.

The downloaded-base guard now has a crate-private, release-gated startup commit.
The explicit `snapshot_bootstrap` archive path starts proof retrieval while
admission is closed, commits and force-syncs the projection, then runs installed
state recovery before publishing caches. To download before installation, explicitly
set `snapshot_bootstrap` to the same path as `snapshot_transfer.fetch.output`:

```toml
snapshot_bootstrap = "/absolute/path/download.snapshot"

[snapshot_transfer.fetch]
epoch = "REPLACE_WITH_64_CHARACTER_CHECKPOINT_HASH"
minimum_height = 1
output = "/absolute/path/download.snapshot"
```

This remains release-gated. Startup uses a bounded archive-only inbox while normal
admission is closed. It checks the configured epoch/freshness floor even when a
completed file already exists. Cancellation or 120 seconds without new accepted bytes
fails startup without installing partial state. The downloader does not replace
existing files. Fetch without the matching bootstrap path still only stages a
file. Installation then retrieves authenticated account proofs, commits the
projection, force-syncs and recovers installed state before opening admission.
Iteration 255 passes the full-node active-panel download/install/payment/restart
journey. Before the server starts, recovery P2P is available and RPC stays closed.
Cancellation and recovery-inbox isolation pass too.

Iteration 251 passes the empty-node archive installation,
peer-proof retrieval, payment admission and restart journey. Selection publication
now checks pre-base receipts against the stored signed archive and its installed
authority. Exact-round beacon proofs persist separately at
`snapshot_selection_beacon/v1/{selection}` and are reauthenticated during recovery.
The archive-backed committee test passes wrong-round rejection and reopen without
historical checkpoint records. Iteration 252 passes forced process crashes before
commit, after commit, after force-sync and during economic/final cache publication.
RPC remains closed at each boundary. Each restart validates persisted state and
admits a payment. These are process-kill tests, not power-loss tests.
Iteration 253 passes active-panel archive bootstrap with the original committee
and activation deadline, a payment, restart and another payment. Its version-2
header preserves exact beacon proofs without changing the signed economic body.
Iteration 254 passes completed normal-verification bootstrap and restart, including
the verification snapshot, activation metadata, stake state and pending rewards.
Iteration 256 passes startup replacement of an older compact base that has
accepted successor certificates and an unresolved final-vote promise. Both sets
of bytes survive replacement and restart; the active panel and payments still
work. This fixture-initialized database does not cover every provisional tail
or in-place rotation of running workers. Iteration 257 passes SIGKILL during a
partial download, before authenticated file publication and after publication
but before installation. RPC stays closed and the reopened ledger contains only
genesis. Each restart recovers the archive state and admits a payment. These are
process-crash tests, not power-loss tests.
Iteration 258 passes five replacement process-crash boundaries. The old base
and metadata survive before commit; recovered state matches the new archive
after commit. Accepted certificates and an unresolved final-vote promise survive
every boundary, followed by restarted payment admission. Iteration 259 adds
settled fraud/inactivity challenge archives and successful appeals of both.
Running-node bootstrap and restart preserve receipts, certification markers,
identity state, holder balances, stakes and rewards. TRST wallet and merger-graph
bytes match before payment. Iteration 260 restores active fraud/inactivity
challenges and appeals with their panel, deadline, stakes and original judgment
intact across restart. These checks admit payments but do not complete a new
certified review after archive recovery.
Iteration 261 supplies later certified challenge deadlines and restoring appeal
outcomes to the peer only after the target is ready on its active-review archive.
The target receives and installs them while running, keeps the same compact base
and admits payments before and after restart. Economic and lifecycle records
match the archival peer. Both fraud and inactivity cases pass. Certificates and
clocks are fixtures; this is not independent quorum-formation evidence.
Preparation now stages the accepted checkpoint head in the same writer as the
archive and economic projection. It preserves retained certificate bytes rather
than manufacturing missing pre-base records. Reconciliation permits missing old
records only under an authenticated stored base on the incoming archive's path;
every accepted successor after that base must still be present. Retained records
must match their certified account-head commitment.
Dropping any guard still aborts the ledger, economic state, archive and epoch
together. The sparse storage primitive alone is not an import API or permission
to restart a node from partially projected state.

## Installed compact successors

`RecoveredInstalledMetadata::prepare_next_from_peers` replays the immediate
accepted successor of the checked installed epoch. The checkpoint publication
runtime now uses this path when a compact base is present. Its gated commit
rechecks installed metadata, retained records and finality under one writer,
projects sparse authenticated bodies and economic state, advances the epoch,
then force-syncs before publishing runtime engines under the existing locks.
The original signed base remains stored; successor records authenticate later
state. All old signing promises remain, without unproven minority classification.

All six event types pass commit and installed-state recovery in the TCP observer
fixtures. Separate runtime-object tests cover cache publication and idempotence;
coordinator rotation uses the checked commit. Iteration 246 also passes a
started-node journey through two compact successors, actual peer proof retrieval,
RPC payment admission, relay and restart. The sparse store still lacks the full
checkpoint prefix. Iteration 247 kills the sparse node at five boundaries of
its second successor installation: before commit, after commit, after economic
cache publication, after authority publication and after complete publication.
Each restart recovers the authenticated state, rejects stale-epoch payments and
stores a valid payment. These are process crashes with fixture certificates and
controlled clocks, not power-loss tests or installation of a newly downloaded base.

The historical-proof service keeps a bounded in-memory cache of successfully
authenticated requested bodies. Reuse requires the same network, genesis,
checkpoint and snapshot state hash, plus membership within the requested account
prefix. A cache miss uses the existing peer retrieval path. The cache retains at
most 128 bodies and 1 MiB of encoded block data, with bounded per-entry overhead.
It neither writes ledger data nor bypasses installation's final writer checks.
It is not a pruning policy or a substitute for an available archival peer.

## Still required for fast sync

The full-node download/install/payment/restart journey and closed-admission checks
pass, together with 783 fast regressions and strict node/daemon Clippy.
Startup replacement of an existing compact base also passes, including retention
of accepted certificates and an unresolved final-vote promise. Process-crash
recovery also passes around download publication and at five replacement
boundaries. Settled challenge and successful appeal archives also pass bootstrap
and restart. Active challenge/appeal archives also restore across restart;
later certified challenge deadlines and restoring appeal outcomes pass live
peer delivery, installation and restart too. Pruning remains unfinished.
Compact replay reconstructs each kind of successor and restores
pre-base selection records from an authenticated archive with their parent path.
The installed-base startup and successor publisher use the compact APIs.
Iteration 262 lets the running checkpoint service detect a new compact base and
handoff to authenticated compact recovery without losing its receiver or pending
packet. Invalid bases fail rather than resume full-history processing. A service
test publishes the base directly in a disposable store.

Iteration 263 adds `BurstNode::snapshot_installer()`, a gated local control handle.
Obtain it before moving the node into its running task, then call `install(path)`.
It authenticates a bounded archive and retrieves history proofs before queuing
publication in the single ledger worker. The writer rechecks finality and signing
promises under the existing engine and election locks. It commits and force-syncs
before publishing matching runtime state. One install may be active at a time.
Cancellation before submission makes no ledger changes; after submission it
cannot undo a commit already in progress. No RPC endpoint or pruning is added.

Running-node tests cover installation from genesis and replacement of an older
compact base, unchanged local promises, corrupt/nonadvancing archive rejection,
and payments before and after restart. Initial installation uses a controlled
authenticated proof adapter; restart uses real peer transport. Live peer delivery,
signing races and live-install process crashes remain unverified. Source release
gates stay closed until the complete path is validated.

Iteration 264 adds controlled late-write checks. A final-vote promise written
after proof selection survives live installation and restart. A confirmation
target added at that point, without membership in the retrieved proofs, makes
publication reject without changing the epoch, ledger count or metadata. The
queue record remains durable. These cases do not establish all concurrent
signer orderings or peer-delivered live installation.

Iteration 265 verifies initial live installation and compact-base replacement
through the normal authenticated peer connection and witness service. Ordinary
checkpoint requests are withheld until installation finishes, then their
delivery is restored before restart. Both journeys preserve local promises and
economic state, reject stale payments and accept current-epoch payments before
and after restart. This replaces the proof adapter for those cases, but still
uses fixture certificates and controlled scheduling. Arbitrary network schedules,
remaining signing races and live-install process crashes are not established.

Iteration 266 adds five SIGKILL boundaries for initial live installation.
The pre-commit case retains the old epoch and no base; the other cases recover
the authenticated archive after commit and force-sync, including partial runtime
publication. Reopen preserves the ledger count captured at the boundary and
the exact unresolved final-vote promise. Restart uses real peers and checks
stale/valid payments. The offline state comparison uses a controlled authenticated
proof adapter. This does not cover power loss, disk faults or live replacement
of an already-installed compact base.

Iteration 267 covers those five process-crash boundaries during live replacement
of an existing compact base. The child must first report the old installed epoch
through RPC. A pre-commit kill preserves the old archive and state; later kills
recover the new archive and state. The exact old-epoch signing promise and
captured ledger count survive every case. Real-peer restart rejects stale
payments and accepts valid payments. Power loss, disk faults and arbitrary
signing/network schedules remain unverified.

Iteration 268 tests an injected error between live commit and force-sync. The
node returns the fatal cause during cleanup and closes both listeners while its
in-memory authority still reflects the previous epoch. Restart recovers the
committed archive and resumes valid payments. The injection exists only in tests;
it validates the failure-handling branch, not physical disk faults or power loss.

The full-history replay path still seeds `applied` from every block in the preceding
`ConfirmedHistory`. It uses those signed blocks for account predecessor balances,
pending sources, endorsement target heads, appeal admissions/outcome receipts and
governance ordering. `Selections` also rebuilds panel dependencies from certified
selection records and vote blocks. Those are functional dependencies, not
redundant logging that can be deleted to make a pruning test pass.

Arbitrary old references need authenticated recovery from archival peers when
their witnesses were pruned. Stored successor recovery now coordinates those
requests through the peer fetch API. Gated compact bootstrap starts that transport
before admitting payments and consumes the recovered state. Installation
reconciles the base with cemented local history and durable signer promises in
one transaction, then publishes engine caches before reopening admission.
The existing full-history path remains available for complete signed cuts.
Neither this dependency inventory nor archive authentication authorizes deleting
history.

The new reference inventory conservatively retains all historical verification
votes, challenges, governance support and withdrawals. Selection receipts retain
historical frozen inputs and account heads; the mint inventory retains burn
identities and timestamps. Compacting these inventories and
the authority path remains necessary for long-running operation; no limit was
raised here.
