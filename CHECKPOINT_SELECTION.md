# Certified verifier selection

Iteration 161 adds owner-authorized expired-proposal recovery through the
existing consensus rounds. A fresh later-cut proposal still needs pre-beacon
votes; known prepare proofs retain their exact checkpoint and beacon. The
independent-store restart test and service TCP test pass. See
`spec/DEADLINE_RECOVERY.md` for the rule and the separate adversarial-bias review
that still blocks release.

Iteration 136 changes P2P envelopes to Protobuf in wire generation 30 while
preserving signed block encoding. Repeated connected fraud appeals and an
inactivity appeal pass settlement, restart and fresh-observer recovery. An
opt-in authenticated-clock test recovers a promised future drand round after
storage reopen. An honest 2+2 partition at fraud and appeal deadlines retains
identity, stakes and payment epoch until quorum recovers, then passes settlement,
restart and fresh-observer recovery. Byzantine handoffs, production clocks and
the remaining prerequisites below still block release.

Iteration 135 connects configurable exact-round relay failover to the node.
Relay requests run concurrently within a four-relay bound; successful proof
verification cancels the remaining requests. Interrupted-fetch recovery retains
the same certificate, frozen request and committee. Historical proofs and local
HTTP relays do not establish production-time beacon secrecy or partition safety.
Fatal checkpoint worker errors now request shutdown and retain their cause.
Shutdown flushes committed storage without rewriting it from engine caches.
Release gates remain closed.

Iteration 132 adds governed verifier balance and emergency voting rules. The
expanded connected-node journey passes active-panel settlement, emergency
activation/rejection, all-node restart and fresh-observer recovery. Wire
generation was 29; verification snapshots remain version 7. Separate fresh
development data is required by the parameter encoding. Release gates stay closed.

Iteration 131 added governable rolling Neither-vote rules. Certified activation
updates the retained window in replay order; timeout votes use the active
threshold. The connected active-panel governance journey passes settlement,
all-node restart and fresh-observer recovery with those rules. Wire generation
was 28 and verification snapshots use version 7. The new genesis-parameter
encoding requires separate fresh development data. This does not satisfy the
production timing or adversarial recovery prerequisites below; gates stay closed.

Status: certificate validation, frozen requests, bounded cut replay and durable
round-recovery transitions are implemented and tested. A running-node service now
transfers checkpoint records and authenticated history. The proposal transport, scheduler and
height-rotation runtime are implemented and exercised by independent-node tests.
Ordinary nodes currently run that path as observers. Automatic checkpoint signing
and normal verifier selection remain behind the release gate.
The explicit genesis bootstrap path still works.

The run recorded in `live-install-network-2.log` passed
normal verification through live checkpoint agreement, committee installation,
signed votes and settlement. It also settles a rejected challenge, restarts all
five existing nodes, and reconstructs the result on a fresh sixth observer.
This uses unit-test-only controlled clocks and
real historical drand proofs served locally. It does not validate real-time
beacon secrecy, production clock bounds, missed-deadline recovery or Byzantine
roster transitions. Those gaps still prohibit opening the production gate.
`live-install-network-2.log` records the passing run with every selection acting
as a payment-epoch boundary. It exercises the running installer and agreed
payment electorate. The earlier run stalled because a fresh observer waited for
ordinary votes from a closed epoch; that failure is retained in
`live-install-network.log`.

A second six-node fixture collects a complete upheld challenge panel without
revoking on the last arriving vote. A later certified cut settles the challenge,
removes the target from the payment electorate and enables payments in the new
epoch. Restart and a fresh observer recover that result in
`live-install-upheld-2.log`. `checkpoint_outcome.rs` separately
replays and authenticates an upheld outcome at an exact cut. Its candidate uses
the prior roster's quorum and a distinct outcome commitment. Fraud splits merged
TRST at current holders; inactivity stops the wallet without revoking its TRST.
The economic candidate remains read-only. A gated mixed certificate chain now
stores selection records and outcome records in the same height sequence and
replays settlements in that order. The gated runtime now schedules complete
upheld panels and persists their decisions. A gated durable installer now commits
replayed state and the payment-epoch handoff together. The block-processing task
now calls it between admissions and fork replacements, then publishes rebuilt
engines, frontiers and elections. Payment weights come from the installed agreed
roster. The live test also checks that pre-settlement payments cannot cross the
closed epoch. An RPC status bug found by the first run is fixed: a revoked wallet
is no longer reported as merely unverified because of its process phase.
Both journeys pass again in `live-install-node-final.log`. The recovery assertions
also compare portfolios, pending transfers, payment epochs and accrual state.
The fraud test rejects a revoked wallet's payment even with the current epoch.
No ordinary node can enable revocation by importing this candidate.

The durable signer now accepts typed selection and outcome candidates in one
height record. Outcome signing is unit-test-only while release prerequisites remain
unfinished. Seven independent signer stores produce and recover an outcome
certificate through the real round transitions. A selection cannot overwrite an
outcome vote at that height. Signer-journal format 3 checksums the candidate kind;
format-2 records retain their checked state and locks when a successful transition
writes the new format. Agreement messages now carry typed selection/outcome
manifests, and the driver replays both before signing. The single-height worker
retains outcome decisions in its journal. The height-rotation runtime persists
those decisions in the canonical chain, including recovery after restart between
the two writes. Neither path installs balances. After any certificate, the runtime
serves certificates but stops fresh signing until its payment epoch is installed.
Outcome persistence remains behind the source gate.
Selection-only readers reject a mixed chain rather than omit
its outcomes. Selection records retain format 1; outcome records use format 2.

Ordinary final promises and vote fences now use epoch-scoped v2 keys. The
storage handoff retains those records, closes the old context and reports old
final choices absent from the certified cut. A local minority vote is not
network finality and must not strand an observer after a valid certificate.
A signer still refuses to sign a cut that omits its own current-epoch final
choice. Selection and outcome signing check that their replay-derived payment context is
installed; recovering an undecided signer journal cannot seal a newer context.
A decided journal may relay its existing certificate after the handoff without
creating a signature or reopening old voting. An observer's excluded minority
promise does not prevent it from learning an authenticated quorum decision.
While the old context is installed, learning that decision fences ordinary
voting in the same transaction as the decision journal. It keeps the minority
promise for reconciliation rather than deleting it.

This storage operation is not certificate authentication or live installation.
Its caller must authenticate the old roster's quorum, reconcile local tails
without replacing cemented state, and install the replayed economy and next
electorate in the same transaction. The gated `PreparedInstallation` now does
this durable write for selection and outcome records. The source-gated running
installer coordinates the write with admission and cache publication.
Legacy v1 vote records require explicit reconstruction; no migration or deletion
is performed automatically.

`confirmed_replay/installation.rs` authenticates the stored chain from configured
genesis and prepares the immediate successor of the installed context. Its writer
rechecks the context and exact record bytes. It replaces account and economic
projections, pending transfers, lifecycle receipts, bootstrap state and the
confirmed authority projection. The same transaction records the agreed roster,
quorum and cutoff, then closes the previous payment epoch. A forced sync precedes
returning the rebuilt engines. Live admission now rejects timestamps before an
installed cutoff.

The store checks every cemented account position and queued confirmation before
replacement. Excluded unconfirmed blocks move to `checkpoint_orphan/v1/` with
their original bytes; final-vote promises stay in their old epoch. Obsolete undo
records and replay-owned lifecycle keys cannot leak into the new projection.
Unsupported tail lifecycles or populated legacy transaction/governance tables
abort the entire transaction. Bounded scans fail instead of installing partial
state. An installer failure after staging replacements also aborts those writes.

Tests install two selections followed by a fraud outcome, compare accounts and
pending balances with replay, retain signed nominal TRST balances, and reopen
the store without settling twice. They supply confirmed prefixes explicitly.
They do not test running-worker publication or payment electorate handoff.
The installer currently refuses any cut omitting locally cemented history,
including a lagging cut when later prefixes have already cemented. Catch-up must
resolve that ordering without weakening finality. Startup validates the installed
authority against replayed certificates. Worker coordination and cache/election
replacement now have the normal live journey's evidence. Roster-only checkpoints
remain open.

Mixed replay extracts each certified account cut, settles its outcome, then
executes the following signed blocks against that result. These blocks must name
the most recent checkpoint hash as their payment epoch and cannot predate the last
certified cut. A panel can resume across that boundary without being installed
twice. Stale contexts, invalid signatures and revoked spending fail replay.
Replay reconstructs and freezes each selection request at its certified cut,
before any beacon or verifier vote. Later endorsements cannot change that
request while randomness is unavailable. Freezing does not grant committee
authority without the authenticated beacon.
This supports later certificate validation, restart and authenticated history
transfer. The gated live installer performs the economic and authority handoff
before payments resume in the new epoch.
Record persistence does not modify live account snapshots, install economic
state or unseal ordinary voting. Historical transfer now authenticates both
record kinds through the same replay validator. The sync worker can load and
serve a mixed chain after restart. Height rotation reads both record kinds. The
beacon worker waits until the selection's payment epoch, or an authenticated
descendant, is installed. Certificate presence alone does not enable a committee.
The installation writer rechecks readiness in its transaction.

The mixed-history TCP fixture starts with locally stored signed history and two
selection certificates. It transfers the outcome and its next selection in
bounded chunks, restarts the receiver, and serves that chain to another receiver.
It leaves live account snapshots unchanged. A separate partial-history test
archives signed history outside the applied ledger only after authentication
and semantic replay. It does not install accounts or advance local finality.
Those component tests do not demonstrate full-node bootstrap across a live
economic outcome. The six-node journey above now supplies that separate evidence.

Signed-tail tests also transfer a certificate covering post-outcome payments and
reconstruct it after reopening LMDB. Fraud removes the tainted share once; the
clean remainder can be spent. Inactivity preserves the originated TRST. Pending
transfers store their revoked chunks when settlement happens, and delivery or
return preserves that split without applying old revocations again. These chunks
retain their origin after restoration. A clean remainder and a restored split
can share a merge origin without sharing the same composition. Delivery and
resend preserve those portions separately. Immediate merge edges retain a
fixed-size composition reference into shared revocation history; no token stores
a flattened burn ancestry. Each transfer still names one original burn or merge.
Holdings now use snapshot format `TRSTv003` and the graph uses `BRSTMG03`. The
pending-record layout also changed. Use fresh development data or explicit
reconstruction, not an older intermediate build's store.

An eight-store loopback TCP fixture exercises typed outcome agreement through
the single-height worker. Six of ten human units cannot decide; seven can. A late
signer with no clock authority receives the decision, and restart re-emits each
store's exact certificate. The fixture leaves account snapshots and the canonical
chain unchanged. It does not exercise live outcome installation or fresh-node
bootstrap of outcome history.

The height-rotation variant of that fixture starts without a supplied candidate.
It discovers the completed panel, obtains seven human units, persists the outcome
and serves a late observer with no key or clock. Reopening the stores preserves
the reduced roster and does not start a new signing epoch. Signed history is
preloaded, and live account snapshots remain unchanged.

Bootstrap authority ends once the verified population reaches its threshold.
The node commits that phase change with the admission or parameter activation
that caused it. Revocations or later threshold increases cannot reopen it.
Restart requires the saved phase record; missing or corrupt records stop startup
without resetting the ledger. Certified replay derives the phase from signed
history rather than trusting that local record.

An independent startup worker replays the durable certificate chain and fetches
missing beacons from the pinned drand chain. It requests the exact committed
round, checks BLS authentication, and attaches the proof durably. Wrong-round
responses and relay outages leave the same request pending. Older pending
decisions are included after restart, not just the latest checkpoint. Record
sync then carries attached proofs to peers. Behind the selection release gate,
the worker also freezes the certified request in the live orchestrator before
fetching its beacon, and installs the committee after the exact proof arrives.
Both changes commit the snapshot before publishing in-memory state. Ordinary
nodes still run without that installation path and reject verification votes.
The HTTP/restart regression exercises both modes using real signed historical
beacons served by a local fixture relay; it is not evidence of live distributed
proposal agreement or honest pre-beacon signing.

Work in progress after iteration 12: a confirmed authority projection now commits
with causal cementation and survives restart. Payment elections freeze those
weights rather than reading provisional account effects or reducing their
denominator to locally online representatives. This is a local-state correction,
not agreement on roster epochs. Votes and history import still need an agreed
checkpoint context; do not treat different locally confirmed cuts as the same
electorate. The selection gate remains closed.

A bounded confirmed-history reader, partial semantic replay and a durable local
cementation queue now exist. Replay reconstructs payments, creator bootstrap
verification, pending normal endorsements and verifier opt-in/out from the
configured genesis. It does not read stored economic snapshots or undo journals.
Overlapping normal endorsement and creator admission of the same target still
fail closed because their order is not committed. Initial certified verification
panels after bootstrap exit now replay from their accepted ancestor certificate
and exact beacon. Complete inconclusive panels can queue and replay certified
revotes while retaining prior stakes and excluding earlier verifiers. This
describes the initial replay work in iterations 32 through 35. Later iterations
add challenge outcomes, deadlines, pending expiry and governance boundaries.
The current remaining gaps are listed in `REFERENCE_IMPLEMENTATION_REVIEW.md`;
the selection gate remains closed.

## Implemented checks

### Signing time

The node has an in-process NTS client, separate from the SNTP offset shown in
diagnostics. `nts_sources` defaults to one endpoint each from Cloudflare, Netnod,
SIDN and Canonical. Each entry names an operator, TLS hostname and key-exchange
port. Configuring aliases of one operator as independent sources violates the
trust assumption; duplicate operator names and hostnames are rejected. An empty
list disables fresh checkpoint signing. It never authorizes a zero-error clock.

The provider requires three fresh authenticated observations, tolerates at most
one incorrect source, and keeps the hull of every possible consistent honest
subset. Each source bound includes the full observed round-trip time, advertised
root delay and dispersion, precision and an additional one-second cushion.
The model assumes honest sources' UTC error is covered by that bound and local
monotonic drift stays below one percent. Authentication does not prove those
physical-clock assumptions, operator independence or absence of common-mode
reference-clock failures. The result is not an unconditional mathematical proof
of UTC.

Observations expire after 90 seconds. Wall/monotonic divergence above 250 ms,
wide or contradictory intervals, leap warnings and fewer than three fresh
sources make the clock unavailable. Bounds age while replay and storage locks
run. A prepare certificate can still complete its already committed value after
the fresh-signing deadline; a local clock cannot authorize resampling it.

TLS uses normal certificate and hostname validation. A badly wrong initial host
date, blocked TCP 4460/UDP 123, or too many unavailable services therefore pauses
fresh signing. No insecure bootstrap or host-clock mutation is performed. The
read-only live probe is `cargo run -p burst-network --example nts_probe`.

Endpoint documentation: [Cloudflare](https://developers.cloudflare.com/time-services/nts/),
[Netnod](https://www.netnod.se/nts/network-time-security),
[SIDN](https://nts.time.nl/), and
[Canonical](https://ubuntu.com/server/docs/how-to/networking/chrony-client/).

### Certificates and replay

`consensus/src/checkpoint.rs` validates checkpoint certificates against the
previous checkpoint's human roster. Each human contributes one unit to their
representative. The default quorum is 67%, rounded up; the supported range is
6667–10000 basis points. No current-online sample enters that calculation.

Quorum intersection assumes less than one third of that weight can equivocate.
One representative key must use one durable signing authority. Running copies of
that key with independent reservation databases can produce conflicting votes;
local disk reservations do not coordinate cloned signers. The independent dev
cluster now uses three public fixture keys, one per signer. It still does not
demonstrate Byzantine tolerance or agreement across roster transitions.

Checkpoint certificates now carry a round. Their signatures authenticate the
precommit phase, round and checkpoint hash, which binds the network, genesis,
parent, height, cutoff, ledger commitment, parameter commitment, next roster,
next quorum and selection commitment. Prevotes and signatures from different
rounds cannot be combined into a decision. Duplicate signers, unknown signers
and invalid signatures fail validation. Advancing the
anchor requires a matching next-roster commitment. The caller must derive that
roster by replaying the certified ledger cut. A signed root alone does not check
state transitions or prove data availability.

`consensus/src/checkpoint_round.rs` implements proposal, prevote and precommit
transitions using the lock/valid rules in
[Tendermint Algorithm 1](https://arxiv.org/html/1807.04938v3).
Timeouts preserve locks. A later proposal can change a locked choice only with
the required prepare quorum evidence. Prepare proofs received after a local
nil precommit can be used for recovery without issuing a second precommit.
Proposer rotation and all thresholds use the parent's human roster.

`node/src/checkpoint_agreement.rs` serializes each signer's transitions through
an LMDB writer and forces the commit to disk before returning signatures.
Records include locks, prepare proofs, local proposal/vote choices, their exact
replay manifests and a decision when one exists. Their keys include network, genesis, representative
and height, but not parent. Snapshots have bounded decoding and a checksum.
Version 2 checksums cover state and manifests together. Recovery replays every
retained manifest against its certified parent and signed ledger history before
retransmitting saved choices. Both a lock and a different prepared choice remain
available across rounds; unused manifests are pruned. Missing history or missing
replay input stops recovery without overwriting reservations. Block rollback
cannot modify these records. Version 1 requires explicit recovery rather than
inventing the manifests behind saved roots.

The old single-choice reservation remains readable for explicit recovery.
A new round machine refuses to initialize over it. Neither old certificates
nor old signer records are silently reinterpreted. These formats were not
enabled in the running checkpoint protocol.

The public durable signing API requires a replay-constructed candidate. The
selection-only convenience API accepts `ReplayedCheckpoint`; the mixed API also
accepts outcome and roster candidates. Selection replay derives
the checkpoint's ledger root, request, parameter hash and human roster
from authenticated confirmed history. The callback implementation is private
to the signing module and remains available to its round-machine tests.
Subsequent cuts extend the previously certified manifest and retain earlier
frozen endorsement sets. They can select a different pending target but cannot
resample an existing attempt. Supported normal verification outcomes replay
through the same settlement code as live candidates. Each vote requires its
certificate's full history; target spending and reward claims require the full
selected panel. Missing proofs, wrong contexts and duplicate voters fail replay.
Block version 4 signs complete vote sets for later verifier spending of returned
stake. The five-node fixture rejects spending that credit without proof and
replays the proof-bearing payment after restart. Signed challenges and complete
panels now replay, including upheld settlement at a certified boundary. These
initial fixtures did not cover agreed deadline releases, timeout-driven revotes
or governance. Later fixtures and their remaining limits are recorded below.
Proposal gossip and round scheduling now have
a running-node worker; ordinary nodes use its observer path. Active scheduling
has TCP runtime tests, but production signing and payment-roster handoff remain
gated. Certificate-record transfer alone does not establish proposal agreement
or open the selection gate.

`verification/src/selection_request.rs` freezes one request's target, triggering
block, attempt, cutoff, parameter hash, complete eligible pool, committee size and
drand round. Version 1 commits one request per checkpoint selection root. The
approved mainnet chain is pinned. The round is the first one strictly after the
cutoff. Selection checks the quorum certificate and full BLS proof, then applies
decision 19.2's hash-and-sort algorithm to the frozen pool. It never filters the
pool using the receiver's current clock or account state.

Before signing, representatives must check the request against replayed state
and reject it if their conservative clock-error bound reaches the beacon release
time. A certificate is not a cryptographic timestamp. Its timing assurance relies
on honest signers enforcing that rule and on drand's unpredictability assumptions.

Address decoding now rejects nonzero base32 padding. Otherwise several address
strings could represent the same public key and evade identity deduplication.
Signature verification uses Ed25519 strict verification to reject weak-key
forgeries. The [library's security notes](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/#weak-key-forgery-and-verify_strict)
explain the distinction from ordinary verification.

## Runtime behavior during integration

The old node path fetched the latest beacon separately on each node and selected
from provisional accounts at local time. That path has been removed.
Endorsements enter the orchestrator and its snapshot in the same LMDB commit as
their block and burn. Genesis verification also commits the target account and
BRN accrual state there. Reaching the normal threshold leaves the request pending.
The gated beacon worker now connects accepted selection certificates to
that state. It checks local finality, parameter identity, bootstrap exit and the
exact certified lifecycle history before freezing or installing. A revote's
history binds prior choices, stakes, timestamps, contexts and excluded verifiers,
not just the endorsements. An older panel is skipped only when all its selected
verifiers have signed votes bound to that context, or a later certified lifecycle
replays their completed outcome and a new signed challenge admission. Normal
verification and each challenge have separate replay-derived lifecycle identities;
their round counters can each begin at zero. A certificate for an unrelated
lifecycle is rejected. While an admitted challenge awaits its replacement
certificate, the worker defers the old panel without stopping. That deferral
does not certify an outcome or authorize a new committee. The check and
installation share the publication lock so admission cannot race between them.
A late local
endorsement outside the certified cut is an explicit conflict, not a reason to
silently change the frozen rewards or pool. Agreed handling of that conflict
still requires lifecycle ordering; the worker does not discard signed burns.
Gated verification outcomes and challenge effects have a transactional candidate path, including
stake receipts and reward claims backed by an actual outcome block. Complete
normal panels have confirmation and replay dependencies. Signed pending
challenges now replay with their stake locked and the target's transaction rights
unchanged. Their first committee binds the signed admission. Incomplete panels
replay votes and locked stakes without creating rewards or releasing stake.
Complete panels that reject the challenge replay stake forfeiture, voter refunds
and minority-funded rewards without changing the target's rights. Complete upheld
panels settle at a later certified boundary; incomplete panels still need agreed
timeout cutoffs. Replay refuses a cut
that crosses an unresolved challenge's deadline rather than inventing a release.
The candidate path
requires certified request/attempt binding, described below. It is not yet
an enabled end-to-end live verification protocol.

The orchestrator can freeze a replay-derived request and install its committee
only after verifying the parent-anchored certificate and exact drand proof.
It cannot replace a frozen pool with local randomness or accept more endorsements
into that frozen round. Retrying installation preserves votes already cast.
Snapshot version 5 retains the request, certificate, beacon and stake-attempt
identities. Version-4 snapshots preserve their existing certification but do not
gain fabricated stake identities. Older snapshots retain their existing state
without acquiring certified status. An outcome with an unscoped stake cannot
settle money; signed history replay is required.

Verification votes sign a context digest in their existing `origin` field. The
digest binds the checkpoint hash and request commitment, including the network,
genesis, target, trigger, attempt, pool, selection parameters and beacon round.
The transaction separately names the active governed parameter hash and locks
the current verifier stake. A rule change cannot resample that committee or
rewrite stakes already paid. A wrong context, stale transaction parameter hash
or timestamp before beacon release is rejected before locking a stake.
A revote or challenge clears the earlier
round's context. The context-free component API cannot vote into a certified
committee. The wallet builder and verification-status RPC carry the same context.
Stake receipts include this context in their keys; challenger receipts instead
bind the signed challenge block hash. Outcomes retain earlier revote bindings,
and a settled receipt cannot be replaced or reused by a later attempt.
The derived selection-history digest also binds the active challenge's signed
block hash, challenger, target, stake, reason and initiation time. A challenge's
first request must use that block hash as its trigger. This distinguishes an
initial verification from a challenge, and successive challenges from each
other, even though each begins at round zero. Legacy challenges without signed
bindings cannot acquire a certified selection. Both live installation and
historical replay compare the participant history against the certified cut
before freezing a request. Installation and vote admission recheck challenge
binding for restored requests, even if they were already frozen or certified.
The digest alone does not order challenge outcomes against payments. The mixed
certificate chain and installed payment epochs now supply that boundary for
complete upheld panels. They do not authorize local timeout settlement.
The digest is reconstructed from history, not stored in the checkpoint wire
record; this change does not alter block, snapshot or wire encoding.
Vote-context binding originally used an unused field in block version 3.
The current block version is 6 and the wire generation is 26. Checkpoint transfers
carry authenticated history alongside the record. Version 5 added the signed
payment epoch alongside returned-stake dependencies; version 6 also carries scoped
key operations. Ordinary blocks must match the
installed context; genesis uses zero to avoid a self-referential hash. Admission,
the economic writer and durable vote reservation check the context. RPC exposes
it to wallet construction. This does not install an outcome or unseal voting.
Older peers are rejected during the handshake. Older block-format ledgers require
a separate fresh development directory. No automatic migration is provided.

First-cut replay derives the trigger and complete eligible pool within the
named cut. Eligibility requires signed opt-in, verified status, the configured
verification age, 50 available BRN and a completed TRST payment involving a
distinct verified wallet. Burns paid to another wallet count once received.
Self-transfers, refunds, unclaimed sends and activity only with unverified
wallets do not count. This applies decision 15.5's real-transaction requirement;
it does not prove that a payment represents real-world trade. The separate vote
stake is checked when voting, not substituted for the 50-BRN eligibility floor.
Target, endorser, prior-panel and penalty exclusions stay in the orchestrator.

`checkpoint_store` persists certificates and canonical manifests before
their beacons are available. Import and restart replay the chain from configured
genesis. Each next certificate uses the preceding replayed human roster.
Only the certificate's exact, BLS-verified round can fill its pending beacon;
retry does not change the request. The record is protected from block rollback.
Storage is bounded to 128 checkpoints, 16 MiB per record and 64 MiB for the chain;
exceeding those bounds fails rather than truncating history. Delayed beacon
space is reserved before accepting a certificate. The store itself does not
schedule requests; the gated runtime and beacon worker perform scheduling and
committee installation. In-flight parameter transitions, complete lifecycle
replay and checkpoint-history compaction remain unfinished.

The gated chain also accepts governance ballot records. Their roots bind every
proposal due at the exact counting cut, with authority from the previous human
roster. Replay freezes each effective result before later delegation changes.
The installer writes and publishes governance, delegation and constitution
state with the economic/payment-epoch handoff. Ballot receipts now freeze the
activation schedule; cuts apply simultaneous approved changes in proposal-hash
order and bind the resulting rules. Signed-history tests cover journal recovery,
restart, governed payments and archival transfer. The gated runtime now discovers
due cuts automatically. A three-human native-node test covers delegated voting,
scheduled rate activation, governed payments, restart and fresh-node catch-up.
Certified admission requires the installed parameter hash. Installed schedules
now fence ordinary votes whose signed timestamps exceed the next certified
activation. A separate schedule derived from cemented proposal support fences
the counting boundary before certification. Provisional proposals cannot publish
that limit, and provisional withdrawals cannot lift it. A final support vote
now reserves its deadline atomically against the signer's earlier final
timestamps. Supported proposals that fail the threshold or are withdrawn close
through certified terminal receipts; they cannot activate a change. Submitted
proposals now retain their original proposal/voting windows and counting buffer.
Later duration changes apply to new proposals. Elapsed proposal windows close
under the old qualification threshold before new rules activate. Other in-flight
rule changes and partition recovery remain open. Governance cuts now settle
coincident panels before counting, apply approved changes, then settle pending
expiry under the resulting rules. Earlier due effects cannot be skipped.
Expiry returns also leave a one-time signed-balance credit receipt for their
sender. Receiving that receipt does not redeliver the tokens. Revived tokens
remain subject to normal origin and revocation checks when spent.
Certified panel activation retains its rule snapshot to validate the original
deadline after later timeout changes. Counting uses current rules and settlement
checks original stake receipts. The running-node journey for an active panel
passes BRN-rate, stake, timeout, minimum-age and Neither-cooldown changes.
It checks future verifier eligibility without disqualifying the selected panel,
the governed penalty duration, deadline settlement, returned-stake spending,
restart and fresh-node recovery. Other in-flight interactions and
partition cases remain open.
See reference-review iterations 105 through 130. The wire version is 27;
version 26 added governable verifier minimum age and Neither-vote penalty cooldown.
Version 27 reserves a nonzero Challenge `target_head` for the signed outcome
being appealed. Ordinary challenges keep a zero reference. Certified appeal
admission and settlement are integrated behind `META_CHALLENGES_ENABLED=false`.
Signed-history replay checks restoration using the old electorate, reward-origin
correction and fresh-store transfer. One connected fraud appeal passes live
settlement, cooldown and expiry ordering, a new payment, all-node restart and
fresh-observer catch-up. Repeated, failed and partitioned live appeals remain
unverified; the separate appeal gate and broader selection gate stay closed.
The wallet can look up the latest certified outcome and review its governed
stake before signing. This client path has connected live recovery evidence;
native desktop appeal signing remains unverified. The RPC capability stays
false in ordinary builds while either release gate is closed.
Verification snapshots use version 6 to retain appeal panel context; the reader
still accepts version 5 and older supported snapshots. No running app or ledger
was upgraded by this source change.
Existing governance parameter discriminants remain unchanged; older peers
cannot join this wire generation. No existing ledger is automatically migrated.
Governance-cut commitment v5 includes terminal constitutional conflicts.
Normal deadline commitment and the confirmed-governance index are v2. Pending
format is 2 and block format is v6. Scoped grants, revocations and secondary
ballots share a separate closed gate. A ballot's historical grant authenticates
admission; its effective authority is resolved from the certified counting cut.
This avoids making independent-chain revocation arrival order decide ballot
block validity. Current proposal scope overrides parameter scope, including
revocation tombstones. Per-owner/scope records replace the whole-registry journal.
Owner/recipient discovery indexes are replay-derived and read in one storage
snapshot. Scoped wallet controls and native signing commands are connected.
Reference-review iterations 119 through 121 record native desktop delegation,
constitutional and parameter proposal journeys through certified activation and
recovery. Partitioned delegation traffic remains unverified.
Older block-format ledgers are rejected without migration or deletion.

`checkpoint_sync` transfers one requested record and its signed history at a
time in 16-KiB chunks, with a 64-MiB total transfer bound.
The normal authenticated peer reader routes these through a bounded inbox.
Reassembly checks peer, height, offset, total size and transfer digest. Import
verifies the certificate, parent ancestry, optional BLS beacon and supplied cut.
Structural validation and semantic replay run before any block is admitted.
A matching certificate grants an age-check exception for its exact block hashes;
it does not authorize an arbitrary old block or install a peer's account state.
Normal bootstrap still checks signatures, work, dependencies, forks and economics.
Missing local blocks defer durable record import until that prefix is cemented.
Older records are revisited to pick up delayed beacons. The service bounds
downloads, export caching, queue depth, serving rates and transfer duration;
stalled peers are rotated. It does not treat a transport digest as a certificate.
Original genesis parameters are pinned separately from active governed rules.
An older ledger can acquire this pin only from inputs matching its actual genesis.

The six-node recovery fixture now reconstructs the rejected-challenge settlement
after all five existing nodes restart and a fresh sixth observer joins. The
earliest blocks are more than eight hours behind its controlled clock. This
tests the bounded historical admission path; it does not establish snapshot
installation, pruning or recovery of unsupported lifecycle transitions.

New verification votes and challenges are rejected before locking stakes.
Legacy challenge expiry penalties are paused because this build cannot assign
certified committees or accept their votes. No stored votes or challenges are
deleted. Restored legacy committees are not treated as certified.

The test-only legacy expiry helper returns half the challenger stake and unlocks cast-vote
stakes without rewards. Its component tests check that arithmetic and atomicity;
they do not establish the intended timeout policy. Decision 15.16 explicitly
defaults absent selected verifiers to Neither. Normal committee activation now
has a certified start and deadline in the gated runtime. The activation record
carries its exact beacon proof, so recovery does not depend on a previous relay.
Fresh signing rejects stale or uncertain activation times. Votes already cast
by that committee remain valid; activation cannot reset its deadline.

Normal deadline completion now has a typed checkpoint in the gated runtime.
It batches due panels, records absent members separately from signed votes and
installs settlement with the payment epoch. Five focused replay/settlement tests
pass, including timeout-driven revotes and historical transfer. A four-node live
test also passes for partial-panel completion, returned-stake spending, restart
and fresh-observer recovery. Challenge activation and completion now use the
same certified event path. Their window uses the separately governable challenge
duration. Four node integration tests pass for terminal settlement, revotes,
restart, historical transfer, late-vote rejection and invalid windows. The live
challenge deadline journey passes with activation before voting, including
refund spending, restart and fresh-node recovery. Earlier runs stalled with a
frozen clock and a confirmed vote after the candidate cut. Unsigned proposals
now replay a later safe cutoff without waiting for the original start to expire.
A separate live journey confirms a vote before activation, then passes deadline
settlement, refund spending, restart and fresh-node recovery. Signed proposals,
prepared values and ordinary final-vote fences remain protected. Adversarial
handoff liveness is still unverified. The local half-stake expiry sweep has been
removed from the running node.
If a panel finishes before activation, an unsigned candidate can now yield to
its replayed outcome, revote selection or roster update. The expanded upheld
challenge journey also claims the challenger and correct-verifier rewards,
spends their returned BRN and recovers those effects after restart and on a
fresh observer. This does not establish re-verification or adversarial handoff.
Ordinary replay rejects crossing an incomplete activated panel's deadline
without its completion certificate. Do not promote the legacy sweep merely
because its storage tests pass.

`verification_status` exposes `selection_available: false` and
`selection_unavailable_reason: "certified_checkpoint_selection_not_integrated"`.
`CHECKPOINT_SELECTION_ENABLED` is a source-level release gate, not an operator
option to bypass certificate checks. Changing it alone does not integrate the
protocol.

These are source changes. Existing running apps and Azure deployments have not
been upgraded by this pass.

RPC submissions now wait up to two seconds for a tracked admission result.
Known rejections return their reason instead of making the wallet wait for
confirmation. Receipts use the complete serialized payload, not just its declared
block hash. The in-memory tracker holds at most 4096 entries. Missing, evicted or
late results remain pending; accepted blocks still require consensus confirmation.
This does not provide durable submission receipts after a node restart.

## Validation evidence

The final iteration 22 workspace run passed 1,440 tests with zero failures and
one existing ignored documentation example. Strict all-target Clippy passed.
Logs: `target/validation-evidence/certified-selection-workspace-final.log` and
`certified-selection-clippy-final.log`. The conformance release gate still fails
because implementation and review gaps remain.

Iteration 22's rebuilt-daemon run passed with independent fixture signers,
hard-crash recovery, a post-restart payment and fresh-node catch-up. See
`target/validation-evidence/certified-selection-release-network.log` and
`burst-validation.NSo9hTWr/`. The expanded three-node replay test also passed
with pending normal endorsements, including a second run inside the workspace
suite. Neither run enabled certified selection.

`cargo test --workspace --locked --offline --quiet` passed after the bootstrap
replay change: 1,429 tests passed and one documentation example remains ignored.
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` also
passed. Logs are in `target/validation-evidence/bootstrap-replay-final-workspace.log`
and `bootstrap-replay-final-clippy.log`. These tests do not open the selection gate.

The checkpoint tests cover duplicate and unknown signers, altered commitments,
roster replacement, fixed quorum during partitions and restart-safe reservations.
A combined LMDB/signature test shows that one equivocating representative among
four cannot certify conflicting checkpoints when the overlapping honest signer
keeps its reservation across restart. Selection tests cover real mainnet BLS
proofs, changed request inputs, wrong rounds and missed commitment deadlines.
These are component tests, not a running distributed checkpoint network.

An earlier disposable multi-node validation passed with evidence in
`/tmp/burst-validation.kgAjq1Vh`. It includes signed verification-vote and challenge
rejections on every node with unchanged balances, stakes and frontiers, plus
payments, merges, delegation, hard-crash recovery, a post-restart payment and a
fresh fourth node's catch-up. An earlier run exposed the queue-acknowledgment bug;
its logs remain in `/tmp/burst-validation.c2wrGjj8`.

The rebuilt daemon passed the independent three-signer run after iterations
16–17, including hard restart and fresh observer catch-up. Current evidence is
`target/validation-evidence/checkpoint-round-network.log` and
`target/validation-evidence/burst-validation.pMRPi6Hx/`. Certified selection
remained disabled throughout; this run does not sign it off.

Iteration 19 fixes a reproduced bootstrap ordering failure with a signed
endorsement `target_head`. The creator's endorsement now carries the recipient's
earlier signed receive/delegation history as a causal prerequisite. Block version
3 and wire version 4 require updated nodes and a fresh development ledger. The
final independent-signer run passed at `target/validation-evidence/burst-validation.zB5VrMG9/`;
the separate faucet/restart/fresh-observer regression also passed. Neither run
supplies agreed roster epochs or opens certified selection.

## Required before enabling selection

1. Build a replayable, confirmed ledger cut with causal dependencies and all
   verification, penalty, delegation and governance effects. The current account
   store contains provisional effects. Reading it under a mutex does not create
   an agreed snapshot. Governance eviction handling must be explicit in roster
   derivation; nodes must not silently omit offline humans.
2. Retain proposal agreement, bounded transport, authenticated history transfer,
   certificate persistence and catch-up from the configured genesis anchor.
   These paths now have running-node evidence for verification, outcomes and
   roster changes. Complete their adversarial validation; ordinary daemon signing
   remains disabled. A peer-supplied roster is not a trust anchor.
3. Validate the amended missed-deadline recovery rule against adversarial
   scheduling. The service now retries expired, unprepared proposals at unsigned
   starts in later consensus rounds. Fresh cuts require replay and pre-beacon
   votes; refresh never clears a lock or replaces a signed current-round proposal.
   Delayed prepare proofs retain their original checkpoint through restart.
   Independent-store and TCP service tests pass with controlled clocks. Neither
   a timeout nor those tests prove absence of a hidden prepare certificate or
   unconditional resistance to selection bias. See `spec/DEADLINE_RECOVERY.md`.
4. Atomically persist request admission, endorsement threshold, pinned selection
   inputs, certificate and lifecycle effects. Resume exact-round relay retries
   after restart. Complete normal panels now schedule certified revotes through
   this path. Pending challenges also obtain their own certified committee;
   rejected challenges also replay their signed settlements. Upheld challenge
   outcomes now install at a certified cut. Timeout-driven transitions remain open.
5. Preserve the connected frozen-committee installation and context-bound vote
   path. Derive each trigger and complete eligible
   pool from replay, and carry the full panel dependencies into cementation and
   history import. Keep endorsement burns, verifier stakes, Neither rules,
   challenge outcomes and proportional revocation intact.
   Outcome-producing votes and reward claims now carry local confirmation
   prerequisites for the complete signed outcome. The first account block after
   successful verification also binds its identity source; later blocks inherit
   that edge through their account chain. Normal returned-stake spending now
   signs its complete vote-set dependencies. Lifecycle fork replacement,
   agreed deadline releases and re-verification
   ordering and distributed identity activation remain unverified; these local
   records are not peer certificates. A local-only prerequisite needs its own
   durable consensus decision or prior cementation; the dependent block's quorum
   does not authorize an independent fork choice absent a signed reference.
6. Test independent node arrival orders, missing dependencies, partitions,
   conflicting proposals, outage recovery, hard crashes and certificate catch-up
   through the actual wire path. Library tests and genesis-authority verification
   do not demonstrate this lifecycle.

The gated runtime now installs agreed roster epochs and uses them for payment
finality. Its partition and conflicting-handoff behavior still needs broader
network evidence. Certificate validation establishes signatures and weight under
the previous trusted roster, not whether a human-verification decision is true.

The subsequent [reference implementation iterations](REFERENCE_IMPLEMENTATION_REVIEW.md)
unify payment-election roots and add transactional causal cementation of account
predecessors and receive sources. They do not yet supply the complete confirmed
lifecycle snapshot required by step 1. Endorsement admission, genesis verification,
governance/delegation block effects and pending-transfer expiry now commit with
their backing economic state. Iteration 15 adds transactional gated lifecycle
effects; their confirmed replay and distributed authorization remain incomplete.

## Outcome/payment handoff and remaining release checks

`Election::try_confirm` already requires final votes. Soft votes alone cannot
cement a payment. Owner-signed block hashes now include the payment epoch, so
`vote_signing_payload` also binds that epoch through the block hash.
`reserve_consensus_vote` durably locks a final
choice by epoch, representative, account and predecessor. The gated outcome signer
checks those reservations and commits a per-signer vote fence in the same LMDB
transaction as its checkpoint journal. Every reserved final hash must appear in
the candidate's replayed history. The ordinary signer checks this fence before
reserving another sequence, for both soft and final votes. Restart, round changes
and a different candidate at the same parent cannot remove the fence.

An unresolved final reservation outside the cut prevents this signer from
approving it. An observer can learn an authenticated certificate that excludes
its minority promise. Installation preserves that promise, closes the old epoch,
and installs the certified roster for the next epoch. It never clears the old
fence or discards network-final history. Conflicting reservations can still
prevent a signing quorum; no general liveness claim is made. Pending challenges alone do not create a fence;
this path requires replay of a complete upheld panel and remains release-gated.

The storage installer checks every cemented position and durable confirmation
handoff before replacing the ledger. A locally read head set is insufficient.
Old signatures cannot spend against a new revocation state or a different roster.
Elapsed time cannot clear a signing reservation.

The canonical record loader and historical-transfer validator now replay mixed
selection/outcome ancestry, apply each outcome at its certified cut in a read-only
candidate, and derive subsequent authority from that result. The sync worker can
recover and serve this sequence. The runtime can now discover and recover outcome
decisions; the beacon worker waits for installed state across their boundary.
The gated block-processing worker now installs that state. An outcome is not another
drand request. Restart and catch-up must recover the installed economic epoch
before accepting post-outcome payments. Test concurrent
payment/outcome decisions, partitions, crashes during the handoff and fresh-node
reconstruction before opening the gate. The local fence does not implement those
remaining requirements, even though single-height outcome agreement passes over TCP.

In certified mode, payment quorum and checkpoint quorum use the same agreed
electorate. Installing a new epoch retires old elections and candidate routes.
Local confirmation cannot change the installed roster. The gated runtime now
proposes roster-only updates when no selection or outcome is pending. A separate
event commitment and format-3 record distinguish them from committee requests.
Validators replay the cut and require a changed human roster; the previous
roster's quorum approves it. No beacon is requested. The same signer journal,
payment fences, history transfer and coordinated installer apply.

`roster-tests-2.log` records the initial two-node live pass, including a delegation
update, old-epoch rejection, a new-epoch payment, restart and a fresh observer.
The latest extension also admits roster updates before bootstrap exit so genesis
dilution need not wait for that threshold. Its final bootstrap and mixed-journey
regression results are tracked in iteration 79 of the reference review. Roster
updates do not resolve agreed timeouts, re-verification or governance ordering.
