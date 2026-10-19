# Completion ledger

Current plan: [COMPLETION_PLAN.md](COMPLETION_PLAN.md). Updated 2026-10-19.
Active working goal: [one qualified public-testnet release](TESTNET_PILOT.md#working-goal-and-finish-line).
The user requested this goal refinement on 2026-10-19. Finish the identified
candidate's P1-P5 blockers, reuse applicable evidence and deliver the packages
with measured operating limits. The saved active goal now matches this public
testnet scope, as confirmed by reading it back. This refinement does not mark
any requirement complete or open a release gate.
P1-P5 have retained passing stages, summarized below; their labels are not orders
to rerun whole journeys. Software reconciliation for the selected Linux public
testnet is complete. The final pre-VPS handoff is identified below. Actual VPS,
bootstrap/contact and publication checks remain. The full whitepaper completion
plan remains the roadmap. Mainnet gates stay closed.
Requirement sign-offs below identify the evidence reviewed. The full release
remains blocked. Historical evidence keeps its original scope and failures.
Update this handoff rather than appending another iteration narrative.

## Current candidate status

### Completed pre-VPS software handoff

The selected Linux public-testnet software handoff is complete. Final archive:
`target/release-candidates/burst-testnet-linux-amd64-v7-pre-vps.tar.gz`.
SHA-256 `bdf2dabd76396dc4cb75d17ae5c31117b7a4aee8bc76e95cb69ba19e8e94437c`
matches the validation host and the desktop's resource-limited checksum run.
The inner checksum manifest is
`ef370bad680c2df807b29d44c73f8fa8ee8b15988118cf2c7bd47f3ce7b62b75`.
Assembly and archive receipts are respectively
`49e80211b999e450d3d6355d9c253183489005700bda3751012e976fd4a8e8fb` and
`968e37246fb99b4efff9741ed13821335ba93fa8422512edd3bbff3861b6e0b6`.
All new jobs are terminal. No VM was created and no publication occurred.

The manifest records software qualification for this scope while keeping
`publicationApproved: false`. Remaining work needs the owner's actual VPS:
installation/private administration, reachable bootstrap, independent participant
join/payment/restart, verified contacts and trusted publication. The full
whitepaper scale/platform roadmap remains separate and is not marked complete.

The owner requested completion of all software work before their VPS is ready.
The selected Linux public-testnet candidate remains fixed. No mainnet claim or
new Azure deployment is part of this work.

| Requirement | Status | Evidence |
| --- | --- | --- |
| D-31.9 panel votes | Complete for the selected installed workflow. | Receipt `4d35205c...` proves review, disconnected signing, original bytes, one accepted submission, two app restarts and exact panel/stake effects, then certified outcome and node recovery. |
| D-31.9 initial challenge | Complete for the installed workflow using retained two-node setup. Original failed fixtures remain retained. | Receipt `b2b24e04...` proves the three phases and exact challenge stake/target effects. No quorum or production change. |
| P5 final handoff | Assembly, archive and local checksum passed in `pre-vps-release-bundle-20261019/`. | Binary/source identities unchanged; updated qualification and operator instructions. Final archive and receipt hashes are above. Publication remains false until VPS/contact checks pass. |

Earlier P1-P5 and source-impact reviews remain evidence for their recorded scope.
Do not turn unreviewed heading counts into missing features, or promote those
records into mainnet, indefinite-scale or unsupported-platform qualification.
The next broad check is the final bundle, not another full protocol suite.

D-31.9 audit: the offline TRST transfer, manual combination and payment-return
paths were supported by signing primitives but missing from wallet reviews.
They now use the shared action context/journal and existing review dialogs.
Origin selection and exact merge-selection validation are shared with connected
commands, rather than maintained as separate offline rules. This batch is now
included in the installed TRST wallet and assembled candidate described below.

VM1 `offline-trst-core-20261019/` passed two focused selection/signing/journal
tests; `offline-trst-native-20261019/` compiled the native bridge. In
`offline-trst-ui-20261019/`, native runtime and typecheck passed. Five of six
browser checks passed; the new merge case incorrectly searched for a desktop
navigation link in a narrow-screen fixture. Its existing online counterpart
uses direct portfolio navigation. The corrected setup passed the single affected
case in `offline-trst-merge-ui-20261019/` (3.30 seconds), preserving the five passes
and both earlier stages. Frontend assets passed in `offline-trst-package-20261019/`.
No daemon/dashboard dependency changed.

Installed TRST acceptance is complete for transfer, active combination and
payment return. Each used the existing native offline driver against the
unchanged release daemon in disposable Dev mode: review/cancel, disconnected
signing with no RPC, a restart, one accepted submission with its response lost,
discard refusal, a second restart and confirmation of the original hash without
resubmission. Archived journal bytes match exactly. Positive ledger assertions
verify transfer amount and original age/origin, merged total/oldest age/ancestry,
and the rejected payment's sender return credit. Receipts on VM1:

| Scope | Evidence directory | Receipt SHA-256 |
| --- | --- | --- |
| Transfer | `offline-trst-transfer-runtime-20261019` | `0050032894d49f7884b2514121486afcfef5d130b7696eede6e2c0ec61b616b5` |
| Combination | `offline-trst-merge-runtime-20261019` | `48be5f86f309fdbef2a84aabbb68f1185caba6239b85154c720660d632a87607` |
| Payment return | `offline-trst-reject-runtime-20261019` | `312188b4c20da0e6ab1fe141dd38183a89a8fe9417d73147a3bf5d3f737189d5` |

Build and exact installation passed in `offline-trst-package-20261019/` and
`offline-trst-installation-20261019/`. Debian SHA-256:
`9d0cd5aa7a81401ed46c6d8a8aaee4e5aa23dccb642fc62fa1e53d24a80fcdb6`;
installed executable:
`3b8e653491cee05ed1f0a10be87ff592f6bbad0968068fcf5ba15b44630f8afa`.
Assembly passed in `offline-trst-release-bundle-20261019/`; its checksum manifest
is `de19a21e8cc5a052dc0edcb7d3b8439d4e78aafaa27bf31d8c0fe4a18f762123`.
All those units are terminal success/MainPID 0. Prior node/dashboard and ordinary
identity/governance evidence remains scoped to its unchanged dependencies.
This closes the TRST offline integration gap, not all D-31.9 action qualification
or final public-release sign-off. No mainnet or public deployment claim.

The assembled archive is now local at
`target/release-candidates/burst-testnet-linux-amd64-v7-offline-trst-candidate.tar.gz`.
Its SHA-256 matches VM1:
`ff9ee015fd5c9d3a01d9807fcaada49c5e5670df6f17fcfeb8294090a08da40d`.
The older candidate archives are retained. The new manifest still explicitly
sets `publicationApproved: false`.

The first D-31.9 appeal attempt, `offline-appeal-20261019/`, is terminal failed
(127.78 seconds). Installed offline review/sign/save/submission passed, followed
by replayed admission. The later readback in `post_challenge_recovery::capture`
called signing-authority-enforcing `signing_state` for an account now controlled
by its enrolled desktop device. This is a fixture read-only observation, not a
request to sign. Its capture and frontier checks now use `inspect_signing_state`;
the production device guard is unchanged. The fixture's generic success line
mentions double-click protection, but that assertion belongs to its connected
branch, not the new offline branch. Do not infer it from that log line.

The corrected node test executable compiled in 69 seconds under 1 CPU/3 GiB,
zero swap (`offline-appeal-build-20261019/`). The affected appeal then passed in
156.11 seconds (`offline-appeal-readonly-20261019/`), including certified judgment,
restarted-node comparisons and a fresh observer. Receipt SHA-256:
`1259cdbc90b2847187cd29dde9b05021f4378543cdd1380aa668aa6a1e88a4ed`.
Both units are terminal success/MainPID 0. This is installed-wallet plus
source-validation-node evidence; no new release-daemon qualification is claimed.
The failed original remains retained. Release daemon and wallet were reused.

The first participation setup failed before signing because it assumed a fresh
Dev founder was already opted in. `offline-participation-corrected-20261019/`
now passes offline opt-in, two restarts and lost-response recovery, followed by
a separately reviewed opt-out with exact state and unchanged BRN commitments.
The original failed setup is retained in
`offline-identity-participation-runtime-20261019/`; no product change was needed.
Participation receipt SHA-256 is
`81784d3db104634dc032a3dabe97995dcebde49532b37040b9b65232a4b9d2e7`.
The initial-challenge variant failed in recovery before the expected lost-response
state (`offline-identity-challenge-runtime-20261019/`). Preparation, disconnected
signing and journal retention passed. No attempt marker exists, so the failure
preceded process submission. Read-only inspection of a copy of its retained
ledger (`offline-challenge-inspect-20261019/`) found no saved block or successor,
zero new stake and a still-verified target. Its current payment epoch differs
from the saved block's epoch. That is consistent with the intended stale-context
guard; the original UI error was not captured, so do not claim the exact original
error is proven. No signature or submission was repeated during inspection.
Use a settled certified fixture for positive challenge/panel-vote qualification;
do not weaken epoch checks or rebuild the release binary to force this test.

`offline-scoped-20261019/` passed in 51.81 seconds using the existing certified
scoped-delegation fixture: both scope types, two secondary ballots and revocation
through offline controls, with stale-ballot refusal and counting/recovery.
Receipt SHA-256 `d760fc0e247da5ff9be747d7798d9841c3dfde44947992a79f19179072fb5ce4`.
The unit is terminal; no build or product change was needed.

The existing certified challenge fixture now has an opt-in SDK offline path:
live preparation, serialized public context, pure signing, exact durable journal
readback and unchanged head before submission. `offline-panel-build-20261019/`
compiled in 68 seconds. `offline-panel-20261019/` failed at panel preparation
after the initial challenge was confirmed and its committee activated. The build
omitted `private-testnet`: `checkpoint_selection_enabled(Dev)` therefore returns
false and the SDK correctly refuses unavailable panels. This is a fixture
capability mismatch, not grounds to weaken the guard. The failed run is retained.
An early feature assertion is added. The corrected testnet-feature build in
`offline-panel-enabled-build-20261019/` stopped at the compile-time requirement
for `BURST_TEST_GENESIS_PUBLIC_KEY`; it never ran a scenario. The next build,
`offline-panel-identity-build-20261019/`, supplies the recorded public candidate
key. Both failures are diagnosed configuration prerequisites, not resource
failures or reasons to relax checks. The scripts/context record private-testnet;
their inherited plan command arrays omit the added feature argument, so use the
captured build script as the actual invocation evidence. Do not rewrite those
immutable records or repeat a build solely to correct that metadata. No release
daemon or wallet changed.

The corrected build passed in 67 seconds and runtime passed in 197.37 seconds:
`offline-panel-enabled-20261019/`, receipt SHA-256
`b15417026b422e7da652be0bbbcf56383aead8fe99c1ce59319b507a5cba3f07`.
The initial challenge and four selected panel votes used live SDK preparation,
serialized context, pure signing and exact journal readback before certified
submission. Installed offline appeal admission, judgment reversal, expiry
preservation, four restarted nodes and a fresh observer passed in the same run.
This closes the SDK live-context question. It is not an installed offline
panel-vote UI journey or response-loss test for each identity action. Reconcile
that remaining installed-path coverage against the existing connected UI and
shared offline component/recovery evidence before final D-31.9 sign-off.
All jobs are terminal with no validation process left. Top-level receipts/logs
are copied to `target/validation-evidence/`; remote profiles remain retained.

Installed panel qualification now passes in `offline-panel-ui-20261019/`
(178.08 seconds), receipt
`4d35205c99c2bbe0c48a66173b4e813cd6fdacf4181f24aa422f132dd5e1c4f2`.
Both the initial Illegitimate vote and appeal Legitimate vote used the installed
review UI, disconnected signing, exact journal bytes, one accepted submission
with its response lost, two app restarts and confirmation without resubmission.
Target/context, selected decision, stake delta and account block count were
checked. Judgment reversal, expiry preservation, four restarted nodes and the
fresh observer also passed. The test-only build took 66 seconds. No product
artifact changed. This closes the installed panel gap identified above.
The retained-ledger initial-challenge check in
`offline-challenge-retained-20261019/` failed before challenge creation: device
enrollment remained unconfirmed with only one signer after the second human
entered authority. The original random target key was not retained, so that
ledger could not supply its missing signer. No old action was resubmitted.
`offline-challenge-quorum-20261019/` replaces only this unusable setup with a
known public target key and its second node. Both logs record certified epoch
`12fa9f5ec4e7f8e906b5310b31c5135937a1a233b801c709a2b10180167ba461`.
The new setup wait then failed because it required `peer_matches`, but
`rpc::online_signing_phase` deliberately returns `local` on Dev. The SDK accepts
both. `offline-challenge-ready-20261019/` resumes copies of these two ledgers,
without repeating target creation or endorsement, using the actual readiness
contract. No quorum, timeout or product code changed. Original failures remain.

That continuation is terminal pass. `offline-challenge-ready-20261019/` receipt
`b2b24e04f58fab440e7005822365b0cf1ffd02617c6cb4122ccec4f34f4172c8`
proves installed initial-challenge review, cancellation without signing,
disconnected signing, one accepted submission with its response lost, two app
restarts, exact journal archival and confirmation without resubmission. The
challenge's stake delta, target state, non-appeal target field and one-block
account delta match. This closes the initial-challenge path; it does not relabel
the earlier single-node failures or claim fresh onboarding from retained state.

Owner update, 2026-10-19: offline identity and governance are required before
release. D-31.9 implementation and selected Linux workflow qualification are now
complete, including the final panel and challenge records above. The dedicated
action context preserves operation-specific review and governance content.
Payment/receipt APIs still reject nonpayment actions. Stale contexts refuse
without silently rebasing or signing again.

The first backend batch
loads saved actions with their verified proposal content and submits the exact
signed bytes through the existing process/confirmation path. A lost response
retains the original journal and attempt marker; confirmed readback needs no
new signature, work request or submission. The common preflight now also checks
the saved signing grant against current state, even when the caller has adopted
the newer grant. The payment-only public entry point remains restricted.

Validation on VM1 compiled wallet-core once in 4.90 seconds and passed all 23
signed-client component tests. The new case submits a saved identity endorsement,
governance vote and full-content proposal, checks the process payload and marker,
preserves journal bytes after both success and response loss, and resolves an
already confirmed action without resubmission. Stale frontier, parameters and
device grant refuse before the attempt marker or process request. These use
scripted loopback RPC; they do not prove live protocol or installed UI acceptance.
Runtime receipt SHA-256 is
`91468a60d3c07eb7dbae21590bd2b876d22228b9f6ad4c8080467e0d9493f5ef`,
retained with build/results/logs in
`target/validation-evidence/offline-actions-20261019/`. Unit
`burst-offline-actions-20261019` is terminal exit 0/MainPID 0.

The next backend batch adds typed action contexts for endorsements, challenges,
appeals, verification votes, full-content proposals, governance votes,
endorsements and withdrawals, verifier participation and scoped delegation.
Delegation grant/revocation and secondary-ballot construction now share pure
helpers with the connected client. Offline callers no longer need a pre-signed
delegation payload. The native wallet registers preparation, disconnected signing
and exact saved-action submission commands. Payment-only commands remain narrow.

The action-context runtime receipt is
`314e9d465b6548e1e8ead699907eafa5b6601cfa5ca4e7ecef3ade1c66fd3f61`,
in `target/validation-evidence/offline-context-20261019-v2/`.
Its 21 selected tests passed in 14.67 seconds after a 4.96-second build. Assertions
cover positive action/signature/amount/content roundtrips, journal preservation,
delegation grant/revoke and secondary ballot construction, expired proposal
reviews, wrong signers and invalid work. Contexts in these tests are fixtures;
this does not verify live RPC preparation or installed UI behavior. The first
compile failed because a new test called a private verification method; its
receipt is retained in `offline-context-20261019/`. The corrected test uses the
public operation verifier. Both units are terminal with no remaining process.

Native compilation passed in 11.37 seconds under 1 CPU/3 GiB, zero swap, in
`/home/burst/offline-context-native-20261019/`. This proves command registration
and type integration, not runtime behavior. No daemon or release package changed.

| Reusable evidence | Proven scope | Invalidated by | Next action |
| --- | --- | --- | --- |
| `offline-actions-20261019/runtime-receipt.json` | Exact saved submission/content and response-loss recovery | Changes to submission, journal or their dependencies | Reuse reviewed unchanged path; do not repeat the whole group for UI edits |
| `offline-context-20261019-v2/runtime-receipt.json` | Offline construction and journal roundtrips from captured-context fixtures | Changed context/builders/storage rules | Add missing RPC preparation assertions, not another generic signing helper |
| `offline-context-native-20261019/native-receipt.json` | Native commands compile | Native or linked SDK input changes | Reuse until the UI/native review batch requires a rebuild |

D-31.9 UI integration is now implemented. Existing identity, appeal, panel vote,
governance, parameter/amendment and delegation reviews expose offline preparation.
The shared review keeps the signing context opaque, separates signing from
submission and shows exact amounts. Saved action recovery can review and submit
the original signed hash after reopening the app. Native adapters accept decimal
amounts as strings. Neither votes retain zero stake, and preparation refuses
missing proposal identity/deadline or provisional phase information.

New evidence is separated by stage on VM1:

- `offline-review-core-20261019/`: scripted RPC preparation followed by signing
  with the client dropped, positive Neither and governance vote behavior, and
  missing review-field refusals. Runtime receipt
  `9b7875d245b5802a77f00b05c046e3fc132c97842b876f771368e1af49f253dd`.
- `offline-review-native-20261019/`: native compilation passed in 7.93 seconds.
- `offline-review-ui-20261019/`: native adapter test, frontend typecheck and 13
  affected mocked-IPC browser checks passed. Browser receipt
  `3f71cc96579f0c9d82d1e9e650f48f60aee5e6a558ad31377727bff501796427`.
  The initial preflight used the wrong working directory for the native receipt;
  it started no test. The corrected preflight reused that build. The narrow-screen
  action review screenshot was inspected; no horizontal overflow was observed.
- `offline-review-package-20261019/`: frontend assets passed in 7.33 seconds,
  release wallet and Debian package built in 2m07s under 1 CPU/3 GiB. No daemon
  rebuild. Debian SHA-256 is
  `c680b598af2e76e8bbfe096af3ebf15b9c41a16b29403a368c931f765a0908b0`.
- `offline-review-installation-20261019/`: package metadata/dependencies and
  exact payload installation passed. Installed executable SHA-256 is
  `04a1c909abd465be7f99dd627040eaef09a103df3a926022109a715528887f51`.

The installed identity journey passed in `offline-review-runtime-20261019/`,
receipt `c737b07dc507a1507b83d05c06a22f0e426a5039bfcdaccd9754713346c12323`.
Unit `burst-offline-review-runtime-20261019` is terminal success/MainPID 0.
The existing offline driver exercised an endorsement, disconnected signing,
two app restarts and loss of an accepted submission response. Recovery retained
the exact journal and confirmed the original hash without signing or resubmitting.
This closes installed offline endorsement recovery, not every governance action.
The full-content proposal journey in `offline-proposal-runtime-20261019/`
passed preparation, disconnected signing, original-content review after restart,
response-loss handling and confirmation/archive after its second restart. Its
final assertion expected `brn_rate`; the RPC contract returns `BrnRate`. That
attempt remains failed. A readback of a copy of its retained ledger passed in
`offline-proposal-readback-20261019/`, receipt
`6161f5c5627f2bd8f5a76514abb5f707e18c0cda11b8aa82493ed10584c87b12`.
It confirms the original hash, exact `2000000000000000001` proposal value, one
account-chain addition, byte-identical archived journal and absence of pending
action/attempt markers. No onboarding, signing or submission was repeated.

The existing three-node parameter fixture exercises offline publication,
endorsement, withdrawal, delegation and direct/delegated votes with the installed
wallet. The first attempt accepted publication, endorsement and withdrawal, then
its second lookup incorrectly assumed list view. The corrected lookup returns
to All proposals. The retry in `offline-governance-navigation-20261019/` passed
those actions and a delegation grant, then refused offline vote preparation.
Both units are terminal failed/MainPID 0. The fixture deletes its temporary
ledgers on failure; preserved app logs and receipts remain the evidence.

Two-attempt diagnosis checkpoint: the first failure was driver navigation. The
second exposed an implementation error in the new SDK: requiring
`phase_projected == false` rejects the normal Exploration voting window.
`rpc/src/handlers/governance_projection.rs` describes the scheduled phase as a
read-only projection. `node/src/certified_network_tests/governance_ballot.rs`
explicitly asserts that projected Exploration accepts ballots. This is intended
functionality, not an unresolved design choice. The correction allows projected
Exploration for direct and delegated ballots while retaining exact proposal,
phase/deadline, typed metadata and signing-context checks. The focused core test
now includes positive projected and committed voting and passed in 0.15 seconds,
after a 5.29-second core build. Receipt in `offline-voting-core-20261019/` is
`84b56fa9a76969c21df59f2c88e37e61a2e751e9b30b9f22e6cfb3b6973f5731`.

The SDK-linked wallet rebuilt in 2m06s in `offline-voting-package-20261019/`.
Frontend assets were reused after receipt verification. Package installation
passed in `offline-voting-installation-20261019/`; installed SHA-256 is
`261e47b361e5886ea624a9a196f094a1a8a564985141a61462addfb8c7600e77`.
The corrected installed wallet then passed the existing three-node governance
journey in 87.52 seconds, including all eight offline publication/endorsement/
withdrawal/delegation/direct-ballot/secondary-ballot actions and the fixture's
activation/recovery assertions. Receipt in `offline-governance-voting-20261019/`
is `ba014337c817cff24d048072b2c69fa8aebd5d57693b0751f70ef16ab7fb6ff6`.
All three units are terminal success/MainPID 0. This closes the demonstrated
offline voting defect. Identity signing, journal/recovery, frontend and transport
branches did not change; their recorded evidence is reused, not relabeled as new.

Updated bundle assembly passed in `offline-release-bundle-20261019/`. Wallet
Debian SHA-256 is `76ec9b15da68791df677d0978dc6d7a09f664d6027ae9b47aa06f3c2b0b212cc`;
daemon and dashboard match the preceding retention bundle byte-for-byte.
The checksum manifest is
`9aa61a02ea6cea8668ffd3e4867d2b414d0379f2f143d71cbff4d04ede205b85`.
Its provenance retains failed original attempts alongside scoped continuations.
Final full-requirement sign-off and operator deployment handoff remain open.
VPS provisioning does not block software work. The older selected archive below
is unchanged; neither it nor this updated candidate is approved for publication.

The owner clarified that the future VPS is a deployment prerequisite only.
Continue software implementation and qualification on the existing bounded
validation environment without waiting for it. Host-specific deployment, DNS,
mail delivery and public launch remain separate; Azure is not the release host.

At the owner's request, a byte-identical creator seed backup was made under
`/home/nitesh/burst-creator-backup-20261019/`, outside the repository. The directory
is mode 0700 and the file 0600, owned by the user. `cmp --silent` passed; no seed
contents were displayed or uploaded. This resolves the requested local copy,
not independent disaster recovery. Both copies are on the same machine.
The owner subsequently approved deterministic internal fragment IDs with original
transaction/provenance links. That interpretation is resolved; no token-ID rewrite
is required. The owner will operate a separately provisioned VPS for release,
not Azure. Planned contacts are `support@brst.cc` and `verify@brst.cc`; routing
and delivery remain unverified. VPS details are pending, not authority to allocate
a substitute server. These decisions supersede unanswered-question wording in
the historical notes below.

Offline source review confirms that `offline/storage.rs::save_signed_action`
already preserves and validates proposal content alongside recovery ancestry.
Reuse that format and its existing restart tests. The missing capability is
operation-specific preparation/review/disconnected signing and later submission
through the native app, not a second proposal-content store. Payment-only
submission still calls `validate_signed_payment`; widening that check alone
would not supply the missing identity/governance context or reviewed UI.

Selected archive, 2026-10-19: the retention-corrected bundle is local at
`target/release-candidates/burst-testnet-linux-amd64-v7-retention-candidate.tar.gz`.
SHA-256 `5cfcd1ccdd3cbad77051b95fa8f5366fa06f09b0734665fadff2aaf4fb3abc1b`
matches the authenticated VM copy. Build, identity, assembly and archive units
are terminal. It is an unreleased candidate; remaining requirement and owner
handoff gates are unchanged.

Previous archive handoff: the epoch-corrected bundle remains local at
`target/release-candidates/burst-testnet-linux-amd64-v7-epoch-candidate.tar.gz`.
Its SHA-256 is `7718f46321b572de13b204393488ecbd438edca00a831f42d40fc3008d8fbe64`,
matching the authenticated VM copy. Archive unit
`burst-protocol-epoch-archive-20261019` is terminal exit 0/MainPID 0.
Assembly and source checks were reused; no compilation or user journey repeated.
This is an unreleased candidate, not approval to distribute it.

D-10.5/P5 reconciliation found unshared ingress/unchecked retention accounting,
an unenforced accepted-backlog capacity and growing fork-cache removal metadata.
The selected retention candidate contains the corrections described below.
Source inspection identified these defects; it did not establish a measured
exploit. The focused pressure and recovery results qualify their stated scope,
not total process memory or sustained public-network load. Do not reopen this
completed source batch because the broader scalability requirement remains partial.

D-10.5 source batch now passes focused validation. Live accepted-block tracking
checks capacity before insertion; its default is 65,536 entries. It is metadata,
not authority to admit or confirm blocks. The existing bounded LMDB scan recovers
accepted work independently. The new regression fills a one-entry cache with
three durable accounts, reopens the store and finds every next-unconfirmed block
without advancing confirmation. Fork-cache removal releases FIFO metadata;
full-capacity duplicates no longer evict unrelated roots.

Live ingress, authentication workers, unchecked entries and released recovery
batches now share 65,536 retained-block slots. Slots follow blocks until dropped,
including while authentication runs or recovery waits to requeue. Unchecked
storage is limited to 61,440 entries, reserving 4,096 for incoming dependencies.
That is a retention partition, not a reduction of the aggregate slot limit.
Already reserved recovery transfers can exceed the smaller ingress queue limit
without exceeding the shared budget. This prevents a full ingress queue from
discarding a dependency batch that was already held in memory.

The first capped validation build passed in 1m14s; 62 focused cases passed in
`retention-20261019/runtime`, receipt SHA-256
`7c1dfa6f5bc558fe95393eefe19db76356b1fe6a2b34ba04c6d76c641926bde9`.
A source review then identified the recovery-transfer/local-ingress edge. Its
correction compiled in 1m06s. Twelve affected checks passed in `runtime-recovery`,
including slot transfer through authentication/gap/requeue, full-ingress recovery,
and the actual two-node epoch/restart/owner-successor journey. Receipt SHA-256 is
`8349627aa1ec6b7a6f89e00f842a84cb5f54cbafb51bfabed0ddf1935c5c8f6f`.
Both builds and runtimes are terminal exit 0/MainPID 0. Local evidence is under
`target/validation-evidence/retention-20261019`. Other component passes were reused.
The ancillary source review found separately bounded caches: 1,024 rebroadcast
entries, 16 governance transfers with 512 KiB content each and a 64-message inbox,
and one active plus one queued historical-dependency request. Submission receipts
store payload digests and bounded replies, not entire input blocks. These caches
are not represented by the shared retention counter; it is not a total process
memory guarantee.

The legacy synchronous `BurstNode::process_block` bypassed durable worker
publication despite claiming to write LMDB. It now submits to the same bounded
queue and returns `Queued`, never premature `Accepted`. No production caller used
the former direct path; the RPC trait adapter remains separate and unchanged.
A regression verifies overflow refusal and no frontier/store mutation on enqueue.
The existing live fixture now preloads the real 4,096-entry ingress queue with a
missing-parent block, verifies overflow rejection, then starts the nodes and
submits the parent. Parent/child propagation, exact economics and restart pass.
This is duplicate-dependency ingress pressure, not 65,536 distinct gaps or a
total-memory stress benchmark.

Pressure build passed in 1m05s. The two new checks passed in 5.95s, including the
two-node pressure journey in 5.91s. Receipt SHA-256 is
`a090d4d26705a789cd6a609c52a7dc92ecc8b92ac1c0c6225952d6bcc845f6a6`
in `retention-20261019/runtime-pressure`; build/runtime units are terminal with no
remaining process. Earlier component and epoch-recovery passes were reused.

The optimized public daemon build passed in 4m15s under the existing VM caps.
SHA-256 is `a10053c78565815c9a23429594e81c282b5eae2c042c28ff155b06c6be5f7c99`.
Build-pressure and runtime-pressure receipts still match the actual current
source and retained executables. The exact release daemon passes Dev signed
marker/successor admission and two byte-exact history restarts, plus separate
public Test identity, 1 BRN/hour and authority-isolation checks. Identity receipt
SHA-256 is `7e262bde2fd4ae5170ddf5936ef9e7847b6154245fb20dfbc49dda916778efe7`
under `retention-20261019/identity`. These are startup/admission/restart checks,
not a release-binary load benchmark. Both units are terminal exit 0/MainPID 0.

Bundle assembly passed in `retention-20261019/bundle`, reusing unchanged wallet
`bdb93c1f...` and dashboard `94f3ea4e...`. Checksum manifest is
`c742e80356730476501d0a675b8be486d324ebf19df3103940395c31b8aa43c0`.
The operating limits distinguish the separate payload caches, source-level
pressure test and exact-artifact smoke. Archive job
`burst-retention-archive-20261019` is terminal exit 0/MainPID 0; its local checksum
matches the VM copy recorded at the top of this handoff. No packaging job remains.
Publication stays closed pending requirement reconciliation and owner handoff.
No creator seed was read, GUI rebuilt, VM created or VM2 restarted.

P3 installed presentation is now checked on the selected wallet executable
`d606a6d40eee9b35fa5f7090101b92b44e5f8af1671ab4747b7f2f31fbdef9b7`.
`p3-readback-corrected-20261019` passes both activated proposal screens, exact
constitution content, governed parameter values and restored verification after
opening a copied retained ledger. The restored wallet's account head is unchanged;
no signing-device enrollment, proposal, vote, challenge or payment was submitted.
Receipt SHA-256 is
`fd0ca956eb44f75d04c7ec971089e0a6bfa4743a826e532ae23ebaa5ef1f1f9e`.
The unit is terminal exit 0/MainPID 0. Local evidence, including the first failure,
is in `target/validation-evidence/p3-readback-20261019/`. The first assertion
incorrectly equated the native string constitution version with the RPC number.
The corrected assertion follows the explicit conversion in `constitution.rs`;
all other document fields are compared unchanged. No production code was changed.

The P3 dependency review compares the device-certified native build with the
selected installed wallet build. Constitution/delegation modules, signed-client
and governance-content sources are unchanged. Archived source hashes anchor the
comparison of the unchanged challenge, appeal, verification vote, proposal vote
and delegation handlers and their command registrations. Governance/verification
page sources are unchanged; their emitted chunks differ, so byte-identical UI
reuse is not claimed. This installed readback supplies the missing presentation
check, while original positive submission journeys retain their development scope.
The old frontend receipt records its log, not output hashes; do not invent an
output receipt for it. The selected daemon's comparison with the payment-qualified
daemon contains the reviewed epoch/retention changes, not governance or verification
engine changes. Exact public-daemon governance execution is not claimed by this
read-only run, which uses the retained ledger's matching staging daemon.

P4 transport dependency review is complete for the retained isolated NAT scope.
The NAT-qualified `suffix-batch.vEo5wkoe/candidate/SOURCES.sha256` and selected
retention manifest have 820 and 833 entries. All 34 network-source entries and
`Cargo.lock` match. The archived `node/src/wire_message.rs`, `node/src/bootstrap.rs`
and `types/src/lib.rs` from `public-testnet-bundle-20261019` match the NAT manifest
hashes. Direct comparisons show only test-fixture signing-grant fields in the
first two files; the third advances protocol 35 to 36 for signing-device grants.
This is not a claim that old and new signed blocks interoperate. The v7 device
journeys cover that boundary; the isolated NAT proof retains its own version,
Dev transactions and emulator topology. No network or NAT-lab rerun is justified
by these changes. The NAT receipt's file SHA-256 is
`fc401c1a12de5d02510ae45151ff544c7265758ab993d34587ef080796a5b602`;
the older `3d9266ff...` reference identifies its validation fingerprint, not this
file checksum.

The retained P1 and P4 continuation logs were inspected, not inferred from test
counts. `device-release-20261019/certified-readback/runtime.log` checks the exact
verified subject, certified context, confirmed payments and received balance on
three nodes, plus locked refusal with unchanged draft/head. Its SHA-256 is
`bca78e986500827f5c05fd949c92309d30f46003a75c9c0aa8423086ab8ee2a5`.
`snapshot-payment/runtime.log` checks a new signed payment after restart and exact
verification/governance state on two nodes; SHA-256 is
`0a6b64873d50920a8b256433b5bab4dbb89f9996c5914737e431b446c9e476a1`.
These establish their recorded positive behavior, not a new uninterrupted run.
The later epoch/retention source batch has its separate recovery evidence above.

P1/P4 change-impact mapping from the device-release daemon to the selected
retention daemon is now recorded. Comparing their immutable source manifests
finds 41 changed/added paths, including tests, helpers and deployment records.
No checkpoint-authority, selection, verification engine, snapshot installer,
storage transaction, wire transport or dependency-lock source changed across
this boundary. The changed production paths belong to the batches below.

| Changed behavior | Evidence reused or added | Scope retained |
| --- | --- | --- |
| Checked BRN arithmetic in admission/replay/RPC; removal of an unused proposal balance calculation | Arithmetic correction and installed payment evidence already recorded below. Direct archived-source comparison confirms the RPC diff is checked arithmetic plus reference-value display; governance submission still uses the same authority checks. | No new quorum or selection behavior. Overflow is an error, not a fabricated zero balance. |
| Permitted self-send and linear TRST reference value | Installed `linear-reference-20261019/consumer` payment/self-send/receive results and reference-boundary checks. | Reference value does not change face amount, provenance or expiry. Ordinary lifecycle evidence remains applicable. |
| Creator-authenticated epoch recovery | Focused replay/snapshot proofs, two-node recovery and exact selected-daemon marker/successor restarts above. | Current-format markers, not a future-format migration claim. |
| Shared retention slots and bounded scheduling/fork metadata | Slot-transfer regressions, full-ingress parent/child recovery, durable-backlog restart and live epoch/restart checks above. | No total-memory or sustained-load qualification. Durable history and authority are not replaced by cache membership. |

Together with the original positive onboarding, observer, snapshot, reconnect and
uncertain-action records, this closes the pending dependency-mapping task. It is
composite qualification, not a claim that the final public identity reran every
old topology or that a previously failed uninterrupted run passed. Keep default
Live/mainnet closed. No additional compiler or acceptance run was needed for this
mapping. The remaining whitepaper interpretations and public operator handoff
still prevent release sign-off.

Owner decisions are settled: offline identity/governance is required, internal
fragment IDs retain original ancestry, and the owner will provision a non-Azure
release VPS. The requested same-machine creator backup is verified; independent
disaster recovery is not. D-31.9 and public-testnet software reconciliation are
complete within the recorded Linux scope. Deployment/contact readiness remains
a public-launch gate. These decisions do not require a full-suite rerun.

Completed source correction, 2026-10-19: D-10.3 protocol epoch markers were accepted
by live admission and persisted as account epoch updates, but certified replay
rejected `Epoch` as unsupported. The selected candidate contains this P4 recovery
correction and remains closed to publication for the other gates. It authenticates markers with the configured
creator in genesis replay, checkpoint continuation and provisional snapshot-tail
validation. Shared validation prevents a marker from changing balances,
representative or signing grant, or carrying payment/stake/delegation actions.
The existing account projection retains its version update. No new serialization
field or protocol version was introduced.

| Changed dependency | Affected evidence | Reuse and next check |
| --- | --- | --- |
| Epoch field/continuity checks and replay authentication | D-10.3, P4 accepted-chain recovery | Compile the coherent batch once, then positive creator marker/owner successor, wrong signer/action rejection and durable history reopen. |
| Epoch continuation and provisional-tail handling | P4 authenticated snapshots and installation | Extend the existing snapshot roundtrip with a marker; retain unsupported-tail rejection using GovernanceActivation. Run those affected cases, not the full crash suite. |
| Daemon dependency on these sources | Selected artifact qualification | Old package receipts retain their prior scope. After source integration passes, build once and qualify marker admission/recovery on that artifact. No GUI redesign, NAT rerun or new VM is justified by this change. |

The first VM validation build failed with E0382 because snapshot `Base` was used
after being moved into the plan. The correction captures the configured genesis
before that move; it changes no rule. Failed evidence is retained under
`/home/burst/protocol-epoch-20261019`. The corrected build passed in 1m04s.
The focused runtime completed in 2.86s with 13 passes and one failure. Positive
creator-marker replay, owner successor, durable history reopen, authenticated
snapshot and affected installation checks passed. The failure was the new
rejection fixture unwrapping a history read which correctly rejected an epoch
containing an account action. The test now asserts that exact early error; signer,
balance, representative and grant refusals keep their replay assertions.
This diagnosis follows the compiler failure and runtime failure; neither is
erased. The test-only build in `protocol-epoch-20261019/assertion-corrected` passed
in 1m06s; only the corrected rejection case ran again and passed in 0.10s.
At that point, the other 13 results remained reusable with unchanged production inputs.
Local evidence is `target/validation-evidence/protocol-epoch-20261019`.
The aggregate failed runtime receipt SHA-256 is `26c4a8804fd9e4785e453ef2def719e10b7e63c3b465c7dfd79aed7618d2be7f`;
the focused passing receipt is `eb714f393ca68b1a92276d63afc566960367d4253ab48d634b99e8330003e84f`.
The latter's actual assertion scope is rejection only; its generic result review
does not independently prove the positive cases recorded in the former log.
The new live fixture uses the existing compact-node runner. Its first build
passed in 1m05s; runtime failed after 3.34s during restart with `invalid signed
snapshot tail block`. Live admission, peer delivery and the pre-restart account
assertions had completed. `SnapshotTailPlan::read` still used the account key for
every block; `HistoryCursor::accept` had the same assumption. Both now reuse the
live admission signer selector. Creator authority remains limited by the marker
field and continuity checks; ordinary blocks still require their account key.
The existing snapshot test now includes a marker on another account and checks
that a forged owner signature cannot advance the historical cursor.

Failed live evidence is under `protocol-epoch-20261019/live`. The corrected build
passed in 1m05s. All four affected cases passed in 7.25s; the actual two-node
journey took 5.67s. It proves creator-marker peer delivery to another wallet's
chain, preserved account rights, receiver restart, an owner-signed successor and
a second restart. The changed snapshot case also proves foreign-account marker
membership and forged-owner refusal without advancing the history cursor.
Local receipt SHA-256 is `904c7d1492b69477bdce2b78af3cc6cd69b45bd45d29672440fab5a822cf8ad9`
in `target/validation-evidence/protocol-epoch-20261019/live-corrected`.
These are controlled-time, loopback development nodes, not an independent-host
ORV quorum or release-binary test. The other component results retain their scope.

The optimized public daemon build passed in 3m56s. Its SHA-256 is
`9fdca645920fe0644de08f51be6c7b09d857b15be76df362bbc38ac37fcecede`.
The build receipt checked successfully against the source before test-only edits.
Its immutable source archive remains in `protocol-epoch-20261019/daemon/candidate`.
No creator seed was read; the public identity and 1 BRN/hour setting are unchanged.

Exact-artifact qualification passed in `protocol-epoch-20261019/identity-work`.
Receipt SHA-256 is `52b2b94f87960fd75e4e1a187c1305b3a991cd19c0507e684a2d332677753ea0`.
The existing smoke driver checks a disposable Dev epoch marker, an owner-signed
successor and byte-exact signed history across two restarts. It separately checks
public Test genesis, initial rate, creator authority isolation and Dev/Test/Live
separation. The first attempt remains failed in `identity`: its helper omitted
proof-of-work and admission correctly rejected it. Only the helper changed to use
the existing work RPC; its rebuild took 0.78s. The daemon was not rebuilt.
Both runtime units are terminal with no remaining process.

The current-format D-10.3 recovery mismatch is resolved by the source and exact
artifact evidence above. Unknown future block decoding and arbitrary migrations
are not proven. Wallet/dashboard packages remain unchanged. Bundle assembly passed
in `protocol-epoch-20261019/bundle`; its unit is terminal with no remaining process.
The component checksum manifest is
`e672196954dfe6c76fa762c58eb8ea4abd67e926889c8df0743c9a258e1e533a`.
Publication remains closed pending final requirement reconciliation and the
creator-backup/bootstrap/contact handoff. No unrelated wallet, NAT or onboarding
rerun is justified by this change.

Current handoff, 2026-10-19: no public release gate has opened. D-6.7 self-send
and D-6.13 linear reference are signed off for the selected Linux capability.
The new exact installed wallet and daemon pass the affected real payment,
manual merge, self-send/receive and reference-field journey. Its receipt is
`cf0cddeb8081fe052567e2db2ed26afcee1f0159e8dcfbf01d46251dd331d6f4`.
This does not qualify the whole release or all restart scenarios.

VM1 `linear-reference-20261019` retains the core/native/browser, build,
installation, identity and consumer results. Wallet compilation took 2m19s;
daemon compilation took 4m44s. The prior wallet package is
`c3508e84a835109e535cc2bb9a7acc634c833974268bf42176a15db301ed7245`,
installed payload `784da5dc5d172e67b660abeacbf028c2562a5694e09386898b41d759d5055f1d`,
daemon `1cd642ba31a6c6127dc4746962c0612c3638a77130199aa78d86b360702e3be4`.
The unchanged dashboard is retained. Assembly and archive are terminal pass.
That assembly is terminal. Its archive SHA-256 is
`1ec15c95a2caa9826920b7266565731e1a8ece0ff7cce1c8bdb4d2b4f86a5f79`.
The local archive's checksum matches the VM copy at
`target/release-candidates/burst-testnet-linux-amd64-v7-trst-candidate.tar.gz`.
The older arithmetic archive remains immutable as prior evidence.
Stage receipts and logs are copied locally under
`target/validation-evidence/linear-reference-release-20261019`.
The D-6.12 capacity review now passes. D-6.17 pointer-based origin representation
and D-6.18 upward rounding also pass their reviewed assertions. Only an engine
test changed; selected production artifacts stay frozen. No job is running.
Graph persistence D-6.4 is now signed off by reviewed existing two-node restart
evidence and matching storage sources. D-6.3 has an explicit interpretation
conflict: ordinary holdings use transaction hashes, but automatic returns,
revocation fragments and multi-portion receipts use derived internal IDs. The
owner was asked whether these exceptions are acceptable; preserve the current
behavior pending that answer. No production code changed for this review.
TRST raw representation and no-expiry launch defaults (D-6.1/2) now pass explicit
types assertions. Only tests changed; removing those assertions reproduces the
selected daemon's full source hashes. D-6.5/6.6's installed recipient-permission
check now passes in `recipient-policy-20261019`, receipt
`e5e13471e50d5cbc46832a385135bbe03234c5cc3c10ead25db9c5bfc03fd28d`.
An unverified recipient receives a Send but cannot spend it. Refusal leaves its
head and holdings unchanged. The sender retains the remainder ID and publishes
exactly one block. The shared node verification guard covers Send and Burn;
this new runtime assertion exercises Send, not a separate unverified Burn.
Existing burn-to-unverified receipt evidence is retained. Only the driver changed;
no rebuild or new VM. The capped unit is terminal exit 0/MainPID 0, and receipt
integrity checks pass. D-6.15 now has revalidated native appeal-restoration
evidence, copied under `target/validation-evidence/lifecycle-reconciliation-20261019`.
Receipt `bdda3ce340d52ea73d0f083d541ac43e3d02b71626661aef1b4d00101569646c`
passes in its recorded working directory. At that review the lifecycle driver
matched the tested copy exactly; the later notice-only branch is recorded below.
No lifecycle was rerun. D-6.14's distinct clean-holder
assertion now passes in `holder-restoration-20261019/corrected`, receipt
`99d1041b014a2061ad2f971898c039518c3699dde4ca19c7329d7ded096d62e2`.
Signed candidate payments preserve clean holdings through revocation, reject
spending, then permit Send/Receive after re-verification and durable reopening.
Settlement uses direct existing helpers, not a newly run certified panel.
Only a test changed. The first compile passed; inspection then caught a missing
fixture snapshot before runtime. The corrected compile took 1m06s and its one
runtime case passed in 0.13s. Both builds are retained; no production artifact
changed. All owned units are terminal exit 0/MainPID 0.
The four-file impact review is complete; D-6.15's reversible-origin rule is
signed off using the retained native result and unchanged settlement logic.
No lifecycle rerun or release rebuild. D-6.9/16 now pass on the already compiled
current node test artifact. Historical logs lacked current dependency closure,
so only the two existing expiry cases ran, sequentially, with no compilation.
Governed expiry extension restores claim/spend; pending expiry returns once and
rejects late receipt. Both preserve the required state through restart and fresh
catchup. Runtime was 80.75s, receipt
`fb177372c12ecff4eb82dc4ed7958abf7a1f5289189a7a936be50871f4ff3024` in
`target/validation-evidence/expiry-reconciliation-20261019`. The unit is terminal
exit 0/MainPID 0. Controlled clocks and the recorded authorities do not establish
all failure boundaries. D-6.1 through D-6.18 are now signed off for their stated
rules except D-6.3's unresolved internal-ID interpretation. Next reconcile the
revocation requirements in section 7 against these retained lifecycle results.
D-7.1/2 now pass by source/assertion review and reused native/merge evidence;
current engine and graph hashes still match the merge receipt. D-7.3's automatic
notice is now implemented in the wallet shell using existing polling and origin
inspection. The accessible persistent message avoids repeated popups; details
show exact amount and affected origins. It resets on wallet/node changes,
labels unavailable refreshes, and clears after restoration. Mobile/touch,
repeated-poll, restoration and stale-response browser assertions pass as scoped
composite evidence. The first run passed the mobile case but failed a test-only
exact-call-count assumption under StrictMode; that failure is retained. Only
the stale-response case reran and passed after request-identity matching.
TypeScript/Vite also passes. Evidence is in
`target/validation-evidence/revocation-notice-20261019`; stale-case receipt
`3abc95d68e2dc56069f5395adde97a68bc3cd92664e7eedd67a24b1c21d3e146`,
frontend receipt `c6708f3775b5f89818c3d5d3ba7cfdc03ddec836d27a0d5ead46278387694a31`.
Mobile screenshot `/tmp/burst-revocation-mobile.png` was visually reviewed.
The wallet package and exact installation now pass. Compilation took 1m49s.
Package SHA-256 is `bdb93c1fb258cdd18b0045f900984f376ebacdacec06fe35fe780ae8a575e0ee`;
installed payload is `d606a6d40eee9b35fa5f7090101b92b44e5f8af1671ab4747b7f2f31fbdef9b7`.
Build receipt `d2217b039735b85dffedbcb6d8035c1a290428f21f25292972a9acfc1caf7c26`
and installation receipt `7922a3437cc132b5e9b7785290febab171166a01f76c87b754af94f4ba466728`
retain exact package provenance.
The installed notice passes against a copy of a retained certified fraud ledger.
It shows 2 TRST revoked, the real origin wallet and revoked-origin flag, while
0.001 clean TRST remains active. Reading the notice does not change the account
head or enroll a signing device. Native receipt
`45039177b0d529ea5f405d099ca6d678c1f91423f23d255f60ae814c7ceb17b6`
is in `revocation-notice-20261019/native-corrected`. The first native attempt
expected integer text instead of the existing six-decimal display; preserve its
failure in `native`. Only the corrected read-only check reran, without rebuilding.
The matching historical staging daemon served that retained ledger. This proves
the new installed notice/RPC integration, not another current-daemon panel run.
The lifecycle driver now has an explicit notice-only branch; its default journey
is retained. The restore helper can skip signing-device enrollment for read-only
checks and still enrolls by default. D-7.3 is complete for this Linux behavior.
No daemon, RPC contract or native signing change. Candidate assembly and archive
are terminal pass in `revocation-notice-20261019/bundle`. Assembly receipt is
`759759cef5f7c66878a71b56e72040ef422423b462eca5595212652d3285ceb9`;
checksum manifest is `52b9fd937ce02c750d330bc74b7248262d2b7c3e49cd01cb67c54016a1ebcccd`.
The new archive is `target/release-candidates/burst-testnet-linux-amd64-v7-notice-candidate.tar.gz`,
SHA-256 `1067b52bf7239aea74bac7afd090cffcc880ccc5780cee3be7235b796099197f`.
The previous archive remains immutable. All owned remote jobs are terminal;
no daemon or desktop rebuild was needed. The coverage index has 388 sections,
349 unsigned at that boundary and zero structural errors. Counts are not a readiness measure.
D-7.4 now passes its reversible-judgment requirement using the retained native
meta-challenge evidence, not another panel run. The current settlement, receipt,
appeal lookup, orchestrator and release-policy files match both that build and
the selected daemon. The signed appeal names the reviewed judgment. Admission
does not restore rights; selected-human settlement restores the identity and
originated TRST before a new payment/receive succeeds. This preserves D-6.15's
separate re-verification restoration rule; it does not erase historical judgments.
The Test feature enables appeals while Live remains closed. Original composite
development, timing and fault-coverage limits still apply.
Section 8 is now reconciled. Existing installed payment evidence proves the
account-balance model, two-phase Send and two-phase Burn. Schema review confirms
no on-chain memo or optional application subtype. Bincode transaction encoding
was already signed off; Protobuf is its outer peer envelope under D-23.3.
State-block, persistence and wire sources match the selected daemon. These are
seven status corrections backed by existing evidence, with no build or runtime
rerun. Next review section 9's actual block-format and limit requirements.
Section 9's representation review now closes D-9.2/3/4/9/10. The selected
candidate has a version byte, no generic extension field, fixed Ed25519 block
signatures, u64 Unix-second timestamps and little-endian Bincode block encoding.
The relevant production files match the recorded daemon inputs. Bincode's locked
local implementation confirms its defaults; the hash preimage's separate
big-endian layout is unchanged. These are schema/encoding findings, not new
migration, clock-safety or cryptographic fault-test claims.
D-9.1/5/6/8 remain open. The size review must include C01/C07's recorded merge
and returned-stake allowances and the bounded typed delegation payload. Pending
quantity, timeout and chain-depth claims need their own evidence; do not infer
them from the representation sign-offs. No build or runtime job was started.
D-9.7's missing long-age assertion now passes. The existing composition fixture
submits a signed Burn, preserves the exact pending entry and recipient head after
31 days and one year, reopens storage at each boundary, then accepts a signed
Receive and reopens again with the exact active amount and origin. Infinite and
two-year expiry both pass. Runtime was 0.20s; receipt
`d752ed2e4c122caf2fb629a90f345335f42b7aad27be43cddfad27aafc4d6da5`
is retained in `target/validation-evidence/pending-age-20261019/corrected`.
The first test used seconds as 30-second fixture ticks; its finite case correctly
expired after an unintended 30-year jump. Preserve failed receipt `65e1cfba...`.
The corrected test asserts elapsed seconds explicitly. Both test-only builds
took 1m06s on the capped VM. No production code, daemon or GUI package changed;
existing network expiry/return/revival proofs were not repeated. All owned jobs
are terminal. This is signed-candidate/storage evidence, not another live network
or GUI journey.
For D-9.6, the existing 131,073-row pending-storage test was found, but its old
`/tmp/burst-streamed-pending-recovery.log` is absent locally and on VM1. Keep that
historical claim distinct from verified current evidence. Admission/storage have
no per-destination count check in the reviewed paths; large-count qualification
still needs retained evidence or one focused existing test. D-9.8 similarly must
distinguish no protocol chain-depth ceiling from resource-bounded recovery and
the documented lack of arbitrary-scale qualification.

Continuation review, 2026-10-19: the corrected pending-age result and full runtime
log are present locally. The terminal result is pass, exit 0, one named test in
0.20s. No repeat was needed. The desktop validation unit is absent; VM1 has only
the existing private validation node running. No VM was allocated or started.

The section 9/10 review found stale records rather than another reason to build.
Coverage now distinguishes the following work:

| Requirement | Current finding | Next action and release relevance |
| --- | --- | --- |
| D-9.1 state fields | Version-3/4 limitations in the row are historical. Current schema is version 7. `brn_balance` names a cumulative commitment; `link` is operation-dependent, with separate origin/merge references. | Map the selected naming/provenance intent explicitly. Do not rename serialized fields or alter economic behavior just to erase a documentation discrepancy. Current lifecycle receipts remain reusable. |
| D-9.5 encoded size | Base/merge, returned-stake and typed delegation allowances are implemented. Positive and rejection test cases already exist. | Reconcile retained execution evidence and delegation allowance authority. Do not add duplicate tests or reopen C01/C07. |
| D-10.2 pruning | The unsafe index-deletion task was removed. Configurable physical compaction is still incomplete. | Keep C16 on the scale roadmap under the selected pilot scope. A supported-envelope safety/recovery failure would make it a release blocker; age alone never authorizes deleting restorable rights. |
| D-10.3 epoch upgrades | Shared creator authentication passes replay/reopen, snapshot/history proofs, two-node recovery and exact release-daemon marker/successor restarts. | Current-format recovery is qualified. Fresh version-7 startup is not a future-format migration test. No repeat scheduled. |
| D-10.5 unchecked capacity | Shared slots and bounded metadata pass focused checks, full-ingress missing-parent recovery on two nodes, and exact-daemon admission/restart. Ancillary caches are separately bounded. | Identified retention-growth defects corrected. Preserve documented limits: no sustained-load or total-process-memory claim. Broad D10.5 sign-off is distinct from these scoped P5 results. |

No additional requirement became complete in this source-review batch. D-9.7
remains the latest completed requirement. No candidate or release gate changed.
The next work is the remaining requirement-to-evidence mapping, with runtime work
only for a concrete missing assertion. The owner subsequently selected the full
offline-action scope, internal fragment IDs and a self-operated non-Azure release
VPS. The requested local creator backup is verified. Contact/bootstrap deployment
is still a launch gate; these choices are no longer unanswered questions.
Do not rerun expiry or unrelated network acceptance for documentation updates.

The installed expired-merge check is terminal pass in
`/home/burst/expired-merge-20261019`. Its test-only compilation took 1m07s;
the installed journey and two-node restart took 9.64s. The native driver extends
the existing payment/session runner. No release app or daemon was rebuilt.
The required clock library was installed on VM1. No desktop build is running.
Compilation, runtime, assembly and archive units are all terminal exit 0/MainPID 0.
The previous 31 MiB arithmetic archive is retained locally at
`target/release-candidates/burst-testnet-linux-amd64-v7-arithmetic-candidate.tar.gz`,
SHA-256 `47f8f4bdb6215c12f63cc4fbbb32b469ecc9fa8cd519a6edb1cda7cfe2fb970a`.
Its manifest still says `publicationApproved: false`. Do not invite participants
from this package before the remaining gates below close.
Under
`/home/burst/arithmetic-release-20261019`, daemon build, identity, consumer,
frontend, wallet package and installation are terminal pass. Wallet frontend
took 22.819s, package 1m49.932s and installation 1.153s. Preserve these artifacts and reuse
the unchanged native signing, transport and recovery evidence.

| Current work | Evidence and next action |
| --- | --- |
| D-6.1 / D-6.2 | Complete raw representation and launch defaults. One raw and u128::MAX remain exact; 10^18 raw equals one TRST. Explicit default tests confirm no expiry for initial public/Test/Dev configurations. No production or package changes. |
| D-6.5 / D-6.6 | Complete for account-style change and recipient permission. Frozen installed artifacts pass Send receipt while unverified, refusal of spending with unchanged head/holdings, retained sender remainder ID/age/origin, and exactly one Send block. `recipient-policy-20261019` receipt `e5e13471...` passes integrity checks. Source review confirms Send/Burn share the node verification guard; the new negative runtime case is Send. No product changes or rebuild. |
| D-6.14 holder preservation | Complete accounting rule. New focused candidate test preserves exact clean holdings through revocation/reopen, rejects spending without mutation, then permits signed Send/Receive after restoration/reopen. Amount, age and origin survive another reopen. Receipt `99d1041b...`; no new live-panel or installed GUI claim, product change or repackaging. |
| D-6.15 origin restoration | Complete reversible-origin rule. Retained native evidence revalidated in `device-certified-20261019/lifecycle-continue`; exact amounts and unrelated-origin preservation pass, followed by restored payment and three-node readback. Archived source matches the recorded build. Four-file diff review leaves settlement/restoration unchanged. Preserve original resumed-development limits; no new installed, uninterrupted or all-fault claim. |
| D-6.9 / D-6.16 | Complete existing-token expiry policy and automatic pending-return rules. Two current-source live-node cases pass in `expiry-reconciliation-20261019`, using the existing executable. Return/claim/spend, late and duplicate rejection, restart and fresh catchup pass in 80.75s. No GUI rerun, compilation or all-fault claim. |
| D-7.1 / D-7.2 | Origin-wide revocation and proportional clean/revoked splits are complete for their stated rules. Existing native and merge proofs reused with current matching engine/graph hashes. Internal fragment-ID interpretation remains D-6.3, not silently resolved here. |
| D-7.3 holder notification | Complete for Linux. Browser composite evidence covers touch details, repeat polling, offline labeling, restoration and wallet/node isolation. New package and exact installation pass. Native receipt `45039177...` proves automatic notice, exact revoked amount, real origin address/flag and unchanged clean balance/head using a retained certified ledger. Original browser and native assertion failures remain recorded. No daemon or signing change. |
| D-7.4 judgment reversal | Complete for the specified meta-challenge behavior. Retained native receipt `bdda3ce3...` proves reviewed appeal, no restoration at admission, selected-human verdict, restored funds/rights and subsequent payment. Relevant sources match the selected daemon. D-6.15 separately preserves re-verification restoration. No new panel run or broader fault claim. |
| D-8.1 through D-8.7 | Complete for the selected transaction-model choices. Installed payment receipts prove account balances and two-phase Send/Burn. Matching schemas have no memos or app subtype. Existing Bincode block encoding remains inside the Protobuf peer envelope. No new test or product change; these narrow sign-offs do not close all payment/recovery requirements. |
| D-9.2 / D-9.3 / D-9.4 / D-9.9 / D-9.10 | Complete representation choices by matching source/dependency review and retained signed-payment evidence. Version byte, no generic extensions, Ed25519 block signatures, Unix-second u64 timestamps and little-endian Bincode payloads are present. No migration or trusted-clock safety claim. D-9.1/5/6/8 still require review. |
| D-9.7 pending age | Complete no-independent-timeout rule. Signed-candidate and durable-storage assertions pass at 31 days and one year for infinite/two-year expiry, followed by successful Receive and exact restored holdings. Receipt `d752ed2e...`; retain the original test-clock failure. No production change or repeated network acceptance. |
| D-6.3 token identity | Awaiting owner interpretation of deterministic internal fragment IDs versus literal transaction-hash IDs. Ordinary receive/merge holdings follow the selected rule; automatic lifecycle fragments have no separate transaction. Do not silently rewrite them or mark compliance. |
| D-6.4 graph storage | Complete on-disk LMDB persistence. Existing two-node restart checks preserve the exact portfolio and origins. Storage publisher/loader source hashes match both the retained validation build and selected daemon. No new run. |
| D-6.7 self-transfers | Complete for self-send/receive. Public helpers now agree and are packaged. Exact new installed artifacts pass pending debit, restored total, preserved age/provenance and duplicate-receive refusal in `linear-reference-20261019/consumer`. Prior `self-transfer-20261019` remains supporting evidence. |
| D-6.13 linear reference | Complete display-only reference. Shared token/RPC calculation, native field, touch UI, fallback and unchanged balance checks pass. Selected installed package and daemon pass real field roundtrip. No enforced market price or face-value reduction. |
| D-6.8 manual combination | Implemented through existing journaled signing. Active manual merge, stale/replay guards and provenance passed previously. Exact installed payload `a5ca4520...` passes expired review/cancel/focus, duplicate-click protection, signed combination, exact amount/age/origins, non-spendability and two-node restart in `expired-merge-20261019`. Controlled-clock validation nodes do not establish public expiry-governance timing. |
| D-6.12 / D-6.17 / D-6.18 | Complete for the 256-input limit, pointer-based ancestry representation and upward revocation rounding. Focused engine/wire checks pass in `merge-capacity-20261019`; native selection evidence is reused. No production change or repackaging. Broader fork, storage and challenge authority requirements retain their own status. |
| D-5.12 arithmetic correction | Node/RPC/native balance fixes have focused passes and release qualification. New daemon `e163bc5b...` passed identity/restart and real payment/endorsement acceptance. Frontend accrual now checks overflow before deductions; eight BRN tests and the rendered error-state check pass. Corrected wallet package `aec12eb1...` and exact installation pass. The current archive includes these corrections. No arithmetic build or acceptance run remains scheduled. Publication stays closed. |
| D-31.2 wallet correction | Complete for the selected Linux behavior. All 14 focused merge tests pass, including the exact 90-100-day boundary. Corrected UI/package build, installation and native payment/merge acceptance pass. Portfolio and native automation share one selector. Daemon, takeover and network dependencies are unchanged; those journeys were not repeated. |
| Certified development journeys | Consumer recovery, challenge/appeal, governance and enrollment-opening recovery passed. Preserve the original failed attempts and composite-evidence limits below. No repeats without changed dependencies. |
| Selected public daemon | `a10053c78565815c9a23429594e81c282b5eae2c042c28ff155b06c6be5f7c99` passes build and exact signed admission/restart plus public identity checks in `retention-20261019`. Source pressure/recovery passes retain their separate scope. Earlier binaries remain evidence for unaffected payment/reference paths. No full-suite replay or new GUI build. |
| Selected wallet package | Current Debian `9d0cd5aa...` in `offline-trst-package-20261019`, installed payload `3b8e6534...`; full hashes and qualified offline paths appear in the current handoff. Exact package transformation and installation pass. Earlier holder-notice, reference/payment/self-send and device-recovery evidence retains its unchanged scope. No whole-suite rerun. |
| Dashboard | Local comparison against the retained package source found only added design documentation. Application source and package configuration match. Existing RPC/rendered-navigation/focus drivers passed with the final daemon in `device-release-20261019/dashboard`, unit terminal exit 0/MainPID 0. No rebuild/reinstall. |
| Installed wallet | Current payload `3b8e6534...` passes all three installed TRST offline/restart journeys. Earlier `d606a6d4...` notice, `784da5dc...` payment/merge/self-send/reference and `96b3ddd7...` backup/enrollment/signer/return/group results retain their unchanged scope. No new full-suite claim. |
| Certified release qualification | Optimized disposable daemon build passed in 3m45s, SHA-256 `ea0b4e7103f15596f0e445cea43da1498911f1308dabf2ff077975ada13f1616`; never distribute it. `certified-consumer` is terminal exit 1/MainPID 0 after fresh verification, seven votes, payment, transfer and receipt passed. Final test expected `/locked/`, but locking intentionally unloads the wallet and `pay` returned `Wallet not loaded`. Source confirms this contract. Preserve profile `burst-native-wallet-YRZMqJ`, journals and nodes; reconcile readback and lock behavior without replaying onboarding. Original failure remains failed. Driver now asserts `is_locked` and accepts only these specific lock errors; it also saves positive-flow evidence before negative checks. |
| Certified consumer readback | `device-release-20261019/certified-readback` passed, terminal exit 0/MainPID 0. Exact saved receipts, seven votes, provenance and balances agree on all three nodes. Locked native restart refused payment with unchanged draft/head. Original fresh positive journey was not repeated; its late wording-assertion failure is preserved. Reconstructed report is labeled as such. |
| Snapshot qualification | `device-release-20261019/snapshot` remains terminal failed, exit 1/MainPID 0. Authentication, tamper rejection, installation, SIGKILL/reconnect and exact lifecycle/governance readback passed. The final probe checked peer connectivity but not signing readiness. The example-only correction and retained-state payment continuation are now complete; see Snapshot final payment below. No rebuild or scenario repeat remains scheduled. Composite evidence does not prove uninterrupted timing. |
| Installed uncertain-action recovery | `device-release-20261019/offline-recovery` passed all three stages, unit terminal exit 0/MainPID 0. Disconnected signing, real node acceptance with lost response and a second locked restart recover the exact receipt without signing/resubmission. Installed release wallet and public daemon; no build or broader suite repeated. |
| Outgoing offline payment | `device-release-20261019/offline-outgoing` passed all three stages on earlier wallet `2da50ea2...` and public daemon `2ba9a14d...`. Disconnected outgoing signing, cancellation, reservation, accepted-response loss and restart confirmation pass without duplicate submission. Those paths are unchanged by the reference/helper batch. This is payment evidence, not qualification of offline identity/governance actions. |
| Snapshot final payment | `device-release-20261019/snapshot-payment` passed, terminal exit 0/MainPID 0. Payment `0196552243bde81aca0fde9b60ed13936fabd59a5926a7938f7ac697d76db60c` confirmed on both nodes, with exact recipient pending amount and unchanged lifecycle/governance results. Earlier snapshot export/install/crash/reconnect stages were reused. No production change or repeated snapshot sequence. |
| Candidate bundle | Final pre-VPS assembly/archive passes in `pre-vps-release-bundle-20261019`, with unchanged daemon `a10053c7...`, wallet `9d0cd5aa...` and dashboard `94f3ea4e...`. Checksum manifest `ef370bad...`; downloaded archive hash is recorded above. Source privacy filtering and separate build-input snapshots remain applied. Older archives remain historical evidence. Only actual VPS/contact/publication checks remain for this Linux public-testnet scope. |
| Independent installation | VM2 `device-release-20261019/independent` passed the earlier wallet payload `96b3ddd7...`, unchanged dashboard, public daemon identity/restart and native signer startup/safety. Current reference/helper batch is installed and checked on VM1; no new VM2 installation is claimed. Keep the prior host/dependency evidence for its unchanged scope. VM2 remains deallocated; no VM was started for this batch. |
| Remaining release gates | Software qualification for the selected Linux public testnet is complete. Final pre-VPS handoff is recorded above. Actual owner VPS installation, reachable bootstrap, independent-host deployment smoke/restart, verified contacts and trusted publication remain. Local creator backup is verified, not off-machine disaster recovery. Mainnet, indefinite-scale and other-platform work remains in the full roadmap, not claimed complete. |

### Requirement reconciliation, 2026-10-19

Eleven TRST source sections are now signed off. D-6.1/2 use strengthened types
assertions. D-6.4/8/10/11 reused existing
evidence; D-6.7/13 include the affected installed journey. D-6.12/17/18 use
reviewed component assertions and production-source comparisons. This does not
open public distribution. Unsigned sections remain review work, not a count of
missing features.

| Requirement | Behavior and evidence | Scope |
| --- | --- | --- |
| D-6.1 / D-6.2 | u128 raw amounts, 18-decimal units and no-expiry initial defaults pass explicit assertions. | Representation and launch configuration, not every overflow caller or later governance transition. |
| D-6.4 | Graph publication uses the economic LMDB writer; startup loads durable graph rows or the retained legacy snapshot. Both restarted nodes return exact merged origins and holdings. | Persistent graph storage, not indefinite-scale completion or every crash boundary. |
| D-6.7 | New exact installed artifacts permit self-send/receive and preserve pending accounting, age and origin. Helpers agree. | Self-transfer capability; no new all-recovery claim. |
| D-6.13 | Shared linear reference reaches the consumer wallet; core/RPC boundaries, native compatibility, touch display and real installed roundtrip pass. | Display/pricing reference only, not enforced value decay. |
| D-6.12 | Exactly 256 minted inputs combine with preserved amount, age and immediate origins. 257 inputs leave no merge record. Ledger accepts 256 within its wire budget and rejects 257; native/UI selection preserves that capacity. | Input-count limit, not every merge lifecycle/fork scenario or a new maximum-size installed journey. |
| D-6.17 | Tokens point to merge transactions. Graph nodes store immediate inputs; multi-level merges, revision pointers and repeated edges survive the tested roundtrips. | Origin representation, not indefinite-scale storage completion. |
| D-6.18 | A two-level 2/3-raw taint rounds up to one raw. Wide products and pending quotation/delivery checks preserve the selected rounding. | Proportional rounding, not the entire challenge/appeal protocol. |
| D-6.8 | Expired holdings can be consolidated, not voluntarily split. The installed expired-merge journey passes. The matching compiled engine checks transferability before producing either the recipient or change output; no separate voluntary split operation bypasses it. | Merge-only expired bookkeeping. Governance reactivation and fork recovery have separate requirements. |
| D-6.10 | One raw TRST can be transferred. The retained arithmetic-daemon native journey transfers exactly one raw from a merged holding and checks confirmed provenance. Zero is rejected; the validator and engine have no higher minimum. | Minimum amount, not permission to spend expired or revoked holdings. |
| D-6.11 | One raw BRN can be burned. The retained outgoing offline journey confirms a one-raw recipient pending amount after response loss and restart. The validator rejects zero; subsequent checked-arithmetic changes do not alter the minimum. | Minimum amount. Broader offline-action scope remains unresolved. |

Read back the actual driver assertions and saved pass logs, not just result labels.
The active-transfer and outgoing-burn log hashes match their receipt evidence.
The offline driver matches its recorded input hash. Removing only the new expired
journey's imports and wrapper from `native-payments.mjs` reproduces the previous
driver hash exactly at that review. Engine, token, transaction validator and block
format source hashes matched the completed expired-merge build's inputs then.
The later self-send/reference batch changes only the reviewed helper guards,
reference calculation and RPC/display field; expired merge signing and transfer
expiry guards are unchanged. Its affected installed checks pass separately.
Local evidence copies are in `target/validation-evidence/release-reconciliation-20261019`
and `target/validation-evidence/expired-merge-20261019`.

The capacity follow-up changed only `merge_accepts_maximum_sources_and_rejects_one_more`.
It replaces the negative-only 257 fixture with minted inputs and a positive
256-input assertion. Focused ledger/TRST checks passed after a 17.41s bounded
small-crate build; runtime was 0.57s. Receipt
`target/validation-evidence/merge-capacity-20261019/receipt.json` has SHA-256
`202a045bd262f2e73d2f212803b6e1727cd5dc3579a3840f335b9796a9291285`.
The selected daemon's ledger, token, merger graph and node candidate source
hashes match current files. Engine production lines 1-1809 match exactly,
SHA-256 `41337642838731ea98047278f7f544a71fa625ba0174ab7daf24958e1daba6b9`;
only its test module differs. Native selection's saved 256/257 checks pass in
VM1 `manual-merge-20261019/native-corrected`. Its selection code is unchanged.
No release build, installed maximum-size merge or whole-suite rerun is claimed.
Full fork behavior must not keep the input-count requirement open by itself.

For D-6.4, reread the original restart assertions and checked the copied runtime
log against its receipt: `ae7aa59de9be2702549ccd6139dc8ac4bf100d0d0a0ee7405d0d3f6b6b602f3e`.
`graph_storage.rs`, `graph_storage/delta.rs`, `economic_candidate/staging.rs`
and `node.rs` match both the expired-merge build receipt and selected daemon
build receipt. This is reuse of the retained validation-profile restart proof,
not a new restart of the selected daemon.

The types-only follow-up took 6.98s to compile and under 0.01s to run. Receipt
`target/validation-evidence/trst-basics-20261019/receipt.json` has SHA-256
`4715989c01357e43260b46c483f2863985fd13701e94ea3e345afddbe05579b7`.
The earlier launch-default test did not assert expiry; it now does for public,
Test and Dev defaults. Amount checks now include one raw and u128::MAX.
Reconstructing both files without these test assertions matches selected daemon
build inputs exactly. No production source, binary or archive changed.

### TRST self-transfer correction, 2026-10-19

Decision 6.7 explicitly permits self-sends. The installed wallet's signed-block
path already allows them, but `transactions::validation::validate_send` and
`TrstEngine::transfer` rejected equal sender/receiver addresses. Remove those
two guards only. Burn-to-self policy is unchanged. The normal node path still
checks verified spending authority, current transferable origin balance, pending
ownership and single consumption through `economic_candidate`.

The new helper checks cover partial/full self-transfers, unchanged total and
provenance, and refusal of zero, overspending, expired and revoked inputs.
Both pass under the desktop wrapper after a 15.72-second small-crate build.
Receipt `target/validation-evidence/self-transfer-20261019/receipt.json` has
SHA-256 `b7df3281c1eac8591c55a4250187b408e1062675eb533e8cc09bd121c9afeae5`.
Its input list omitted the transitive `store` crate; retain the actual passing
output, but do not treat this receipt as complete dependency-closure evidence for
future reuse. Include `store` in the next changed-source check. Do not rerun this
unchanged scenario merely to replace its manifest.

VM1 `self-transfer-20261019/receipt.json` is pass/0, SHA-256
`0637c68a99fe24e36851fcdc73c05502d2050ab8b14709533f1c149cd391f901`.
The existing native payment driver adds self-send/receive assertions under
`NATIVE_SELF_TRANSFER=1`. It passes on installed wallet `a5ca4520...` and daemon
`e163bc5b...` without a rebuild. Pending value increases by exactly the debit;
receiving restores the original total and preserves age/origin. A second receive
is refused. This is not a restart test or qualification of the unbundled helpers.

The D-6.13 batch now exposes the shared token reference through RPC, native
deserialization and expanded wallet details. No-expiry returns 100%; governed
expiry changes override stale cached state. Amounts are unchanged. Two focused
core/RPC tests pass in `target/validation-evidence/linear-reference-corrected-20261019`.
The first RPC fixture failed because it minted but did not track the token;
that failure remains recorded. Native adapter and installed real RPC checks now
pass in `linear-reference-20261019`. Touch/spacing checks and exact release
packaging also pass. D-6.13 is signed off for this display-only capability.

Browser diagnosis checkpoint: VM1 `linear-reference-20261019/browser` failed
because the test selected a hidden desktop navigation link at mobile width.
`browser-corrected` reached the reference and disclaimer, then failed waiting
5 seconds for the tooltip (7.91-second scenario, 9.75-second total). Both jobs
are terminal; neither needs a protocol build. The test used a mouse click on a
touch viewport. Existing help tests use `tap`; Radix treats touch and mouse
pointer movement differently and closes help on ancestor scroll. These are
candidate causes, not a confirmed application defect. The next discriminating
check uses a real tap and captures trigger/tooltip state and a screenshot on
failure. Keep the timeout and assertions unchanged; do not patch shared help
without evidence. Prepare from the latest `browser-corrected` receipt.

The discriminating touch run passes (4.13-second scenario, 5.70-second total).
It verifies 50%, 0%, 100%, missing/invalid fallback, readable help, unchanged
face amount and no horizontal overflow at 390px. Shared HelpLabel code is
unchanged. Earlier mouse-click failure remains failed; this pass establishes
the intended touch interaction, not its exact earlier failure mechanism.

### Manual combination review, 2026-10-19

Decision 6.8 permits consolidation of expired TRST, but not splitting it. The
existing engine implements this rule. The consumer wallet lacked a manual action;
its automatic policy only selected active batches. `MergeBatches` now offers a
paginated picker and frozen review with exact amount and earliest-expiry warning.
The native command checks the reviewed holding IDs, amounts, status and expiry
against current node data before using the existing signed-action journal.
Revoked inputs and mixed active/expired selections remain rejected.

VM1 `/home/burst/manual-merge-20261019/browser-result.json` is pass/exit 0.
Its focused browser test took 7.08s and uses mocked IPC, not real signatures.
The desktop browser attempt failed before app assertions because Chromium could
not create threads under the existing task cap. No desktop limit was raised.
The first native compilation failed because `TxHash` has no `Ord`; the duplicate
guard now uses its existing `Hash` implementation. That failed receipt remains.
`native-corrected/native-result.json` is pass/exit 0: compilation 5.04s and three
review-selection tests 0.03s. The corrected run used the failed receipt's preflight.
The frontend package stage passed in 22.95s. The release wallet compiled in 1m55s
and bundled successfully. Its recording step initially ran from the candidate
directory instead of the original working directory. Recording was completed
from the original directory against unchanged inputs, without rerunning the build.
The original service exit 1 and recording diagnostic remain; build result and
completed receipt record the successful artifact production.

New wallet package SHA-256 is
`357b01dbaed90d5e64f4bed49d313ed350bb9b4a790cb6a48b405d0a7275611b`;
raw executable is `f42ef91602c3901621f4f9966d9529972c3aabfcac4f4d1d9e52d608c28fbc11`.
Exact installation passed in 2.676s, installed payload
`ba89a73c108539b0dfe3ef45426a702d1572b3f8c98cfa887ff71c8d3ba91585`.
The existing native payment driver, with `NATIVE_MANUAL_MERGE=1`, passed in the
10.010s consumer job. It checks unchanged holdings after stale-review rejection,
successful active combination, consumed-selection rejection, merged transfer,
original creator provenance and locking. Its receipt SHA-256 is
`d2a604438ec5d13d76f6ea79af3515fc6d1eacb271205f3efe966a273628c170`.
That package is historical evidence. The current selected wallet and daemon are
now included in `arithmetic-release-20261019/bundle`.

The retained positive expired-consolidation test was found in historical logs.
Recent economics changes make those logs insufficient for the current source,
so `expiry-proof` reused the matching arithmetic-review node executable and ran
that one existing test. It passed, with committed/restored amount 50, original
age 100 and zero spendability. No compiler or new fixture. This is component
integration evidence. The later installed journey closes that separate gap.

VM1 `expired-merge-20261019/runtime-receipt.json` is pass/exit 0, SHA-256
`6bccb6db163e409d607b8d32e8da90235a595151b6605d39cfc8e5ad0efb5625`.
The exact installed wallet combines two expired holdings totaling 18 raw, keeps
their original age and burn origins, and refuses spending the result. Review
cancellation restores focus without changing the account head; double-clicking
confirmation produces exactly one block. Both nodes retain the exact portfolio
and provenance after restart. This uses a controlled clock and validation-profile
nodes. It does not qualify public expiry-governance timing. Production binaries
were unchanged; the one test-only compilation and 9.64-second journey are terminal.

| Changed dependency | Evidence retained | Remaining check |
| --- | --- | --- |
| Manual picker and review | Browser contract, TypeScript, exact installation and installed expired review/cancel/focus pass | Reuse unless these dependencies change |
| Native merge command | Guard tests, installed active merge and expired signed merge/restart pass | Reuse unless command, journal or merge semantics change |
| Automatic grouping, device grants, history and transport | Existing scoped receipts | No change from this feature; no repeated broad journeys |
| Node merge handling | Arithmetic qualification and expired two-node recovery pass; merge production logic unchanged | No additional build or repeat scheduled |

### Arithmetic review, 2026-10-19

Lifecycle reuse review, 2026-10-19: extracted the four changed files directly from
VM1 `device-release-20261019/bundle/candidate/sources/daemon.tar.gz` and checked
each against `device-certified-20261019/build-receipt.json` before comparing it
with the selected source. All four archived hashes match:

| File | Retained SHA-256 | Reviewed production change |
| --- | --- | --- |
| `node/src/node.rs` | `76dcc0fe0c1ed6073a9a6281cab440c70d3b95759b514cb1c5e355c46de63a06` | Checked accrual and checked BRN odometer addition report overflow instead of saturating. No settlement/restoration change. |
| `node/src/governance_commit.rs` | `4411a3e2b17f75b3941b2011d0f45019b9aab8404d88902b10b741a90e3d97fc` | Removes balance calculation for the unused `_proposer_brn_balance` argument. No expiry activation or identity restoration change. |
| `trst/src/engine.rs` | `7442abaf5e785e1854b0e72d56ad9fe48fce72c4313c6026cfe80e5e249fbd9c` | Removes self-transfer rejection in the public helper. Revocation, origin indexing and restoration remain identical. |
| `trst/src/token.rs` | `35887fb00adf93db333f4ed6d53c8566346414163525ee1d52e09c7b9c87f1d0` | Corrects no-expiry/current-expiry display reference; transferable amount and restoration state rules are unchanged. |

Added tests are not product changes. The retained native driver also matches
its receipt byte-for-byte. This closes D-6.15's impact review without replaying
the panel or payments. It does not turn resumed evidence into an uninterrupted
journey or close independent authority/fault requirements.

Release daemon `e163bc5b4d1aedf5190977f19c14af22f181f59fcd0d6b2ac7f2ae823ca111e0`
built in 3m56.833s. Public identity, initial 1 BRN/hour, restricted authority and
Dev/Test/Live restart passed in 13.618s. The installed wallet's positive burn,
endorsement, receipt, balance and manual-merge/provenance journey passed against
that exact daemon in 10.863s. Evidence is in VM1
`arithmetic-release-20261019/{daemon,identity,consumer}`; every unit is terminal.
Consumer receipt SHA-256 is
`7ab9ae4159104deabfe63c559e35292d9b08abdda689cb9d340eca6ab6b7f531`.

Input comparison against the previous public daemon found production changes
only in `node.rs`, `governance_commit.rs`, RPC handlers and wallet balance helpers.
The other source difference is a standalone restart-payment example. Direct diff
review confirms the arithmetic changes below. Transport, authority, snapshot and
merge-engine inputs are unchanged; their existing scoped evidence is retained.

Final frontend review found the same requirement incomplete in `computeBrn`:
checking only the net balance allowed deductions to hide accrued-BRN overflow.
The UI now checks accrued BRN before subtraction, matching the native calculation.
All eight BRN tests pass, including the exact maximum, zero after a full burn,
overflow hidden by burns/stakes, and overflow across rate segments. The focused
rendered-dashboard check passes in 3.233s and shows an unavailable balance, not a
number. The corrected frontend package and exact installation now pass. Native
Rust is unchanged, so the earlier installed signing/merge/payment assertions
are reused rather than repeated. Build receipt SHA-256 is
`a275b64b6d806f14c8577ebe57a73d5143e68d5e5e424dc307e8a01d7f0ac52a`;
installation receipt is
`9e0a1df6a6de7270ddb440e4c21082730cd015f970871f31f98c4a023da9d7ef`.

D-5.12 explicitly requires an error on BRN overflow. Core checked accrual already
existed. The gaps were callers: account-info/balance and test-only burn RPC used
the zero-on-error convenience method; transaction conversion and test-only RPC
spent counters saturated; public wallet balance helpers returned zero or a
saturated estimate on accrual overflow. These callers now propagate checked
errors. Wallet display still floors deductions beyond accrual at zero. This is
not a new economic cap, rounding rule or change to valid balances.

Proposal submission was also computing a balance that the governance engine
deliberately ignores. That computation is removed, not turned into a new guard.
Decision 20.1 charges endorsers, not proposal authors. The new node regression
keeps a verified person's proposal valid at both zero and overflowing accrual,
and verifies that submission leaves BRN state unchanged. The existing endorsement
test still proves its burn and uniqueness marker commit together.

Desktop checks used the required wrapper. Final commands were
`cargo test --locked --offline --profile validation -p burst-wallet-core --lib balance`
and `cargo test --locked --offline --profile validation -p burst-rpc --lib brn_balance_tests`.
Wallet compilation took 1.69s; 16 selected checks passed in 0.13s. Final RPC
compilation took 1.73s; its zero/valid/overflow check passed. The earlier cold RPC
build took 42.50s; it was not repeated after unrelated changes.

VM1 `/home/burst/arithmetic-review-20261019` holds the single combined node build
and focused runtime receipts. Build fingerprint is
`c8fa143e5a06a1e6b5e9daacd8d9a28c55af27f67cc199f48106614bab1c9031`.
Runtime receipt SHA-256 is
`d78b3d655d05848d891006aceaf9a196004af40e729c32817414eb72be1ded33`.
Both saved results are pass/exit 0. That earlier build reused the stable validation target;
it did not itself produce a release artifact. The later release stage is recorded above. Final doc-comment-only
changes in wallet helpers do not change these assertions.

| Changed dependency | Affected evidence | Reuse / remaining work |
| --- | --- | --- |
| RPC and node arithmetic handling | Balance queries, burn/endorsement/stake construction | Focused checks and affected positive release-daemon operations pass. |
| Wallet library balance API and frontend accrual | Callers report overflow before deductions | Native compilation, BRN boundary tests, rendered unavailable state and corrected package installation pass. |
| Proposal staging | Removed unused balance calculation | Positive zero/overflow proposal regression and existing paid-endorsement atomicity pass. Do not repeat the full voting journey just for this removal. |
| History, device grants, transport, merge selector | No behavior changed by this batch | Retain their scoped receipts. No new NAT, takeover or 105-payment retention run. |

### Source distribution review, 2026-10-19

P5 source distribution review removed `.cf2/log` and `deploy/validation-peer.env`
from the daemon and dashboard source archives. They are development state, not
build inputs. Original archives and all source files remain unchanged; no user
data was deleted. The exported source manifests verify against the original
file hashes. Wallet source selection was already limited to build inputs.
The source-clean assembly unit exited 0 in 2.558 seconds; its archive job exited
0 in 2.119 seconds. No binary rebuild, installation or protocol rerun occurred.
`tools/build-linux-candidate.sh` now omits agent state, environment files, logs
and key-file paths from future source inventories. The focused source-inventory
regression passes under desktop limits and checks retained build inputs too.
This path filter is not a claim that arbitrary source text has had a security audit.

### Wallet requirement review, 2026-10-19

#### History retention and explicit future scope

D-31.4 local retention is implemented, not a new build task. On VM1,
`wallet-cursor.kj87aHYA/retention-overflow.log` records 105 additional confirmed
payments, exactly 100 retained receipts, no unresolved draft/orphan marker and
exact ledger deltas. Its SHA-256 is
`cf7524d3bdfee9ec47c0c15467c1adb2f06511c6348d620434f938b372d98e82`.
`wallet-migration.DzVkIrp5/run.log` records both successful native startups,
preserved receipt bytes/mtimes and a still-reserved unresolved draft. Its SHA-256
is `8e50a72cbf070cbc981eb301219281390366cd226eed941ddbfd88813fa3cb5e`.
The installed Activity stage passed in `wallet-cursor.kj87aHYA/acceptance.log`;
that combined job later failed on a receipt-filename assertion and remains failed.
Its successful Activity stage is not a passing whole-job claim.

Compared with the current native workspace, retention and migration source files
and the Activity page are byte-identical. `offline.rs` and `offline/storage.rs`
have only test-fixture grant-field additions; their production logic is unchanged.
The v7 block format is a relevant dependency change. The focused current
`offline::storage::tests` group passed all nine cases in 2.88 seconds, using the
existing validation build (Cargo preparation 0.17 seconds), under desktop caps.
It exercises over-limit retention, independent accounts, unresolved drafts,
interrupted cleanup, migration conflict/reopen and exact action content with
current signed blocks. Current installed outgoing recovery also archives its
exact v7 journal. No 105-payment journey or old migration fixture was regenerated.
Reproduce the focused check with `bash tools/limited-run.sh cargo test --locked
--offline --profile validation -p burst-wallet-core --lib offline::storage::tests`.

The local-history portion is supported by this evidence. The server-side portion
was reviewed separately; client pagination alone would not establish it.
That server behavior is implemented too: `handle_account_history` joins indexed
entries with `historical_lookup::Reader::account_page`, rejects inconsistent or
incomplete pages and checks that the account head did not change during lookup.
The reader traverses snapshot-authenticated witness chunks without installing
the returned blocks. Its existing `consumer_history_lookup_proves_membership_without_installing_blocks`
test checks positive pages, exact signed bytes, busy/corrupt/wrong-network failures
and unchanged storage. Retained two-host acceptance is indexed under
`target/validation-evidence/history-final-20261016/`. Its saved `run.log` passes
complete paginated history against the archive, with unchanged balances/authority
and retained lifecycle/consumer state. These gitignored logs exist locally;
ordinary `rg --files` hid them. The workflow now specifies scoped ignored-file
searches before treating evidence as missing.

Current history-reader changes add bounded queued lookup admission. Witness and
v7 block changes required current component evidence. The saved node-library build
matches current inputs except three standalone `wallet_core/examples` programs,
which are not compiled into that library. Its executable hash matches the build
receipt. The existing exact historical-page test then passed in 0.976 seconds,
with positive byte-exact pages, invalid/busy/corrupt-base refusal and no storage
changes. VM1 `device-release-20261019/history-review` records pass/exit 0;
receipt SHA-256 `cb51e99818b6e02b946ed22a1349a3f766346350641314b12f05d02c25739e2c`.
The capped unit is terminal, MainPID 0. No compiler, fresh ledger or VM2 start.

D-31.4 is signed off for bounded wallet-local retention and retrieval of older
authenticated history from an available archive. This combines unchanged installed
behavior with current format/proof checks; it is not a new public-binary two-host
run. C02 longevity and broader pruning/storage policy remain separate open work.
No claim of archive availability during partitions or unlimited-history speed.

D-31.11, D-31.12 and D-31.13 are explicitly later work in the owner's selected
answers and C23. The inventory now marks multisig, timelocks and escrow excluded
from this initial testnet, not implemented or removed from the roadmap. These
scope corrections do not relax deadlines, expiry, staking or recovery behavior.

#### Storage, receive policy and device coordination

This review closes D-31.1, D-31.5, D-31.6 and D-31.8 for their selected behaviors. It does
not sign off all wallet functionality or other platforms. No runtime was repeated.

- D-31.1 selects password-encrypted JSON. `wallet_core/src/keystore.rs` implements
  Argon2id-derived AES-256-GCM with random salt/nonce. Installed
  `wallet/tests/native-wallet.mjs` passed save/load, identity recovery, wrong-password
  rejection, non-overwrite and mode 0600. This is storage behavior, not a crypto audit.
- D-31.6 selects 24 recovery words. The same installed driver creates 24 words,
  locks the wallet, imports those words and compares the exact address. The handoff
  distinguishes recovering keys from preserving pending journals and device grants.
- D-31.8 selects querying a node. `wallet/src/hooks/use-wallet.ts` refreshes native
  RPC-backed state; `wallet_core/src/wallet.rs::NodeClient` sends those requests.
  Installed native payment/restore flows query actual node state. Certified readback
  then matches exact verification, receipt and provenance state on all three nodes.
- D-31.5 selects automatic receive by default with configurable reject rules.
  The store defaults to automatic receive; the automation hook invokes the native
  signer only for the unlocked selected wallet. Installed `native-payments.mjs`
  proves allowed-sender acceptance and held payments. `native-groups.mjs` proves
  original-creator checks behind merged transfers, any/all rules, explicit outage
  fallback, malformed/negative holds and manual acceptance. `native-automation.mjs`
  proves cancellation stops further receives. The consumer result and terminal
  logs were read back from VM1 on 2026-10-19; all three drivers passed.

D-31.2 is now verified for the corrected Linux wallet. The decision's
explicit 90-100-day example fixes the denominator: a merge may lose at most 10%
of the longer remaining lifetime. The source now follows that boundary instead
of 90-99 days. Both portfolio and native automation use the same selector and
0-100 slider mapping; origin timestamps no longer supply a conflicting policy.
New focused assertions cover the exact example, one-second-outside boundaries
and anchor-based grouping without transitive range expansion. Existing installed
merge/provenance evidence remains valid for the old package, not this correction.
The focused core merge group passed all 14 selected tests under the desktop
wrapper: `cargo test --locked --offline --profile validation -p burst-wallet-core
--lib auto_merge`. Build time was 57.76 seconds; test execution was below 0.01
seconds. The capped service invocation was `9f54b1c283da419c93039e632d59d488`.
The frontend build passed in 23.122 seconds; the release wallet build passed in
1m59.191s. Exact installation passed in 2.693 seconds. The existing installed
`native-payments.mjs` driver passed in 10.674 seconds, including receipt, default
merge, balances/provenance, merged transfer and lock. Profile
`merge-consumer/profiles/burst-native-wallet-5mZlzx` is retained on VM1.
The runner's new explicit `--payments-only` option selects that existing driver;
the default eight-driver selection is unchanged. No fresh certified or cross-host
claim is inferred. Daemon, takeover and network dependencies are unchanged.

All records are under VM1 `device-release-20261019`. Receipt SHA-256 identities:
frontend `5ba4c065b46a398a975ff856bcd7c015fe48e654c39ba5e092a85f97b7f53ab5`,
wallet build `19cbb268bdab36af459981d3e9ba002375a4e8f8d54d14fce29fa2431a1d15f1`,
installation `a922226db8267fd046f2062c8cf521feba38068fc6c83f1be02a58799d8de5ff`,
native merge `32c7a00ffc2c19a308cbb3b909dd9c428d95a6fbf0fcba7addc19859436fadc3`.
Saved results and terminal logs were inspected; all are pass/exit 0.

D-31.10 is signed off for delegated work through the configured node. Source
review follows `wallet_core/src/wallet.rs::work_generate` to offline preparation
in `signed_client.rs`: only a public root goes to the work RPC, and the wallet
checks the returned root and nonce against the required threshold. The installed
offline driver prepares, disconnects, signs without any further RPC requests,
then submits the valid block. Its three saved stages pass in
`device-release-20261019/offline-recovery`; receipt SHA-256
`213210ed95fb260d4adbfef8d2a7083dd4e4caabf516a04c08ed79cee0051f2b`.
Result and terminal stage logs were read back from VM1 during this review.

D-31.9 outgoing payment now passes on the current installed package. The existing
`native-offline.mjs` driver completed disconnected signing, restart, one accepted
submission with a lost response, and another restart confirming the original
one-raw-unit receipt without signing or resubmitting. The VM1 result is pass/0;
all three terminal stage logs were read. Evidence is in
`device-release-20261019/offline-outgoing`, profile `burst-native-wallet-GvECYn`.
Receipt SHA-256 is `84aecd1813f44683342d0f1837a12baee48c5c6ab2b77ab4d0d479c2a1d03216`.
The run took 13.037 seconds. No binary was rebuilt.

D-31.9's scope is now settled by the owner: identity/governance are included.
The separate typed action context is implemented alongside the narrow payment
context. The current handoff records positive core/preparation checks, installed
endorsement and proposal recovery, eight governance actions, and all three TRST
offline paths. Those results replace the former missing-implementation claim;
they do not turn every context into an independently qualified installed journey.
Remaining action-specific qualification belongs to D-31.9, not a new requirement.
D-31.7 is
also implemented rather than absent: portable backups do not carry local device
bindings, and takeover activates the local binding only after confirmation.
The saved two-host `contest-resume` result is pass/exit 0 with exactly one active
profile after competing handoffs. The dependency review below closes its mapping
to the frozen package without repeating the contest.

#### Device-grant dependency review, 2026-10-19

D-31.7 is signed off for the settled cooperating-device model. The read-only
profile remains usable; restored keys do not silently adopt the network's grant.
Explicit takeover must confirm before its local role is activated. Stale drafts
remain recoverable, and a competing takeover resolves to one canonical grant.
This is not a guarantee against a malicious holder of the root key.

- The cross-host receipt records wallet SHA-256 `1bd7cdf12d074431c89858f89aac03ab1f544c9d91847c139b9124eb25dea86a`.
  That exactly matches `device-handoff-20261019/wallet-recovery/native-receipt.json`.
  All 327 shared wallet application, bundled frontend, Rust dependency and manifest
  inventory entries match the release build receipt. The release adds packaging
  tool/package manifests; no shared application input differs.
- The cross-host daemon SHA-256 `35692174fd1b321b6721ec830b3dafaa9b987fc34fdc2e2ea03500fe3dea2530`
  matches its saved build receipt. Of shared protocol inventory entries, 931 match.
  Differences are two node test files, two wallet examples and wallet-core signed
  client code. Wallet-core is a node dev-dependency, not part of the daemon runtime.
  The release also records documentation/tooling absent from the earlier inventory.
  Profile, Test feature and creator-key changes are qualified by the installed
  consumer/certified and public-identity checks; they are not called identical builds.
- `ledger/src/state_block.rs::validate_signing_grant` binds each successor to its
  authenticated predecessor. Live economic admission and confirmed replay call
  it. This unchanged code has no feature-specific bypass. The native takeover
  driver asserts unchanged identity/balances, durable response-loss recovery,
  stale-draft refusal and exactly one active profile after concurrent handoffs.
- Installed v7 consumer, watch-only/signer guards and exact-action recovery provide
  the packaging-boundary evidence. No new installed two-host or timing claim is made.

The capped VM1 unit `burst-device-review-reuse` checked the wallet-recovery build,
contest and release-wallet build receipts using `tools/validation-reuse.mjs check`
from each receipt's original working directory. All saved outcomes were pass/0;
all integrity checks passed. Fingerprints respectively are `24b848fb7109cf438f8ddacc51b0d6f294af05972c7765770bc4e06846a11ab0`,
`c2bab54a0343a3e90a9806159895277e1a37a5af1fcda80506e3cd36653cda10`
and `afd365ccf197716b0a3ece2513fa8acb53604728e29af124f4d92edc633a5481`.
The unit exited 0 in 2.235 seconds, with 3.2 MiB peak memory and no swap.
No compiler, VM start or runtime scenario was needed.

Evidence on VM1: `device-release-20261019/consumer/receipt.json`, SHA-256
`964bc50e8731ad28f8ad2d2d3e9a6835e45428888d78da6f0318a224da8dd1de`,
and `certified-readback/receipt.json`, SHA-256
`e6331538f3258d8ae0dd81491e17e48d5b021199f1bb395a513104d7a6780d1e`.
Both saved results are pass/exit 0. Exact binaries are listed above. The recorded
commands are each run directory's `run.sh`; they use existing native drivers,
one-CPU/3-GiB/no-swap units and preserved inputs. Reproduction needs a new evidence
directory and receipt preflight, not overwriting these write-once records.

### Retained development history

The records below retain earlier results and failures. They are not a queue to
repeat completed work. The current table above identifies the final artifacts.

**Device-grant compact-history retention is verified.** The added focused case
authenticates a snapshot after a handoff and a later account block, constructs a
sparse projection without the handoff body, and proves the retained head carries
the grant. A current-grant successor replays to exactly the full-history state;
a freshly signed stale-grant successor is refused. The sparse projection is not
mutated. This closes the specific retention uncertainty, not final installed
package retirement acceptance. Source review confirms grant validation reads the
immediate predecessor's committed grant, not a lookup of the original handoff.
Evidence: VM1 `/home/burst/device-retention-20261019/snapshot-{result.json,receipt.json}`,
`snapshot.log`, fingerprint
`663c259d735ca36dfd5dca2c481add66dbaa34a1a89ef2686b68540ca3299850`.
The build took 68 seconds; the exact runtime case passed in 0.79 seconds.
Both units are terminal exit 0/MainPID 0; the desktop ran no compiler.

The certified Test fixture daemon also built successfully in 37.43 seconds,
validation profile with `private-testnet` and the existing disposable creator
public key. Evidence: `/home/burst/device-certified-20261019/build-{result.json,receipt.json}`,
fingerprint `7178b81d0b063cd0f566cd672d1c6ab52f283dcc33d9c4de7a14f18cecfdaf8c`.
Unit `burst-device-certified-build`, invocation `a6afcf6ab6864a77bd72eb36b42c61d3`,
is terminal exit 0/MainPID 0. The prior Dev daemon lacks the certified Test feature;
the older certified daemon/baselines use incompatible v6/schema2. This justified
one feature build, not rerunning unrelated Dev or transport tests. No real creator
seed was read or supplied; the unchanged native wallet remains reusable.

Certified driver reconciliation: restoring recovery material now imports without
implicitly taking over the account. The restored-action check requires both
ordinary signing and a new takeover to be refused while its exact journal is
reserved. The unopened-account exclusion case now exercises enrollment's first
Open, then explicit re-enrollment and receipt after proven exclusion. Mandatory
enrollment already opens an account before ordinary offline receipt signing;
the separate positive offline first-receipt journey remains verified above.
These changed certified drivers pass syntax/diff checks, not runtime acceptance.
The existing validation runner and onboarding example now offer a bootstrap-only
stage: govern the disposable rate, establish the same 49 helper identities and
seven opted-in verifiers, record that consumer onboarding has not run, then stop.
The default full journey is unchanged. Both helper examples compiled successfully
in 25.82 seconds (`device-certified-20261019/helpers/{result,receipt}.json`).
The first bootstrap run passed governance on all three nodes but stopped before
helper creation because signing readiness was still `peer_differs`. Preserve
`bootstrap-{result,receipt}.json` and `baseline/burst-validation.bdEGRs49`.
The first resume's readiness check mistakenly queried `protocol_params`; that RPC
has no `online_signing` field. Live read-only `params_hash` probes on all three
nodes instead showed `peer_matches` and the same `bd146c75...` payment epoch.
This is the two-attempt diagnosis checkpoint: fix the runner's RPC name, retain
the same 90-second budget and production guard, and resume only the helper stage
from the saved governance ledger. No daemon/helper rebuild or governance repeat
is needed. The corrected `bootstrap-ready` attempt passed: all 49 helper identities
are verified on all nodes and seven opted in; the consumer subject remains fresh.
Receipt fingerprint `6138f9dee6b2bcadfa88f860b71962d3439602d64b71fbe430119486d8bb9c6c`;
unit `burst-device-certified-bootstrap-ready` is terminal exit 0/MainPID 0.
Retain `bootstrap-ready/baseline/burst-validation.retained` unchanged for reuse.
The first native consumer run (`consumer`, invocation
`140b269e22b748148d261d979d0dea0c`) is terminal failed, exit 1/MainPID 0.
It stopped at founder enrollment before either consumer wallet was created:
checkpoint catch-up changed readiness between preflight and signing. Its retained
profile has no signed journal. The shared fixture helper now waits for readiness
and retries that specific pre-sign refusal only with no pending journal and an
unchanged reviewed head/grant/rules; a changed head, pending action or other error
still fails. Four focused harness tests passed under desktop limits in 0.41s.
The affected retry (`consumer-ready`, invocation `3aa7ff307f7c4a80902e17ae80d09166`)
is terminal failed, exit 1/MainPID 0. It passed native creation, explicit enrollment,
encrypted restore, receipt/delegation, participation, three paid endorsements and
all seven reviewed votes. Verification completed. Its first post-verification
payment remains journaled as `e6bff769daf3b78671ced1806af14957875b0b92ba64f29bfe15d1b53fff748b`.
The generic UI wait is 200 × 100ms while core confirmation can wait 30s; logs show
ordinary votes sealed during checkpoint installation. This is a diagnosis, not
permission to increase waits or claim the payment passed. Preserve the exact
profile `consumer-ready/profiles/burst-native-wallet-DT1WXz`, backups, prefix and
node directories. Do not repeat funding, endorsements or votes.
After the two-attempt checkpoint, `payment-diagnosis` passed a read-only check of
a copied source node: no canonical successor, certified exclusion under installed
epoch `dc590e02ed5005c26a56da89eb93065cf27771623f89beb425476cd34b7a699e`.
Fingerprint `98d9e36d383d14a5bea53f022e18e0aacf57b371702b294babdd4dbc7447f637`;
unit is terminal exit 0/MainPID 0. No signer or peer was supplied.
The existing onboarding driver's post-verification recovery stage **passed**:
it verified the original context and seven votes, resolved the exact excluded
journal through UI, then completed a separately reviewed payment, TRST transfer
and receive. Unfunded/disconnected/locked signing was refused. All three nodes
agreed on the subject, context, confirmed payments, provenance and exact balance.
Receipt `consumer-recovery/receipt.json`, fingerprint
`4726d16a844bcd0dac4867da7f31b23712ba1adae8c4a9edda96e90bc62267f1`,
is pass/exit 0. This is composite development/recovery evidence, not uninterrupted
fresh onboarding or installed-package qualification. Retain `consumer-recovery`
nodes, `onboarding.json`, `resumed-data`, and profile `burst-native-wallet-bhTZqR`.
Unit `burst-device-consumer-recovery`, invocation `cd1c3051fba0453e9257f38e129b9eaf`,
is terminal/MainPID 0; its exit 1 occurred only in receipt finalization after the
scenario passed. Running `finish` from the recorded preflight directory completed
the receipt and `check` passed without rerunning any scenario.
Its initial preflight failed before a start record because the prior receipt's
relative log paths were checked from the wrong directory; running preflight from
its recorded directory fixed that environment error without executing a scenario.
Next use the retained consumer state for affected lifecycle/governance checks;
do not recreate verification. The daemon
`6e4006fc...` and wallet `1bd7cdf1...` remain unchanged; no production rebuild.
No fresh complete-consumer or installed-package pass is claimed. VM2 is deallocated.

Next-driver preparation: governance now expects founder + 49 helpers + the
native consumer (51), not the removed duplicate CLI consumer (52). Actual quorum
is still calculated from the electorate. Direct voters are restricted to the
49 existing helpers. Read-only example verification uses `inspect_signing_state`
so it does not claim the founder's native device grant; signing modes retain the
binding check. Both readback examples compiled in 1.65 seconds; saved binaries and
pass receipt are in `helpers-readback`. No daemon/application build was needed.
The first affected lifecycle run (`lifecycle`, invocation
`298aa9e8b1b344dd82dd1c97f27632b8`) is terminal failed. Its challenge
`a4c6309a36a34d82524265fee0e5cbb7df6bbfa60777ffdd7edc74f11ce2fcfe`
confirmed, but spending was refused because the preceding consumer negative test
left its oversized rejected Burn `249fa5ba...` reserved. Preserve this intended
journal protection; do not delete the draft or repeat the challenge.
The existing challenged-payment recovery stage (`lifecycle-recovery`, invocation
`da72b38528f04dd384c80bd4b694893e`) resolved that exact exclusion through UI.
It then failed waiting for `Go to Dashboard` on the second encrypted restore,
before the new payment or challenge votes. Both units are terminal exit 1/MainPID 0.
After this two-attempt diagnosis checkpoint, `restore-diagnostic` passed two
encrypted UI restores on the post-exclusion profile with no account mutation.
Fingerprint `9fec95d6adcec60011b004e9795f76cd8e84203dceaa36812decf40a33e658cb`;
unit `burst-device-restore-diagnostic` is terminal exit 0/MainPID 0. The earlier
unlock stall remains unexplained, not declared fixed. The shared restore helper
now captures only safe route/locked/button/error state if that failure recurs;
no page, input contents or recovery words are logged and waits remain unchanged.
The existing lifecycle resume branch accepts completed exclusion only with the
exact archived Burn, no pending journal and current certified exclusion. It skips
the already completed recovery/extra restore, not challenge or payment assertions.
`lifecycle-continue` **passed**, unit `burst-device-lifecycle-continue`, invocation
`7e36bcd90afe4c649c65802568724389`, terminal exit 0/MainPID 0. It completed retained
exclusion, positive during-challenge payment/receive, seven revocation votes,
origin-specific invalidation, appeal, seven restoration votes, revoked-state
journal reconciliation without resubmission, and restored payment/receive.
The existing readback helper verified exact verification state and balances on
all three nodes. Preserve `lifecycle-continue/{result,receipt}.json`, native profile
`burst-native-wallet-F5SqWv`, `resumed-data`, report and ledgers. This closes the
affected resumed development lifecycle, not uninterrupted or installed acceptance.
Receipt fingerprint `3300172240f58cd2613457e9c75ab1319ef60239635ad026425fc5217497a30f`.
The existing native governance driver **passed** on this completed ledger in
`governance`, unit `burst-device-certified-governance`, invocation
`12571e713d8048f8977b019cbf99c9b0`, terminal exit 0/MainPID 0. Numeric activation,
multilingual amendment adoption, 21 direct/delegated ballots and post-count
delegation revocation passed, with exact readback on all three nodes. Receipt
fingerprint `e297f9d387cd4ec712b21fdd560048ece15f33467c21f1c25de286ee5e8c2bdb`.
Do not repeat helper setup, consumer verification or lifecycle.
Copied ledgers, backups and progress remain intact.

Fixture-only follow-up: the readiness helper now takes an optional Node-side RPC
reader, preserving WebKit's no-direct-node-fetch CSP for its existing injected
caller. Five bounded helper checks pass (0.68s), including no retry of pending,
changed-head or unknown-error actions. No application/CSP change or broad rerun.
Upcoming final-driver checks also make rejection evidence explicit: disconnected
preflight runs before the deliberately oversized payment and must leave no
journal; locked signing must fail specifically as locked. Any oversized-payment
journal is reported as `rejected_payment`, not left implicit for the next stage.
These stricter assertions have syntax review only and do not retroactively expand
the completed consumer pass. Enrollment-exclusion readback now distinguishes the
new Open → ChangeSigner → Receive chain from historical single-Open receipt
evidence, preserving both branches. The enrollment-opening recovery **passed** in
`opening-recovery/{result,receipt}.json`: locked restart retained the exact draft,
duplicate signing was refused, ordinary governance certified its exclusion, and
explicit re-enrollment followed by receipt produced the expected three-block
chain and exact balance on all three nodes. Unit
`burst-device-opening-recovery` is terminal exit 0/MainPID 0. No daemon or app
rebuild was needed. This qualifies development recovery, not installed packages.
Remaining gates: exact final
package/recovery qualification, creator backup and bootstrap/contact handoff.

**Cross-host device coordination now passes for the development candidate.**
Two existing hosts ran independent connected Dev nodes and native wallets. A
enrolled and saved an unsent draft; B restored the same identity without adopting
authority, explicitly took over with its accepted response dropped, then recovered
that exact handoff after both nodes restarted. A retained its stale binding; its
draft was refused and resolved without replacement. These four stages passed in
`/home/burst/device-crosshost-20261019/prepare.log`, `stale.log`, and VM2's
`takeover.log`/`recover.log` (also retained in `host2-evidence.tar.gz`).
The combined first result remains failed: its final contest barrier compared
signed hash byte arrays with RPC hex strings before forwarding either request.
The next attempt failed before launching a wallet because systemd unloaded the
stopped transient node units. Its diagnosis is retained in `contest-retry`.
Neither failure caused a build or repetition of the four completed phases.

Only the contest reran after diagnosis, using the same retained ledgers. Two
distinct signed handoffs referenced the same account, predecessor and grant before
either submission. Both peers agreed on one confirmed grant; exactly one native
profile owned it. The loser recovered its reservation and was refused ordinary
signing. `contest-resume/{result.json,receipt.json}` is pass/exit 0, fingerprint
`c2bab54a0343a3e90a9806159895277e1a37a5af1fcda80506e3cd36653cda10`.
Both native logs/results and per-host profiles are retained. Positive ordinary
current-grant signing has separate native consumer and live-admission evidence;
it is not inferred solely from the contest's active-role flag. This is composite
development evidence, not installed-package acceptance or malicious-root exclusion.
Both hosts used identical saved daemon `35692174...` and wallet `1bd7cdf1...`;
there was no application build or new VM. Combined per-host slices retained
1 CPU/3 GiB/no swap/256 tasks. Temporary jobs are terminal and ports
28176/28177/28190 are closed. Azure confirms VM2 is deallocated after evidence capture.
Next: affected certified journeys and final installed recovery/packages, plus
creator/bootstrap handoff. Do not repeat device coordination without a relevant
dependency change.

Current C18 recovery check: the first native offline receipt run failed after
its disconnected-signing stage passed. The saved-action client attempted to
reserve its existing journal again before submission, and receipt maintenance
also parsed root-level device bindings as payment drafts. Evidence remains at
`/home/burst/device-handoff-20261019/offline-{result.json,receipt.json,log}`;
the profile is retained at `offline-failure-profile`. Neither failure is a pass.
Saved submission now reuses the existing reservation without rewriting the draft.
Bindings now live in `offline-payments/devices`, with exact-byte, fail-closed,
restart-safe migration under the profile lock. The focused saved-submission
regression and both migration/cleanup regressions pass under desktop caps.
The incremental embedded wallet build passed in 13.07 seconds; its receipt is
`wallet-recovery/native-receipt.json`, fingerprint
`24b848fb7109cf438f8ddacc51b0d6f294af05972c7765770bc4e06846a11ab0`.
The affected three-stage offline journey now passes; unit
`burst-device-offline-recovery`, invocation `16b81aa035bb4f0c86e4e0a87dc93bcd`,
is terminal exit 0/MainPID 0. Evidence is `offline-recovery/{result.json,receipt.json,runtime.log}`;
its retained profile is `offline-recovery/profile`. Disconnected signing, actual
acceptance with a lost reply, and a second locked restart recovering the exact
receipt all pass without replacement signing or resubmission. It chains from the
latest failed receipt and retains original assertions/deadlines. The unchanged
daemon and UI assets were reused. Fresh setup was required because the retained
failed draft was outside its admission window; that draft was not re-signed or
given a longer validity window. No broad consumer rerun or daemon rebuild.
This closes the affected offline receipt regression, not all C18. Affected
certified recovery, final artifact
qualification and creator/bootstrap handoff remain release blockers.
No owned validation job remains active after these results.
The appeal, expiry-return and constitution drivers now explicitly enroll before
recording their action baselines, using the shared native helper; syntax and diff
checks pass, but their changed scenarios have not run. No new application build
is needed for these driver edits.

The native losing-ancestor fork journey now also **passes** with device grants.
The existing worker fixture enrolls through native IPC before creating the
provisional branch; it explicitly confirms only that handoff, then carries the
same grant on both competing branches. The losing predecessor remains unconfirmed.
The original assertions still cover dependent receipt rollback, actual node/app
restart, exact draft/ancestry archival, no resubmission and fresh signing readiness.
The first build failed because the fixture treated stored bytes as a decoded
block; its receipt is retained. The corrected integration target built in 9.36s,
then the single native scenario passed in 9.32s with both native stages executed.
Evidence: `/home/burst/device-handoff-20261019/fork/retry/build-receipt.json`
(`481654fde41f3990353b8f8a42c335c7d026b1a608bb8a6b0fcd8412c4382e7e`)
and `fork/runtime-{result.json,receipt.json}` plus `fork/runtime.log`
(`f67b62c0950171727b7e811aaab33a553e9fbad039b84fe20d4d5da6e81139ba`).
Unit `burst-device-native-fork`, invocation `d8ff0ca04a8545afaf84278162ca509b`,
is terminal exit 0/MainPID 0. Profile is retained at `fork/profile`.
Wallet binary and embedded UI were reused; no daemon or application rebuild.
This closes affected native losing-ancestor recovery, not signed network quorum,
separate-host takeover/concurrency or final package qualification by itself;
separate-host evidence is recorded above.

Latest recovery status: current-daemon empty join and crash recovery pass at
`/home/burst/public-testnet-observer-20261019` on VM1. Unit
`burst-public-testnet-observer-20261019`, launch invocation
`f19619e0986f49b0ab0037fa6cd8148a`, is terminal exit 0/MainPID 0. The observer
started from an empty directory and reached the pinned certified epoch, confirmed
heads and online signing readiness in 159,994 ms, within the existing 180-second
deadline. After SIGKILL it recovered the same state in 40,510 ms. Lifecycle,
payment and governance assertions passed before and after the crash.
`result.json`, `receipt.json` and `current-observer.log` retain the evidence.
Recorded observer input fingerprint is
`560f666ac68db99ec83f33e8b7f849be5e3fb8340084273028bfe0bb743bfc90`.
The source ledgers and drivers were reused; no wallet journey or build repeated.
Current dropped-payment recovery also passes at
`/home/burst/public-testnet-inflight-20261019`. Unit
`burst-public-testnet-inflight-20261019`, invocation
`3481a36bf1534fea84d9193ea7779c7e`, is terminal exit 0/MainPID 0. The packaged
wallet retained the byte-exact uncertain draft across all three nodes' SIGKILL
and app restart, restarted locked, blocked duplicate signing, obtained certified
epoch exclusion, rejected the old draft and confirmed a separately reviewed
payment on all three nodes. Its numeric governance proposal activated live;
reuse that assertion rather than scheduling another parameter journey.
The original numeric timing failure remains historical evidence, not a passing
run. This new pass covers its exercised parameter path, not every parameter or
every mid-commit/action fault. Prepared and recovered profiles are retained as
`burst-native-wallet-EIhhqD` and `burst-native-wallet-rM7lre` beneath the run root.
Recorded in-flight input fingerprint is
`cb0fff6dfcb034e21f759c6c394fe7b572ab978f79c6662319e91511540a06f6`.
Both saved results are pass/exit 0. Recovery ports 28076-28083 are closed; only
the normal `burst-validation.service` remains active on VM1.

Remaining finish line: final P1-P5 requirement review using these retained passes,
finish affected C18 certified recovery and installed qualification, private creator backup and
public bootstrap/contact handoff, then distribute the qualified artifacts.
Decision 31.7's cross-device requirement has development evidence above; local
profile locking alone was never sufficient. Final packages must qualify the change.
Do not reopen completed recovery scenarios because the documentation bundle
changes. Public launch and mainnet remain gated.

### Next work, without restarting acceptance

C18 handoff is now an incomplete integrated source change, not a new release
candidate. Signed block v7 carries an account-chain grant; `ChangeSigner` keeps
identity, balances and representative unchanged. Live/branch staging and replay
call the same predecessor-grant validator. Wallet builders carry the grant;
normal signing compares it with the saved local binding rather than adopting the
RPC value. Explicit takeover reviews the head/grant/rules, journals the action,
waits for confirmation and leaves its reservation until the native app durably
publishes the local binding. Pending-action recovery activates only an exactly
confirmed handoff. Offline drafts retain and check their original local grant.

New evidence: the two focused ledger grant tests passed (12.81-second build),
the handoff builder test passed (13.97-second build), and the native wallet
integration compiled under the unchanged desktop caps (6.45 seconds). These
prove their narrow assertions, not network-wide device exclusivity or recovery.
Binding persistence and the handoff builder now pass their focused checks:
separate profiles with identical keys do not inherit the role, restart retains
the binding, and corruption/account mismatch fails closed. After updating the
changed RPC fixtures, the wallet-core integration regressions also passed
(33.25-second build, 18.92-second execution; the two previously passed binding
and builder checks were excluded). This includes the new positive current-grant
and negative stale-grant client cases, offline exact-byte submission, action
journaling and recovery. It is not a multi-node or packaged-wallet result.
Protocol v36, schema v3
and snapshot format v3/state v7 distinguish the changed signed representation;
compatibility rejection, authenticated recovery and pruning still need review.
No old database was rewritten and no new VM or optimized release build was started.

The existing Security settings now expose a reviewed takeover dialog with native
commands, inline failure/recovery navigation and duplicate-submit protection.
TypeScript passes. Its focused browser run failed before app assertions with
`page.goto: net::ERR_ABORTED`; the owned service journal records a Chromium child
core dump (660 MiB peak, no compiler overlap). Do not call this a UI pass or
repeat the run without diagnosing the browser process failure. The service and
Vite process are terminal; the desktop limits were unchanged.
One instrumented diagnostic retry then passed the rendered takeover interaction
check (confirmation required, cancel restores focus, inline pending recovery,
reviewed head/grant/parameter arguments). Browser logging showed repeated
`pthread_create` failures and GPU-process restarts before its software fallback
completed the test. Retain this as mocked-IPC interaction evidence, not clean
graphics/visual qualification. Do not repeat more desktop browser runs with the
same process topology; use the provisioned host or reduce measured worker demand.

Native signing now requires enrollment; the explicit takeover path can use the
restored root key without adopting a remote grant. The existing five reservation
and view-only checks pass on that change, and the added native enrollment check
passes: ordinary signing is refused before binding, works after local activation
and restart, and remains refused on a second profile with the same keys. This
tests local role handling, not network confirmation. The shared action-journal
scenario now includes handoff success and lost responses; it passes and proves
the reservation survives until durable binding activation. The client now opens
an empty account before enrollment. Its positive assertion passes through the existing control-RPC
fixture: fresh account opens once then hands off, already-open enrollment adds no
opening block, and a lost opening response leaves its exact signed journal reserved
without claiming handoff success (5.77-second build, 0.16-second execution).
Existing native release drivers must enroll explicitly
as part of C18, not bypass the new guard.

Schema compatibility: unversioned nonempty stores are no longer identified by
their pending-index marker alone. They are refused without relabelling or
altering data; index-rebuild rollback still has its own assertion. Those checks
pass. The version-rejection test initially failed because its old future-version
fixture (3) is now current; it was corrected to reject old v2 and current+1, and
that focused case passes. Preserve the original failure. No user database was
opened, migrated or deleted during this check.

The existing authenticated snapshot continuation test includes a handoff,
a valid successor carrying its grant and a signed stale-grant successor that must
be rejected. Its first build failed on a moved test value; the corrected build
passed in 65 seconds on VM1 (`device-handoff-20261019/retry-1`, fingerprint
`540edd55314de5f68a9c9ec7e4cdde8e5ec1af0864bd7b44f010d81f7f4296ac`).
Runtime then failed because the shared payment-transition support list omitted
`ChangeSigner`. That failure is retained in `snapshot-receipt.json`, not treated
as recovery proof. The support list now includes it, with a positive admission
and stale-grant/no-mutation regression. The same batch propagates grants through
test-only root-key RPC authoring and genesis governance activation. Ordinary
wallets still must match their local binding; they never adopt RPC grants.
The corrected batch compiled in 70 seconds (`retry-2/build-receipt.json`,
fingerprint `5b5821414570e3f047222ad9f400f7787ee01343f525360b0ef14296cbb2c30e`).
Its saved executable passed both exact regressions in 1.54 seconds:
authenticated snapshot continuation retains the grant and rejects a signed stale
successor; shared live admission accepts a handoff/current successor and rejects
the stale action without changing the account or storing its block. Evidence:
`/home/burst/device-handoff-20261019/retry-2/snapshot.log`, `snapshot-result.json`
and `snapshot-receipt.json` (fingerprint
`ab859b902648bd1801eb5895a46404a44db1f0a9e6abcecbe6fbb023c6c978e4`).
The build and runtime units are terminal, exit 0/MainPID 0. These C18 subchecks
are complete; separate-device integration is recorded above, while final
packaged-wallet qualification remains open.
SSH diagnosis found the desktop public IP had changed. The existing owned
`BurstNativeReceiveTemporary` TCP/22 rule was narrowed to the current
`91.156.229.88/32`; the separate developer rule was unchanged. Pinned host-key SSH
works again. Keep this temporary rule scoped and restore/close temporary access
when host work ends. No new VM, public application port or address range was added.

Remote build workspace: the stable protocol workspace is
`/home/burst/export-handoff.fzkHaK4F/source` itself, **not** a nested
`source/protocol` workspace (that is the wire-protocol crate). A read-only dry run
caught the distinction before any writes. Only changed protocol source contents
were synchronized; identical files retained their timestamps and permissions.
Existing source archives and qualified binaries remain immutable. The sync also
includes pre-existing local lifecycle changes; do not describe the whole diff as
only the device grant. Build plan/scripts are at
`/home/burst/device-handoff-20261019`, with the original external target cache.

| Changed dependency | Affected check | Reuse / new work | Reason |
| --- | --- | --- | --- |
| Signed block/grant, admission and replay | Node library compile; authenticated snapshot continuation and shared admission | Passing `retry-2` build and two exact runtime checks, plus cross-host evidence above | Reuse these assertions; final package qualification remains. |
| Wallet enrollment and durable binding | Existing wallet-core checks; native profile/restart and consumer drivers | Passing four-stage handoff and eight-driver consumer runs below | Earlier packages lack the mandatory role; current development passes still require final package qualification. |
| Snapshot/store versions | Compatibility refusal and continuation | Retain passing store cases and changed snapshot assertion | Pruning/installed recovery review remains; the continuation pass is not proof of every retained-history boundary. |
| Transport/topology unchanged | NAT/connectivity | Retain original transport proof in its scope | No new NAT question arose from this source batch. |

Enrollment now has a dashboard setup route to the existing Security section,
with keyboard focus on arrival; no automatic takeover or new modal is forced.
TypeScript passes on that edit. The first command used a nonexistent
`tsconfig.app.json` and ran no check; the actual `tsconfig.json` check passed.
Native certified fixture and consumer-UI drivers now enroll through the existing
reviewed native commands/dialog, not by writing bindings or clearing journals.
Their JavaScript syntax checks pass; changed native profile and consumer journeys
now pass as recorded below. Certified Test and offline-receipt journeys remain
outside those passes.
The native reviewed-signer regression group compiled in 21.03 seconds: five
checks passed and the appeal fixture stopped early because it lacked enrollment.
The fixture now reuses the existing synthetic local binding setup; its original
changed-review/no-submission assertion passed (9.51-second cached build,
0.05-second execution). No production guard or assertion was weakened. This is
unit-fixture evidence, not proof of network enrollment. The initial cargo command
had no configured default toolchain and did not compile; the checks explicitly
used the repository's nightly-2026-10-11. No validation job remains running.

The existing Dev/payment/return/automation/group and offline drivers now enroll
through one shared native-command helper or the existing UI dialog. First-receipt
assertions explicitly account for enrollment's empty Open + ChangeSigner, retain
zero accepted funds during enrollment, and require exactly one later Receive.
No fixture writes a binding or clears an uncertain journal to make signing work.
The response-loss proxy's two focused checks pass, including retaining rejected
responses and recording upstream acceptance before dropping the accepted reply.

The changed daemon built in 41.44 seconds on VM1 (validation profile, fingerprint
`e23fcb6d70f7e15874d1bc522b7aeed5b002591f7652425af8f7ab7e11b2ca1b`).
UI assets compiled once (`1a2cee7f5d322864ff34ce27e271e41eeb740e7eceb6f41f93ddc8b81fbcb15a`);
the embedded development wallet built in 3m57s
(`67f8932e23994ee1d221b7ab434934666bbdeea2dc2e72f22e0238a4753c6312`).
Build stages have separate receipts under `device-handoff-20261019/{daemon,wallet}`.
Native runs reuse embedded assets without rebuilding the UI. The native wallet
keeps its pre-existing native workspace/dependency path; the daemon keeps the
pre-existing export workspace. Neither was relocated or given a new target cache.

**Native C18 profile/restart integration passes**: fresh profile A enrolls and
saves an unsent draft; separate profile B restores the same public fixture key
but cannot silently sign; explicit takeover loses its accepted response and
retains the exact journal; a locked B restart confirms that same handoff and
publishes its binding without a replacement; restarting A preserves its old
binding, the node refuses its draft, canonical recovery releases the draft and
ordinary signing remains blocked. Identity, balances and representative remain
unchanged by takeover. Four exact stages passed; no concurrent native apps ran.
Evidence: `/home/burst/device-handoff-20261019/native-{plan.json,result.json,receipt.json,log}`,
fingerprint `76c30ebf51577610f559fb9ffbbcd63625eb17df8b73f581ca84b8992eca7ad9`.
Retained disposable profiles: `device-handoff-20261019/native-profile` (copied
from `/tmp/burst-native-wallet-0114Gr`). This closes isolated-profile and process
restart assertions, not separate physical hosts, concurrent takeover or packages.

The eight existing affected consumer drivers **pass** on these same binaries,
under the original caps. Unit `burst-device-consumer-integration`, invocation
`4b67c9b7b2d943f8ae33cc58346b8eef`, is terminal exit 0/MainPID 0. Evidence:
`device-handoff-20261019/consumer-{result.json,receipt.json,log}`, fingerprint
`bede405fcd3e16bc53ed2e70bc5187bbaf9273acadb2779887ad0962ff77a3d4`.
All eight drivers executed: backup/view-only access, onboarding, eleven-command
signer binding, payments/merge, UI review/cancel/lock, returns, automation
cancellation and group policy/real paginated acceptance. The explicit enrollment
setup preserves first-return and first-receive account/balance invariants.
Profiles are retained at `device-handoff-20261019/consumer-profile` (original
`/tmp/burst-native-wallet-M1Ul1q`). No validation job remains running.
Preflight chains from the latest retained eight-driver receipt, `9cb6d4ea...`;
the old installation evidence remains scoped to its old artifact. No package
qualification is claimed for these development binaries.

Next integration batch: remaining certified
recovery; the affected offline receipt regression now passes above. Preserve
the saved executables above. The unused legacy `Node::process_transaction` conversion has no
call sites in the workspace and is not a wallet submission path; review it for
removal rather than enabling its old transaction-signature conversion as a new
way around device binding. Creator backup/bootstrap/contact and exact distribution
qualification remain release blockers after C18.

Remaining C18 work: qualify affected external certified fixtures (native losing-ancestor fork recovery
now passes above), review remaining constructor
paths and mocks for the new RPC field, recovery and
installed snapshot qualification in the final package boundary. Root
key holders can still use custom software; this protects cooperating wallet
devices and stale signed drafts, not against a malicious root-key owner.
Retain old candidate receipts for their original inputs; do not qualify this
changed protocol from those receipts or repeat unrelated transport checks.

C18 view-only implementation is in progress in the wallet source. It opens by
public address, clears loaded keys/keystore, retains pending journals, disables
automation and rejects signing or pending-journal mutation natively. Existing
balance/history/portfolio screens are reused; signing routes are unavailable.
TypeScript passed. The two new native watch tests passed; the same compiled
executable then passed the other 26 native library checks without rebuilding.
These are development checks, not qualification of a new packaged wallet.
The final native edit serializes private-key export with account changes and
checks view-only access before export. The watch test now starts with a loaded
encrypted keystore and proves it is dropped. Its two focused checks pass on that
revision (7.74-second cached rebuild, 1.40-second execution). Do not relabel the
earlier 26-test run as a run on this later revision; unrelated assertions are
retained by source-impact review. Retained daemon and network binaries are
unchanged; the subsequent C18 source changes above are not yet packaged.

Browser diagnosis checkpoint: two local UI attempts failed during Chromium
startup with `pthread_create: Resource temporarily unavailable`, before any app
assertion. The one-CPU-affinity browser-only preflight passed under the same
1 CPU/3 GiB/64-task caps. The combined Vite/test-runner/browser topology still
failed, so affinity alone is insufficient. The same topology then passed with
`UV_THREADPOOL_SIZE=1`, `GOMAXPROCS=1`, CPU affinity 0 and
`--test-isolation=none`, all inside the unchanged resource wrapper. The focused
`dashboard.test.cjs` view-only test passed in 4.93 seconds: real rendered form,
invalid-address feedback, public balance display, blocked signing navigation,
disabled export/representative controls, no automation and close-session flow.
IPC is mocked in this browser test; it is not native packaged acceptance. Preserve
the two startup failures as environment evidence, not failed product assertions.
The existing `native-wallet.mjs` driver now contains the actual IPC view-only
checks for the next wallet candidate; it has not run against a new package yet.

Backup instruction review: `wallet/tests/native-wallet.mjs` asserts a 24-word
creation/restore address roundtrip, encrypted restore, wrong-password rejection
and non-overwrite. Certified onboarding separately uses encrypted backups.
These are distinct assertions, not interchangeable names for one backup test.
`Wallet::from_mnemonic` reconstructs the key; the Tauri app stores pending actions
separately under its app-data `offline-payments` directory. The handoff now
distinguishes key recovery from pending-action and node recovery, and explicitly
states the unresolved cross-device limitation. No runtime test was repeated and
no new runtime pass is claimed. This documentation edit is local; the retained
remote bundle remains immutable and will need refreshed documentation/checksums
at the final handoff boundary, not another binary build.

| Requirement | Current gap | Next action | Evidence to retain |
| --- | --- | --- | --- |
| C18 / decision 31.7 | Signed off for the settled cooperating-device model. The dependency review above maps cross-host takeover/recovery and stale-device refusal to the frozen source; installed v7 signing/recovery qualifies the package boundary. | Reuse this evidence unless grant, signing, recovery or packaging dependencies change. No repeated contest or takeover decision is scheduled. Other C18 requirements retain their own status. | Current candidate evidence, shared input comparison and checked receipt fingerprints are indexed above. No malicious-root, universal crash or new installed two-host claim. |
| Final P1-P5 reconciliation | Some summaries still describe completed stages as open; other requirements need scope review. | Match each disputed claim to implementation and retained assertions. Correct stale text; run a test only for an identified missing assertion or affected dependency. | Current onboarding, lifecycle, governance, observer, snapshot, independent-host and app receipts below. |
| P5 distribution | Creator backup confirmation and public bootstrap/contact handoff remain unresolved. | Finish operator handoff and delivery after required functionality is qualified. | Existing package/source provenance and installation evidence. Documentation edits require link/checksum review, not protocol acceptance. |

This scheduling update completes no product requirement. No compiler, runtime
suite or Azure job was started for it. The desktop validation service was absent
when checked. Remote service state above is the last recorded observation, not a
new live-host check.

The latest handoff is now
`/home/burst/public-testnet-bundle-20261019/handoff-limits/candidate`, with
checksum manifest SHA-256
`e1d875015a366ed4ecd328a52c9d01b6c9e9b7acb1c523600e82d60c12937ae9`.
It includes the operating-limits disclosure and repaired standalone links.
Unit `burst-bundle-limits-20261019`, invocation
`da94a975b61646d2beb09b73359f9e73`, is terminal exit 0/MainPID 0. All 12
non-documentation files retain the preceding qualified bundle's bytes; source
and binary runtime evidence remains applicable. Checksum and link checks pass.
The earlier handoff and its independent-host qualification remain immutable.

### Validation scheduling correction

The release workflow now makes all five platform builds and its reusable CI call
depend on `release-readiness`. Previously they started even when publication was
already blocked. Normal push/PR CI is unchanged. The focused
`node --test tools/check-spec-coverage.test.mjs` run passed all six checks under
`limited-run.sh`, including the workflow dependency contract. The real
`check-spec-coverage.mjs --release` command returned 2 as expected: 388 indexed
sections, 380 unsigned-off, zero inventory errors. Those are review statuses, not
380 missing features. No GitHub workflow was dispatched and no savings measurement
is claimed. This local scheduling fix changes no candidate binary or release gate.

Keep public daemon `a10d5e6a...`, functional staging daemon `cdfe7ba3...`,
wallet `dcfb91f5...` and dashboard `af592ffd...` fixed unless a demonstrated
release blocker requires production changes. Recent runner fixes did not change
these binaries. No broad rerun follows from this documentation update.

| Existing evidence | Next action |
| --- | --- |
| Public identity `dba199ec...`, wallet combination `9cb6d4ea...`, dashboard `a1479009...` | Retain these passes. Do not rebuild or repeat them for driver changes. |
| Fresh onboarding `cc1af413...`, lifecycle continuation `df6a38ca...`, governance continuation `884771a1...` | Retain their exact assertions and the original failures. Review whether uninterrupted timing gaps map to protocol requirements, user-facing limits or only runner budgets before scheduling another journey. |
| Current independent-host, empty-observer and in-flight evidence; retained NAT evidence | Changed recovery integrations now pass. Retain these results and the unchanged transport proof; no further runtime repetition for documentation changes. |
| Distribution and installation | Assembly, provenance, archive metadata, handoff and independent payload checks pass. Complete launch/operator handoff and delivery; do not regenerate funded or verified fixtures for packaging checks. |

The prepared independent-host script is not authorization to repeat that check.
First determine what changed in its recovery/transport dependencies and which
assertion remains unproved. Keep VM2 deallocated until a run is justified.
Creator backup confirmation, distribution handoff, operating limits and unresolved
candidate qualification remain release gates. This scheduling correction closes
no protocol requirement and changes no whitepaper behavior.

Candidate manifest comparison on VM1 found 820 old source files and 824 current
source files, with 39 changed/added paths and no removals. Baseline is
`suffix-batch.vEo5wkoe/candidate/SOURCES.sha256`; current is
`public-testnet-candidate-20261019/candidate/SOURCES.sha256`. Changes are in BRN,
TRST, confirmed replay and snapshot installation, graph/TRST persistence,
governance/verification derived ledger updates, checkpoint projection and tests.
`Cargo.toml` also changed. Network transport, NAT/UPnP, wire definitions,
`Cargo.lock`, daemon source and RPC source have no manifest differences.
Every manifest entry also matches its saved build receipt's source inventory;
all three saved build results are pass/exit 0. Public and staging manifests are
identical across all 824 entries, SHA-256
`be854061f0700ba1a93f022f3ac203f5857c67a3546bfa51a6fd55f227723fb0`.
The old 820-entry manifest is
`0a4fd9aa8dbe53723387ff145a6fb5aa54a864272d0bd762bb6509cbbf718cdf`.
This is an inventory comparison, not another test execution or a claim that the
public and staging binaries have the same identity.

| Qualification | Impact decision |
| --- | --- |
| NAT/firewall/UPnP transport | Retain `3d9266ff...` for its isolated transport topology. No transport change justifies rebuilding/rerunning the NAT lab. This does not claim distributed quorum behind NAT or unchanged economic replay. |
| Snapshot authentication, state installation and crash/reconnect | Current `5fc2a1a1...` covers changed recovery on the current staging artifact. Retain the older Azure LAN proof for that topology; do not call it a current-binary pass. |
| Independent host and final installation | Combined current check passed (`e4233272...`). Do not repeat it for later documentation/checksum updates when binaries are unchanged. |
| Empty observer | Current daemon join and SIGKILL recovery now pass in `public-testnet-observer-20261019`; no lifecycle creation repeated. |
| Uncertain payment | Current daemon and packaged wallet pass in `public-testnet-inflight-20261019`, including live numeric activation and reviewed replacement. No full onboarding rerun. |

This comparison selects work; it does not by itself certify dependency behavior
or open a gate. Final packaging must preserve each app's original source archive
and identify the public daemon separately from the disposable staging daemon.

P5 bundle assembly now passes on VM1 at
`/home/burst/public-testnet-bundle-20261019/candidate`. Unit
`burst-public-testnet-bundle-20261019`, launch invocation
`044848b74a3b493398688d2622c80767`, is terminal with exit 0/MainPID 0.
The existing public daemon and corrected wallet/dashboard packages were copied,
not rebuilt. Every bundled checksum passed. `SHA256SUMS` has SHA-256
`14759dd223525c38dcff9f7775efd97e12e9361cb55e5875a546b2de2cae4f3c`.
Separate source archives retain the public daemon source, 293 selected original
wallet production/build/resource inputs and 1,050 original dashboard/protocol
source files. All selected files matched their recorded source hashes before
archiving. Component provenance retains the failed original wallet post-build
check alongside its passing corrected package and runtime records; it does not
relabel the old app as built from the new daemon source.

Assembly evidence is `assembly-result.json`, `result.json`, `receipt.json` and
`assembly.log` beside the candidate. The manifest explicitly says unreleased.
This closes artifact assembly, not installation, archive buildability or all P5.
Source-archive usability, including the wallet's workspace manifests, and the
standalone handoff links subsequently passed as recorded below.
Keep this assembled evidence immutable; final handoff changes need new checksum
metadata, not another build or protocol journey. The combined independent-host
check also subsequently passed. No public network launch occurred.
Creator backup confirmation has been requested without asking
the user to disclose the seed.

The source-archive check now passes for all three component archives. Unit
`burst-bundle-source-check-20261019`, launch invocation
`502dc7f2d9c34f8ca80a2b8c952f6d4c`, is terminal with exit 0/MainPID 0.
Each archive was extracted outside its original workspace; included file hashes
matched, and `cargo +nightly-2026-10-11 metadata --offline --no-deps` succeeded
for the daemon, wallet and dashboard manifests. Evidence is
`public-testnet-bundle-20261019/source-check/{observations.json,result.json,receipt.json}`.
No compilation, download, installation or protocol scenario ran. This closes
archive path/manifest usability, not reproducible-build verification.
The assembly receipt also revalidated as pass, fingerprint
`73818fdec23772236bc1c93f91b9a78c6a47876cd9eb8d48c41da8441627210a`.

`deploy/TESTNET_HANDOFF.md` now supplies standalone installation, identity,
connection, participant-verification and recovery guidance. It explicitly keeps
public endpoints/contact and release approval pending. It is included in the
handoff with repaired links. `deploy/OPERATING_LIMITS.md` now records measured
recovery times, validation caps and unqualified scale/fault/platform scope. Its
bundle update requires checksums, not recompilation or another runtime pass.

The standalone handoff bundle now passes at
`public-testnet-bundle-20261019/handoff/candidate`. Its receipt revalidated as
pass, `46beecb8e89cef5fe2283142828bc19383bb4eaa945f3191dafdec251f5f5108`.
Both guides are included, all Markdown links resolve within the bundle, and
non-documentation files retain their verified assembly bytes. Final checksum
manifest SHA-256 is
`2513522e5db32b44d398810b3a59d7478d126edcb3f1eb5c303f58e99ec118a7`.
No binary or source archive was rebuilt. Public release remains closed.

The combined P4/P5 independent-host check passed, reusing the existing
snapshot driver and completed ledger. It checks final public bundle checksums,
the exact already-installed GUI payloads and public daemon identity/restart,
then restores the same-source disposable staging snapshot and confirms its live
payment suffix before/after SIGKILL. Public and staging identities stay separate;
no positive endorsements use the real creator identity. Unit
`burst-public-testnet-independent-peer-20261019`, launch invocation
`f736514252e54b7fa4b3a356018822a8`, is terminal with exit 0/MainPID 0.
Receipt `e4233272268f76b840f6d50852931dab3c91a29581e2644749ab3246cc6e23d7`
revalidated as pass on VM2 from `/home/burst/suffix-independent-20261018`.
Evidence is `/home/burst/public-testnet-independent-20261019/` on VM2.
Both pre/post-crash checks preserved the staged epoch `c83ed414...`, lifecycle
and governance state, confirmed suffix payment `e4527e88...` and its exact pending
claim of 1,000,000,000,000. The public daemon separately passed Dev/Test/Live
identity, funding gates, authority rejection and same-ledger restart checks.
The installed GUI binaries match the final Debian payloads byte-for-byte; no
reinstall or UI journey was repeated. This qualifies the current Azure LAN
integration, not NAT, empty-observer or uncertain-action recovery.
VM2 is `10.87.1.6`; VM1 remains `10.87.1.4`. No VM was created. Cleanup is
confirmed: VM2 reports `VM deallocated`; the temporary source has MainPID 0 and
its P2P/RPC/transfer ports 28076/28077/28090 are closed. Its unit reports failed
because the source script exits on deliberate termination; the peer acceptance
completed successfully before that stop. VM1's normal validation service remains
active. No user application or unrelated service was stopped.
Prepared peer script SHA-256 is
`35056a122d2a979f792394d145e7aed4449c1b16fd27e26280c5cbfaee2da386`.

The user reset the active goal to a safely distributable public testnet. The
pilot's release-priority table now separates candidate/user-journey blockers
from scale requirements and optional polish. Whole-state migration is not
automatically a pre-release prerequisite; its effect on the supported operating
envelope must be established. Optimized cold-retirement integration now passes;
qualification of the distributed daemon and apps remains open.

P5 operator guidance correction: `deploy/TESTNET_SETUP.md` no longer directs
users to obsolete validation IPs, pipe an unspecified installer to root, enable
a faucet to create genesis, or delete a ledger to fix synchronization. The guide
now distinguishes creator identity from bootstrap connectivity, keeps RPC/WS
local, preserves signing journals and uncertain wallet actions, and discloses
accelerated Test governance. CLI examples were checked against `daemon/src/main.rs`,
snapshot behavior against `daemon/src/snapshot.rs`, and initial rules against
`types/src/params.rs`. Three shell examples pass syntax checks and local links
resolve. No example was executed and no protocol build follows from this edit.
This closes the stale-guide defect, not final-artifact installation or all P5.

P5 local artifact reconciliation: `target/debug/examples/genesis_keygen` was an
old executable that ignored `--help` and emitted a new throwaway Live seed.
That identity was not written to a seed file, configured in source, deployed or
used. Never reuse it. The executable, SHA-256
`08dfb29cb3f5279354b7014552c395fa91c5f9d277f76ac3a5a6cdf8ae1102b8`, is preserved
as `genesis_keygen.unsafe-legacy-20261019` so the usual path cannot invoke it.
This is stale build output, not a regression in the current helper source.
Current source hash `d2dbe54f...` matches the October 15 file-mode validation log
in `target/validation-evidence/native-release-20261015/independent-install/`.
No real network identity was created by the stale executable check.

P5 creator identity is now generated locally. The retained safe helper was
downloaded through host-key-pinned SSH and matched SHA-256
`ec2989dd70fc5801c196150f38a8b5c9647f1daeb3be2e44bd3ceba5f07e81de` before use.
The existing private-file CLI check passed under the desktop wrapper; receipt
fingerprint `befb6b44cacb2701f76ef551daa7ef5fa7fe2c53c57853b97066b5c4933de2cd`.
`deploy/public-testnet-identity.json` records only the public key
`948acade65c53fcc5e0d2d001fb1b01edc40b9ea102f1a9ae84d53c92551fd7e`.
The new seed is outside the repository at
`/home/nitesh/.local/share/burst/private-testnet-20261019/creator.seed`, owned by
the user with file mode 0600 and parent mode 0700. Node crypto independently
derived the same public key and verified the helper's reported genesis-hash
signature without printing the secret. The final daemon now matches the reported
genesis hash and initial rate in the isolated check below. The seed was not sent
to Azure and no public network was started.
The user has been asked to confirm a separate private backup; launch stays gated
until that confirmation. The exact temporary SSH authorization and NSG rule were
removed, and the local transfer key deleted. Existing developer access was left
unchanged. Evidence is in `target/validation-evidence/creator-identity-20261019/`.

P5 daemon integration build completed on VM1 at
`/home/burst/public-testnet-candidate-20261019` in 3m44s. Artifact SHA-256 is
`a10d5e6a86701b757e8c6a0c1b823cb7cf51ae75736149eb6a62404434c6c8a0`.
Unit `burst-public-testnet-daemon-build-20261019`, invocation
`40e8d7fcedd94616a87a475695ad9c1a`, is terminal with MainPID 0. Compilation and
all candidate checksums passed; the unit exited 1 because receipt finalization
ran from the packaging directory instead of the recorded source directory.
Finalizing the existing start/result from the original directory succeeded,
and the receipt revalidates as pass with fingerprint `b1ff930a8258d05835af0194dd43a35b110a1b3696fc70c43c8c9a6090109d3f`.
No compilation or checksum stage was repeated. The original unit failure and
log remain preserved. The local deployment template now restores that directory.
It uses the validated stable protocol source and target cache, with the new
public key as the only identity input. The seed remains local. The prior recovery
receipt revalidated before scheduling; no functional suite was repeated.
The reused daemon builder preserves a source archive, artifact hashes and
network metadata under 1 CPU/6 GiB/no swap/20 minutes. Local unwired verification
storage work remains outside this selected source snapshot, as already deferred.
App dependency/source inspection proceeds separately; no app build is active.
P5 final-daemon identity qualification passes. The existing daemon smoke driver
checks the Test address, confirmed genesis, payment epoch, verified creator,
zero TRST, initial BRN rate and missing/wrong-key authority across restart. It
never reads the real seed or authors positive endorsements. Dev/Test feature
availability and Live rejection gates also pass, with faucets disabled and clean
shutdowns. Unit `burst-public-testnet-identity-20261019`, observed invocation
`4129b22235714df5aad8d7e32d4fa995`, is terminal with exit 0 and MainPID 0.
`identity-check/receipt.json` beneath the candidate root revalidates as pass,
fingerprint `dba199ec620d568993ed07acc78df68b7cbc95eec77be3878effd34df9861d83`.
This closes final-daemon public identity matching, not full P5 or app acceptance.
The isolated check used 1 CPU/3 GiB/no swap and no bootstrap peers. Remaining
blockers are final app-combination journeys, distribution/installation handoff,
supported operating limits and private creator-backup confirmation.

App reuse review: 407 selected source/configuration files were compared with the
stable native workspace. Wallet app files match; its source input hashes also
match the retained package-build receipt. New BRN/TRST engine journal code is not
called by the wallet: its BRN balance path uses unchanged `state::RateHistory`,
and transaction validation uses unchanged `MAX_MERGE_SOURCES`. The workspace
manifest difference only adds the test-derived validation profile. Retain wallet
`dcfb91f5...` with its original source provenance, not a claim that it was built
from the newer daemon sources. Dashboard source matches except the already-known
Debian dependency metadata correction; corrected package provenance still needs
checking. Review and exact differences are saved in
`target/validation-evidence/creator-identity-20261019/app-source-review.json`.

The existing eight-driver installed Dev check passes with daemon
`a10d5e6a...` without a wallet rebuild or reinstall. Both the prior wallet runtime
receipt `4fb0b859...` and final-daemon identity receipt `dba199ec...` revalidated.
Initial preflight refused because the old receipt's relative log path was
resolved from the new output directory. No scenario started. The corrected
preflight and runtime retain the old receipt's working directory while writing
new evidence to `public-testnet-candidate-20261019/wallet-combination/`.
Corrected preflight passed. Unit `burst-public-testnet-wallet-20261019`, launch
invocation `7dcd0ba2cd514b0aaabdc66d0ceeeb73`, is terminal with exit 0/MainPID 0.
It retained 1 CPU/3 GiB/no swap/20 minutes. The receipt revalidates as pass with
fingerprint `9cb6d4ea9c5476ca048677fa16b836fc6973ce06b28bed36fcdced4f2d910e8e`.
All eight drivers executed, including onboarding/backup, signer protection,
payments, UI review/cancel and lock, returns, automation and group policies.
Native evidence is under `wallet-combination/profiles/burst-native-wallet-m1g6tr`.
This closes the final-daemon/retained-wallet Dev compatibility check, not fresh
certified Test acceptance or all P2/P5.

The retained dashboard package in `tip-bundle.p2NkbHiu/candidate/node-gui/`
matches its recorded SHA-256 `94f3ea4e017115df246becedefdc25a5309a74fb4291ef0a456639d63d26e4b7`.
Its extracted executable matches installed `af592ffd...` byte-for-byte by SHA-256,
and its metadata declares libc6 >= 2.39 plus GTK/WebKit dependencies. A separate
113-file historical dashboard source comparison finds only the known dependency
configuration difference. No dashboard rebuild is justified by that metadata
issue. Final dashboard source provenance and changed-daemon runtime now pass:
all selected local application sources match the frozen package manifest and
archived source contents. The installed dashboard passes the existing native
RPC and rendered navigation drivers against daemon `a10d5e6a...`, including
verification lookup, explorer details and restored keyboard focus. Unit
`burst-public-testnet-dashboard-20261019`, launch invocation
`8bfb6c6e5f0e47de8257677b47b76750`, is terminal with exit 0/MainPID 0.
Receipt `dashboard-combination/receipt.json` beneath the public candidate root
revalidates as pass, fingerprint `a14790099de5c5a791f1388cb975ef3f2e40b82a780c70dff6dcb5e332790039`.
No app build or installation was repeated. This closes that Dev dashboard
compatibility/provenance check, not certified Test or distribution handoff.

Certified Test acceptance now uses a separate staging build of the same protocol
source with only the compile-time creator public key changed to the published
disposable identity. This avoids authoring artificial human endorsements under
the real public identity. The public build receipt revalidated before scheduling.
The build at `/home/burst/public-testnet-staging-20261019` completed with a passing
result and revalidated receipt `5e7c882162f1a6de5207c139f78727d78fec3431f221ba158a6a6952f35dffee`.
Unit `burst-public-testnet-staging-build-20261019`, launch invocation
`c0ae8d7b2f5a429eb56a7a55053f7919`, was no longer active before packaging started.
Caps remained 1 CPU/6 GiB/no swap/20 minutes. Public candidate `a10d5e6a...` remains immutable and distributable
only after its remaining gates pass. Staging artifacts must never be published.

The staging build's network metadata was generated with the public mode label
instead of `private-testnet-fixture`; its identity JSON does explicitly say
disposable. The deployment template is corrected, but do not mutate the active
build inputs or rebuild for this label. The existing build receipt is preserved.
A separately recorded runtime bundle passed the fixture identity validator and
checksum checks with unchanged binary/source bytes. Receipt fingerprint
`89e129f766a41c1e3acd1b9180f4b2128ef8dd78c9e6870da520088bd0ba778a` revalidates
as pass. Package unit launch invocation was `f0c33791cb7a45b6b7984eb2af30172a`,
under 1 CPU/3 GiB/no swap. The subsequent fresh-journey preflight revalidated the
prior completed consumer receipt, then refused a UI-driver hash difference before
creating the new run directory or starting a scenario. The inspected diff contains
existing improvements to payment rejection/uncertainty diagnostics and saved-vote
recovery through the exact confirmed account slot, plus its decoder import.
Keep those checks along with the new readiness wait. Deployment verifies the
retained driver's hash against precisely that reviewed diff. The old test snapshot
does not contain the newly imported `offline-envelope.mjs`; a second preflight
identified that before scenario startup. The deployment now includes the existing
local decoder explicitly, verifies its transfer hash and checks both scripts'
syntax. No protocol journey failed and no rebuild follows.

The corrected fresh certified journey passes at
`/home/burst/public-testnet-staging-20261019/fresh-certified`, unit
`burst-public-testnet-certified-20261019`, launch invocation
`bca2790e6327482e9ef1eb2dba37e444`, now terminal with exit 0/MainPID 0.
Receipt fingerprint `cc1af4138b51742e260a37a58ea37617b6f5293cb755ffb64eb28d9781c41598`
revalidates as pass. New subject/receiver wallets completed encrypted backup and
restore, paid endorsements, all seven reviewed selected-human votes, verification,
accrual, BRN payment, TRST transfer/receipt and unfunded/disconnected/locked
rejection. Exact consumer state matches all three nodes before and after SIGKILL.
The recovery reference reached signing readiness at epoch `972211de...`.
This closes uninterrupted fresh installed consumer acceptance on current staging
source, not independent-host/current-public-identity acceptance or all P2/P4.

The existing driver retained its 600-second native journey deadline and
1 CPU/3 GiB/no swap/20-minute whole-job cap. One observation reported a 791,834,624
byte peak while the job was active; that is not a final capacity measurement.
Completed ledgers, encrypted fixture backups and `onboarding.json` remain in the
same evidence root. Reuse them for the next lifecycle/governance checks; do not
repeat onboarding. Public candidate `a10d5e6a...` and the real creator seed were
not used to issue artificial identity endorsements.

P3's first fresh lifecycle attempt stopped at test dispatch in the completed
consumer root's `package-lifecycle/` directory. Unit
`burst-public-testnet-lifecycle-20261019`, invocation
`1a5b5a50bfb6440aaab3f9766fb5238d`, is terminal with exit 1/MainPID 0. Receipt
`338b1cc9f30f4d3ad981a6dda905536265841040abdde0752dc6801fbf2d210e` preserves
the failure. Node synchronization and all three baseline consumer checks passed.
The child then failed `MODULE_NOT_FOUND`: `native-session.mjs` reconstructed
`../tests/` instead of loading the case alongside itself in `lifecycle-tests/`.
No lifecycle progress file exists and the lifecycle module never executed.
The current onboarding receipt and prior composite lifecycle receipt `c63ce15e...`
both revalidated before scheduling. This uses staged daemon
`cdfe7ba34373066a7f8a141b67562079961254313f69c84f3df021dd22979b9b`
and retained wallet `dcfb91f5...`. No build, installation or onboarding repeats.
The existing lifecycle driver uses copied consumer ledgers, current native test
helpers with explicit dependencies, reviewed challenge and appeal votes, normal
challenged spending, origin-specific revocation and restored payment, followed
by exact three-node SIGKILL recovery. The 1 CPU/3 GiB/no swap/20-minute cap and
600-second native journey deadline remain unchanged. No P3 pass is claimed yet.

The shared native runner now resolves case URLs relative to `import.meta.url`,
so named test directories work without redirecting to an unrelated sibling.
Syntax checking passes. The corrected deployment preserves the failed inputs,
uses a new test directory, checks the actual selected module exists, and chains
`prepare` from the failed receipt. It reuses the same binaries and original
consumer baseline; no challenge, appeal, onboarding or payment is resubmitted.
Corrected unit `burst-public-testnet-lifecycle-corrected-20261019`, invocation
`ec259be34c644c07a29c21b09593f48b`, is terminal with exit 1/MainPID 0.
Receipt `7d9b47123f5756f25d1d0374158e87a641c915f8dac940f3663902c648c5bf5a`
revalidates as a failure. Its output is `fresh-certified/package-lifecycle-corrected/`
beneath the staging root. The job ran 03:30:44-03:36:02 UTC, used 301.9 CPU
seconds and peaked at 748,986,368 bytes. No build ran.

Two-attempt diagnosis checkpoint: the first attempt never dispatched the test.
The corrected attempt completed the challenge, spending during challenge,
origin-specific revocation, seven challenge votes and seven appeal votes.
It checked restored rights and balances, then the final payment was refused
with the explicit synchronization-before-signing error. The UI displayed that
error; this is not evidence of an uncertain submitted payment. Recovery did not
run and the full lifecycle requirement remains open. Preserve the progress file,
three ledgers and native profile `burst-native-wallet-I4J7WN`.

The payment helper assumed restored verification implied signing readiness.
`wallet_core/src/signed_client.rs::work_parameters` deliberately checks these
separately. The positive payment helper now waits for `local` or `peer_matches`
before its first confirmation and asserts the payer head has not changed. It
does not retry a signed transaction, weaken the guard or extend a deadline.
Syntax passes under the desktop wrapper. The subject's pending payment journal
is absent in the retained native profile, consistent with refusal before signing.
Next, resume the existing appeal-completed fixture through the existing resume mode
to discriminate transient synchronization from failed restored payment. Do not
repeat challenge, appeal, voting or onboarding to reach that boundary. A resumed
pass alone will not establish uninterrupted lifecycle acceptance.
The continuation passed as `burst-public-testnet-lifecycle-payment-20261019`,
invocation `df9c724cbe7c4aad9132f7affe019e12`, terminal exit 0/MainPID 0.
`fresh-certified/package-lifecycle-payment/result.json` and `run.log` prove
the completed appeal was reused without resubmission, restored rights and
origin balances matched, and a newly reviewed payment/receipt confirmed.
The compiled verifier checks all three nodes before and after SIGKILL; both
checks pass, with matching epoch `747e2410...` and signing readiness restored.
This closes the resumed restored-payment and exact three-node recovery gap.
It does not replace uninterrupted lifecycle acceptance or independent hosts.
The run reused the original resume driver, copied the failed run's immutable
ledgers and wallet data, and chained `prepare` from failure `7d9b4712...`.
No build, onboarding, challenge or vote repeated. Reuse its resulting baseline
for current-candidate governance; do not replay the lifecycle to create it again.
Its receipt revalidates as pass, fingerprint
`df6a38ca2e1de2ddad75c03ea4ba69f5e5b2204423b5445d462c586f8dd04416`.

Current governance qualification failed as
`burst-public-testnet-governance-20261019`, invocation
`3cc3840b632a41fa89d16f81bb99f7b2`, terminal exit 1/MainPID 0. Output is
`fresh-certified/package-governance/`. The previous governance receipt
`dc676210...` and the completed lifecycle receipt both revalidated before launch.
`prepare` chains from the latest governance receipt, not the lifecycle receipt.
The existing driver checks a numeric parameter, multilingual constitution,
direct/delegated votes, certified activation and post-count delegation revocation,
then governance and retained lifecycle state after three-node SIGKILL. It uses
the same binaries, current native helpers and completed lifecycle baseline under
1 CPU/3 GiB/no swap/20 minutes. No current governance pass is claimed.
The run lasted 03:45:22-03:51:34 UTC, used 246.7 CPU seconds and peaked at
562,753,536 bytes. It timed out at the unchanged 240-second wait for certified
numeric activation (`native-certified-governance.mjs:72`). Saved proposal
`13443cdab1f1d1208e817272b69533c9b93805ee66a8e083710bbf987fd00d7b`
exists in `governance.json.progress`; no amendment was submitted. Preserve the
wallet profile `burst-native-wallet-jgC8t6` and all three ledgers.
Failure receipt `39729e635223ddbe6c8956911c0fef495513a6a64abaebce2627f2a94847a51f`
revalidates. Use it as the latest governance receipt for subsequent preflight.

Node 0 logged a certified checkpoint installation at 03:51:10, then shut down
when the scenario failed. This does not establish the proposal's final phase
or the cause of the delay. The earlier connection timeouts occurred during
startup; no causal claim is established. The existing phase helper now records
phase, projection status, deadlines, tally and observation time on transitions
and failure. Syntax passes; no deadline or assertion changed.
The capped retained-state diagnostic passed as
`burst-public-testnet-governance-diagnostic-20261019`, invocation
`ad3dc5fac35944e6a10cd57408985d99`, terminal with no remaining MainPID. Its output directory
is `fresh-certified/governance-diagnostic/`. It reuses the existing governance
sequence inspector on copies of all three failed-run ledgers, querying this
exact proposal, governed parameters and signing readiness. It submits no
proposals or votes and preserves the original failure. `prepare` chains from
receipt `39729e63...`. Receipt
`50fbd2005275f905c7ea3ae62e8753324258ece0afa8a17db8fe0a063410c83e`
revalidates with exit 0. All three captured responses report the exact saved
proposal as `Activated`, `phase_projected: false`, with one yea vote; all report
`peer_matches`. The governed minimum verification age is now 1 second.
Node 0 prepared checkpoint tip 52 in 12,375 ms and completed publication in
12,595 ms, reaching epoch `c630fce8...` at 03:57:48 UTC. The original run had
installed the preceding `d96c54f2...` epoch before shutdown.

This proves retained numeric activation and convergence after restart, not the
original 240-second live activation bound. The observed rules have a 180-second
voting window, 15-second cooldown and 5-second propagation buffer. Do not infer
the complete live certification delay from one post-restart timing. No timeout,
production rule or assertion changed. Reuse the diagnostic ledgers and the
existing numeric-resume mode to finish fresh constitution/delegation actions;
do not resubmit the parameter proposal or vote. Keep uninterrupted numeric
acceptance open and preserve the original failure. Dependent snapshot acceptance
still requires the complete governance state checks.
The first continuation failed as `burst-public-testnet-governance-resume-20261019`,
invocation `a3d4bee1f8a44cf886358c8af8c31379`, terminal exit 1/MainPID 0. Output is
`fresh-certified/package-governance-resume/`. It uses the existing numeric-resume
mode, recovered diagnostic ledgers and a copy of the original wallet profile.
The confirmed parameter proposal/vote are not repeated. The phase-logging test
change is isolated in `governance-resume-tests/`; previous inputs remain intact.
Preflight revalidated lifecycle, diagnostic and failed governance receipts and
chains from the latest diagnostic. The 600-second native deadline and all job
resource caps remain unchanged. Receipt
`f270befe185187de89a848f6b192f32ff83dd1659d9d36e97129627762b69ff6`
revalidates as failure. Node synchronization and retained lifecycle checks passed;
the native runner then refused the retained profile before dispatching governance.
There is no governance progress file. No amendment, proposal or vote repeated.
The job used 96.35 CPU seconds and peaked at 103,731,200 bytes.

The runner's profile guard omitted the existing numeric-governance resume mode.
It now permits retained data only with the certified governance journey, no
delegation-only mode, and an exact 64-hex numeric proposal identifier. The
governance test still validates the saved proposal's proposer, type and value.
Syntax passes. This is test setup, not a production or protocol change. The
corrected deployment uses separate test/output directories and chains from
`f270befe...`; it reuses the same diagnostic baseline because no governance
actions ran in the refused attempt. It does not repeat the activation scenario.
Corrected continuation `burst-public-testnet-governance-resume-fixed-20261019`
passed, invocation `cf4aaf6e39b149a197db53f72f09a180`, terminal exit 0/MainPID 0.
Output is `fresh-certified/package-governance-resume-fixed/`. Its result and logs
prove reuse of the certified numeric proposal, a fresh multilingual amendment,
direct and delegated voting, certified adoption with 21 yea votes and post-count
delegation revocation. The amendment became certified Activated at 04:12:42 UTC,
within the unchanged 240-second wait starting 04:09:13. No timeout changed.
Governance and retained lifecycle verifiers pass on all three nodes before and
after SIGKILL. Recovered epoch is `c83ed414...`, with matching signing readiness.
This closes this combined constitution/delegation/recovery scenario. The original
numeric timing failure remains recorded; this is not uninterrupted numeric
acceptance or all governance parameters. Independent-host state recovery now
passes in the combined P4/P5 check recorded above.
Reuse the resulting ledger for snapshot acceptance; do not repeat these actions.
Governance receipt `884771a1c4c01c565d66e661dab5d8e00beec08065c59b6887f99de3c90363a4`
revalidates as pass.
Current snapshot acceptance passed as `burst-public-testnet-snapshot-20261019`,
invocation `699238ca18424c3fb27eeed946a16fde`, terminal exit 0. Output is
`fresh-certified/package-latest-snapshot/`. Governance `884771a1...` and the
previous snapshot receipt `629774d4...` revalidated before launch. The existing
driver and compiled verifiers are reused with the current staged daemon and a
copy of the completed governance ledger. It checks attestation/export, exact-epoch
authentication, tamper rejection, installation, SIGKILL, reconnect and a new
payment. No build or earlier user action repeats. The 15-minute scenario deadline
and 1 CPU/3 GiB/no swap/20-minute job limits are unchanged. Receipt
`5fc2a1a1f9cd701d63719dca826f131d0eabafb0ec2e773539aba19705de600f`
and its saved outcome/exit code were checked again without executing the scenario.
Authentication, tamper rejection, installation, crash/reconnect recovery, exact
lifecycle/governance state and a new payment pass on the same host. This does not
prove independent-host, NAT or in-flight recovery. Reuse this result; review the
existing evidence for those other scopes before scheduling any replacement.

P5 setup guidance now explains creator-led verification of initial participants,
public-address-only handoff, paid bootstrap endorsement, sequential confirmation
and irreversible bootstrap exit. It distinguishes human verification from test
funding and disallows fixture identities in the public genesis set. The wording
was checked against the whitepaper's Genesis section, decision 15.2,
`genesis_endorse_locked`, endorsement economics and `BootstrapPhase`. No real
endorsement was issued and no runtime acceptance is claimed from this doc change.

The fresh certified UI driver now waits for `peer_matches` immediately before
its first vote submission, then rechecks selection context and unchanged voter
head. This addresses the recorded October 18 failure during checkpoint propagation.
It does not retry a signed vote, change protocol guards, increase deadlines or
replace fresh acceptance with a resumed journey. Syntax checking passes;
staging runtime acceptance remains required. Only the test driver changed.

The interrupted verification-record check completed with 132 passing library
tests and exit 0; no local validation service remains. Receipt
`target/validation-evidence/verification-records-20261019/corrected-receipt.json`
revalidates with fingerprint
`0ee9c04d3413c547917f6636ea5c715c7c655d0db3dabb937ac787b6208baadb`.
It covers codec round trips, including existing selection/appeal fixtures,
not durable migration or release acceptance. The first compile failure is
retained. No test was rerun to close this interrupted evidence record. Further
verification-record integration is deferred pending release-impact evidence.

BRN per-block projection replacement is implemented and passes focused integration.
Bounded before-images cover insert/remove/mutable access, including burn/stake
detach-and-reinsert and compensation. Persisted encoding, accrual and stake
sequencing are unchanged. Local `brn-transition-20261019/corrected-receipt.json`
retains 37 passing crate cases; initial fixture failures remain alongside it.
VM1 `/home/burst/brn-recovery-20261019/receipt.json` retains eight passing node
cases covering full-projection equivalence, cache deltas, sparse projection,
identity arrival order, both payment formats and publication/download crashes.
Build took 1m18s; cases took 27.57s. Both units ended with exit 0 and MainPID 0.
Runtime receipt fingerprint: `8b11a84852b2e5c5c4e50e5fd1409bdc7b47668dadce73f10bd57c614235a4ff`.
This is a completed migration step, not C02/C05 or public release sign-off.

Mint-origin/expiry/reverse indexes now use the same transition journal. Full
projection remains the test oracle and final publication path. Three new tests
pass for independent storage-key encoding, saturation/replacement and exact full
projection; the passing sparse-projection case includes 1,024 historical mints. The four
changed files and prior originals are on VM1 at `/home/burst/mint-recovery-20261019`.
Build unit `burst-mint-recovery-build-20261019`, invocation
`d950154e491e4ad1b075f961da4415ca`, finished with exit 0 and MainPID 0 in 1m05s
under 1 CPU/6 GiB/no swap/20 minutes. `build-result.json` records pass. Runtime
unit `burst-mint-recovery-runtime-20261019`, invocation
`4062bba2a1cf44e4a63bf153fc5e838a`, finished with exit 0 and MainPID 0 under
the 3 GiB runtime cap. All eleven cases passed in 26.64 seconds, including the
retained BRN, payment-journal, identity and publication/download crash cases.
The reused driver verified the build receipt before execution. This completes
the targeted mint-index migration and focused integration checks; no daemon
build or unrelated wallet/NAT acceptance ran.

Remaining public-testnet work includes supported operating limits,
distributed-artifact end-to-end qualification and P5 distribution. The optimized
130-checkpoint cold-retirement case passes on the current node-test source.
Global engine/final projection work remains on the scale roadmap and is a
conditional release blocker as defined in the pilot priorities. C02/C05 remain
open; historical full-release rows below do not override the new priority table.

### Evidence reconciliation before further implementation

Open capability rows do not mean their functionality is absent. Keep these
distinctions when reporting progress or choosing the next change:

| Scope | Current classification | Evidence or concrete gap |
| --- | --- | --- |
| Live BRN delta persistence | Implemented | `economic_candidate/staging.rs` calls `ledger_updater::persist_brn_delta`; it writes changed wallet rows, deletions, rates and stake sequence |
| Recovery BRN projection | Per-block replacement implemented; focused integration passes | Partial capture omits unchanged BRN wallets and consumes mutation-owned rows. Full-projection equivalence, payment and crash recovery pass on the current node test artifact. Final projection/global budgets and release qualification remain |
| Recovery mint indexes | Per-block migration implemented; focused integration passes | Historical mint indexes are omitted from partial capture. Independent encoding, full-projection equivalence, payment and crash checks pass. Final projection retains required origins; full-state bounds remain open |
| Fresh observer and restart on daemon `2afb7a79...` | Existing pass revalidated without rerunning | VM1 `suffix-batch.vEo5wkoe/observer-receipt.json`, fingerprint `d6fcbc7bfe248931204c80fccc0908f67c50800e3d82e6bfda9dd90df962c8cb`: three-source comparison, empty observer, SIGKILL and exact lifecycle/payment/governance state |
| Cold retirement past height 128 | Current optimized integration passes | VM1 `recovery-retirement-release-20261019/receipt.json`, `dbf5b967...`: empty receiver reaches checkpoint 130 in 70.273 seconds under the unchanged 90-second deadline; exact economics, retired records, payment and restart assertions pass. Private controlled-time test executable, not final daemon qualification |
| Installed governance, constitution and delegation journey | Existing pass revalidated without rerunning | VM1 `wallet-lock-package.HkOxebqD/certified-onboarding/package-governance/receipt.json`, fingerprint `dc6762107c0885350a3ad8909e3474c8dc46f122da0f62c3d8b6ca05a9211ab8`: fresh native proposals, direct/delegated ballots, activation, revocation and exact state after three-node SIGKILL. Not every parameter/fault or the changed source candidate |
| Isolated NAT/firewall/UPnP journey | Existing pass revalidated without rerunning | VM1 `current-nat-ready-20261018/receipt.json`, fingerprint `3d9266ff4f7dcf123e02c90bfdaaa3ad16f0e35041a0ba409110a49cc752809e`: outbound/double NAT, reconnect, multiple clients, forwarding and IGD emulator. Not physical routers or distributed certified quorum |

Both receipt checks above returned pass with unchanged recorded inputs and logs
when run from their recorded working directories. The initial check used the
source workspace instead and was refused for context mismatch; no scenario was
started. Existing governance assertions check exact certified tally, document
text/version and post-count revocation without rewriting that tally. Preserve
these positive results when selecting any narrower remaining acceptance check.

Five further VM1 receipts revalidate on October 19 without builds or scenario
reruns. All returned `outcome=pass` with matching recorded inputs and logs:

| Retained receipt | Proven scope | Remaining distinction |
| --- | --- | --- |
| `wallet-lock-package.HkOxebqD/acceptance-receipt.json`, `4fb0b859...` | Eight installed native Dev drivers, including UI-to-native lock | Dev regression, not certified onboarding |
| Same root, `certified-onboarding/panel-ready/receipt.json`, `af394ff0...` | Certified panel continuation, UI votes, payment/receive and three-node SIGKILL recovery | Completed funding/endorsements were retained; uninterrupted fresh acceptance remains unverified |
| Same root, `certified-onboarding/package-lifecycle-finish/receipt.json`, `c63ce15e...` | Retained challenge/appeal, restored payment/receive and three-node SIGKILL recovery | Composite journey; does not cover every lifecycle outcome/fault |
| Same root, `certified-onboarding/package-latest-snapshot/receipt.json`, `629774d4...` | Latest completed lifecycle/governance epoch export, authentication, tamper rejection, install, SIGKILL, reconnect and new payment | Same-host scope; independent-host evidence is separate |
| `current-wallet-inflight-20261018/receipt.json`, `cfe64f47...` | Exact dropped-payment retention across three-node SIGKILL, certified exclusion and reviewed replacement | Not a mid-commit crash or every action class |

These are evidence reuse decisions for the recorded artifacts, not new source
qualification or whole-capability closure. The fresh-observer receipt also
revalidates using its original `suffix-observer-plan.json` and recorded working
directory `/home/burst/independent-wallet.Q2eenbT7`. The initially guessed
`observer-plan.json` path was absent; locating the existing plan avoided an
unnecessary observer rerun.

VM2 was not running when the October 19 read-only check was attempted. It was
not started for evidence inspection. Local retained records preserve these
historical successes; their remote input/log integrity was not rechecked today:

- `recovery-cut-20261017/independent-peer-receipt.json`: retained snapshot,
  independent-host payment catch-up and SIGKILL recovery on `2afb7a79...`.
- `recovery-cut-20261017/latest-independent-handoff.json`: latest-epoch
  snapshot and exact pending/lifecycle/governance recovery; checked terminal
  receipt `e22df293...`, followed by intentional VM2 deallocation.
- `recovery-cut-20261017/current-independent-wallet-handoff.json`:
  independent package installation and recovered consumer journey. Its
  `completedPanel` and `finalObservation` supersede historical active/failure
  entries; terminal receipt `987df02e...` and all-source crosscheck passed.

Do not report independent installation or independent snapshot recovery as
unbuilt. The remaining distinctions are recorded artifact identity, resumed
versus fresh acceptance and any specifically changed dependencies. Offline
evidence access is not a failed protocol test.

The VM1 build receipt for daemon `2afb7a79...` was compared with its stable
source workspace on October 19. Thirty-one previously recorded input files
have changed. This comparison checked content hashes, not modification times;
new files were not audited, so 31 is not the full change inventory. Production
changes include replay deadline/events/governance/outcome paths, snapshot
continuation/recovery/publication, verification/governance ledger adapters,
TRST/graph storage and checkpoint undo projection. The workspace Cargo manifest
and test fixtures also changed.

Consequently, the successful `2afb7a79...` journeys establish existing behavior
on that artifact but cannot qualify today's recovery implementation. This is a
specific changed-dependency reason for a future candidate recovery run, not a
reason to rebuild unrelated wallet functionality or rerun all historical checks.
The subsequent public-testnet audit compared 690 current workspace-member
source/configuration inputs directly with that build receipt. It found 39
changed files and five additions: BRN `wallet_states.rs`, verification
`orchestrator/records.rs`, replay `mutations.rs`, and recovery `changes/recorded.rs`
and `changes/rows.rs`. The initial removal list also included `fuzz/Cargo.toml`,
which is outside the workspace-member inventory, not a deleted release input.
The audit script's selection predicate was corrected; no scenario was rerun.
This includes test source and is not a count of production defects.

| Changed dependency | Affected acceptance | Reuse / next action |
| --- | --- | --- |
| Replay, scratch indexes, recovery projection/publication and ledger adapters | P3/P4 exact lifecycle/economic state, rollback, join and crash recovery | Retain focused BRN/mint/payment/crash passes. Current daemon must be built and qualified at one integration boundary; old daemon passes are historical. |
| BRN engine wrapper and TRST helpers | P2/P3 accrual, burn/stake and settlement; native wallet dependency identity | Reuse semantic crate evidence. Check native dependency closure before deciding package reuse; no frontend redesign follows from this change. |
| Verification record codec and shared appeal validation | Existing appeal validation and future storage migration | 132 library cases pass. Codec is not used by durable paths; migration remains deferred. |
| Test fixtures and validation-profile Cargo configuration | Test execution and artifact provenance | Do not infer daemon behavior changes from test-only edits. Keep development-profile timing distinct from release qualification. |

VM1's capacity blocker is resolved. Its existing Standard SSD OS disk expanded
from 32 to 64 GiB after deallocation and a successful recovery snapshot. Ubuntu
expanded the root filesystem to 61 GiB; `df` reports 33 GiB available, above the
bundle builder's unchanged 8 GiB guard. The normal development node restarted
and retained build/runtime receipts revalidated. No VM, data disk or larger VM
size was added; no evidence or build cache was deleted. Desktop and remote job
limits are unchanged. Capacity evidence and temporary snapshot cleanup status
are in `target/validation-evidence/recovery-cut-20261017/capacity-20261019.json`.

The native dependency comparison against the retained 175-file pre-package inventory found changes
only in BRN engine/lib, TRST engine and the workspace manifest, plus the new BRN
wallet recorder. No recorded file is missing. This is a source-impact result,
not package compatibility approval. The node GUI has no protocol path dependency.
Retain each existing package's original source provenance when considering reuse;
do not attach a new source manifest to an old binary and call it rebuilt.

Both saved cold-retirement JSONL logs were inspected for the existing installed
recovery stage messages: neither contains any. The non-profiled log ends at
checkpoint 117 of 130 at 90 seconds; its total 439.12 seconds includes fixture
construction. The profiled log also fails, and its CPU samples do not identify
reliable parent callers. The subsequent stage-timed diagnostic below resolves
the missing timing evidence, but does not establish release performance. Reuse
those measurements; do not repeat the development-profile scenario unchanged.

Source inspection then found the isolated receiver subprocess never initialized
tracing. Its parent did, but subscribers do not cross a process launch. Two
test-only files now initialize the existing tracing subscriber in both processes,
route traces to stderr and optionally give the receiver its own create-new log.
The 90-second readiness deadline, bounded stdout completion channel and all
economic/restart assertions are unchanged. This fixes observability, not recovery.

The incremental diagnostic test build is on VM1 under
`/home/burst/recovery-retirement-stages-20261019`. Unit
`burst-recovery-retirement-stages-build-20261019`, invocation
`d7e591c2aa2f41f4ab74313bd42fae60`, is terminal failed with MainPID 0 and exit 1.
It stopped in preflight before creating the compiler log/result; no compiler or
scenario ran. Its service error was EACCES on the root-owned prior profile result.
Exact start/result copies are now owned by the validation user; originals and the
failed preflight plan/journal are preserved. Readability was checked as that user
before retry. Corrected unit `burst-recovery-retirement-stages-build-fixed-20261019`,
invocation `953894440a4944b1996e81dd4a367feb`, completed with exit 0 and MainPID 0.
The incremental test build took 1m03s; its result records pass. This is compiler
evidence only, not a scenario pass.
The setup
uses the existing source/target, 1 CPU/6 GiB/no swap/20-minute cap and preserved
originals; no daemon or package build was started. The prior mint build receipt
revalidated before deployment. The corrected build receipt revalidated as the
service user with fingerprint
`3508f766d9e7715ab1bffb8d67c6c02b022ca3ce6693018b66c22c71cbfd9d7d`.
Runtime unit `burst-recovery-retirement-stages-runtime-20261019`, invocation
`9c69df74160c47d1a5087616e600ecd9`, finished with saved outcome `fail`, exit 101.
The receiver reached accepted checkpoint 116 of 130 at 90.045 seconds, then
failed readiness. Later equality, restart and payment assertions did not run.
The host was confirmed deallocated at reconciliation and the same VM was started
to retrieve the evidence. After boot the transient unit is absent/inactive;
its default exit status is not the test result. No scenario was restarted.

The separate receiver log records 207 epoch-recovery calls, 205 with a replay
cache; 177 of 206 tail/inventory checks reused the projection. Installation
preparation took 43,616 ms across 115 completions; publication-inclusive time
was 47,120 ms. Epoch authentication totalled 18,367 ms, tail/inventory checks
19,790 ms and publication guards 12,834 ms. Applied economic replay totalled
3,663 ms. These timers overlap and are not additive CPU costs. Source exports
totalled 15 ms for 116 completions. The measurements do not support treating
missing caching or whole-state economic replay as the sole cause.

This is a traced, unoptimized `validation`-profile diagnostic under
1 CPU/3 GiB/no swap/20 minutes, not release-performance qualification. Preserve
the failure and unchanged deadline. Before another long cold-start run, compare
the measured repeated validation paths with the intended release build; do not
remove authentication guards or launch the deferred storage migration on this
evidence. Final-artifact qualification still needs sufficient host disk space.
The runtime receipt revalidated against `runtime-plan.json` in its recorded
source directory, fingerprint
`08db0f44a596e4997867b8d4cf5bbc2fac18009660416498d48c94237d4de935`.
Its result remains failed; total case time was 438.737 seconds including fixture
setup. Initial receipt-inspection commands used the wrong checker path and the
base plan instead of the runtime plan; neither executed a test. The 1.3 GiB
free-space constraint observed then is resolved by the capacity expansion above.

Source review identifies where cached recovery still walks history:
`ReplayCache::recover` calls `begin_advance`, which passes the complete authority
iterator to `compact::read_after`; installation preparation walks it again.
`JournalGuard::capture` also supplies authenticated history to separate journal
validators. `AuthorityHistory::iter` verifies certificates on each traversal.
This explains why a cache hit need not be cheap, but the stage totals do not
isolate each traversal's cost. Any reuse change must retain durable-record,
beacon, retired-history and signer-journal corruption checks. The next release
boundary is an optimized recovery check, informed by these saved timings,
rather than another unchanged development-profile timeout or a new storage format.

The unchanged source compiled once with `--release` on the same target cache.
Unit `burst-recovery-retirement-release-build-20261019`, invocation
`092bd606bb2749aa9480c2d44ac8a07b`, is terminal; saved build outcome is pass,
exit 0, and the unit has no remaining MainPID. Build receipt revalidated with
fingerprint `f995e6638844632dd969459f4aeabcf5b3e7bf6b3ea27ec9b2e582114e68ba68`.
Evidence root is `/home/burst/recovery-retirement-release-20261019`.
The build used 1 CPU/6 GiB/no swap/20 minutes. Its preflight revalidated both
prior stage receipts. The reused runtime driver selects only the cold retirement
case, with the same tracing, 90-second readiness deadline, archive/economic
equality, payment and restart assertions. Runtime unit
`burst-recovery-retirement-release-runtime-20261019`, invocation
`d38b5d823ca5463ba9b76c0b4fc65c83`, was confirmed active with MainPID 2614
under 1 CPU/3 GiB/no swap/20 minutes after the successful build check. It is now
terminal with saved outcome pass, exit 0 and no remaining MainPID. The one exact
case took 161.444 seconds including fixture setup, catch-up and restart/payment
checks. The isolated receiver reached checkpoint 130 in 70.273 seconds, within
the unchanged 90-second deadline. Build time was 8m33s.

Runtime receipt revalidated against `runtime-plan.json` with fingerprint
`dbf5b96762a523fb1e7227fe13d2ba1a936550d359eadf5a557e6af51cf82b2b`.
All case assertions ran: exact BRN/TRST and merger-graph equality, retained
original retired records, no snapshot base, confirmed payment and payment after
restart. Installation preparation totalled 3,226 ms across 130 completions,
versus 43,616 ms for 115 in the unoptimized diagnostic; these nested timers are
not additive CPU costs. No production patch, timeout change, fixture change or
unrelated test rerun was needed. Preserve the development-profile failures, but
do not treat them as evidence that this optimized path still fails.

This completes the cold-retirement integration question for these source inputs
and controlled-time private fixture. Do not rerun it unchanged or start a storage
rewrite to fix the old timeout. Creator identity has since been generated locally;
next work is final daemon/app qualification using its public key, while private
backup confirmation remains a launch gate. This test executable does not qualify the
distributed apps, real-clock network operation or an indefinite operating envelope.

The previous profile service log confirms receipt finalization failed with
`Inputs changed during validation`. Its start/result/perf logs remain retained;
do not manufacture a valid receipt for it. The new plan includes its start and
failed result and records the latest finalized cold-scenario receipt separately.

The standalone burn/stake API refactor begun during this audit was removed before
compilation. It moved existing behavior without connecting recovery deltas, so
it supplied no completed capability and should not trigger a validation cycle.
Earlier implementation changes are preserved. The full C01-C23 evidence audit
is not yet complete; do not infer a completion percentage from this table.

### Latest recovery batch

The isolated cold-start retirement journey used the existing contracts
executable. Its plan and syntax-checked driver are on VM1 at
`/home/burst/recovery-retirement-cold-20261019/`. It must cross 130 certified
checkpoints with an independent-process receiver, compare economic state and
archive retention, restart and admit payments. The prior group used its
20-minute cumulative budget before completing this case. This single attempt
keeps that same cap, readiness deadlines, 1 CPU/3 GiB/no-swap limits and 10-second
stop grace. It uses `prepare` against the incomplete lifetime receipt and audits
the passing contracts build. No source edit, daemon build, fixture relaxation or
repeat of completed cases is needed. Build audit returned `reuse`. Unit
`burst-recovery-retirement-cold-20261019`, invocation
`049a73a5572a4503ac30cd5a41af3e36`, stopped before a start record or scenario:
its checker variable referenced a nonexistent copy in the new evidence folder.
The original script and `preflight-error.log` are retained. The variable now
uses the existing checker path; no executable/source changed. Corrected unit
`burst-recovery-retirement-cold-fixed-20261019`, invocation
`ea598813f48044f3800f2fd862c630bd`, is terminal failed, MainPID 0 and no remaining
tasks. The selected scenario failed after 439.12s, exit 101. Its independent
receiver reached checkpoint 117/130 at the unchanged 90-second readiness
deadline. Recorded checkpoints at 10-second intervals were 20, 39, 60, 75, 88,
99, 107, 112 and 117. No economic mismatch was reported; later equality,
restart and payment assertions were not reached. This validation-profile timing
failure does not qualify the current release artifact or erase the older pass.

The isolated attempt answers the cumulative-budget question: the scenario fails
its own readiness deadline even with the whole job budget available. Do not
repeat it unchanged, increase deadlines, or attribute it to BRN projection
without evidence. The two unsuccessful attempts (group interruption, isolated
deadline failure) require a diagnosis checkpoint before another attempt. Retain
the passing build and all unrelated case receipts. The JSON event trace separates
about 349 seconds before the independent receiver from its 90-second catch-up;
finer-stage measurements and profile effects still need investigation.
The failed receipt verifies with fingerprint
`4b2b77429a1271e1de10a00a63ef1d3d9dd0c376f2930458b77720f6be7fc0c1`.

Diagnosis checkpoint: the validation profile inherits unoptimized protocol test
code (with existing optimized crypto dependencies), and `cfg(test)` enables the
full projection comparison oracle. Neither proves the timing failure is only
test overhead. Production also has history-growing work: `AuthorityHistory::fork`
copies its full proof file, and `RecordObservations::verify` visits retained
observations. Scratch records authenticate disk indexes repeatedly. These are
candidate costs, not a measured root cause. Do not remove integrity checks or
replace bounded storage with an unbounded cache based on this inspection.
The smallest next diagnostic is sampling the existing executable's cold receiver
and distinguishing it from fixture preparation. No source patch or optimized
rebuild is justified until that evidence is collected. VM1 has gdb/perf paths;
the software task-clock preflight succeeded. The failed receiver's temporary
directory was cleaned by the fixture, so it cannot be resumed for profiling.
One diagnostic wrapper is being prepared at
`/home/burst/recovery-retirement-profile-20261019`, using the same executable,
software CPU samples at 49 Hz and 4 KiB call stacks. Parent and receiver PIDs
must be analyzed separately. It uses the latest failed cold-run receipt for
`prepare`; no protocol build, deadline change or release acceptance is implied.
Build audit returned `reuse`. Diagnostic unit
`burst-recovery-retirement-profile-20261019`, invocation
`8abbbf5eb9914c11b6f6dd6f7eac6d4a`, is terminal failed with MainPID 0 and no
remaining tasks. The instrumented scenario took 438.82s and again missed the
receiver readiness deadline. Perf captured 20,520 samples and reported an
84.886 MB trace. This is diagnostic evidence, not an acceptance pass.

Saved-trace analysis is complete through the receiver symbol report. PID 64340
accounts for 16,999 setup samples; receiver PID 64620 accounts for 3,521 samples.
The initial report used whole-run percentages. `receiver-relative.txt` corrects
that with `--pid 64620 --percentage relative`. Rounded self-cost sums are 21.74%
for symbols containing `blake2`, `digest::` or `generic_array`, 8.06% for
authority-history symbols, 4.22% for curve/Ed25519 symbols and 0.96% for scratch
index/record symbols. These are symbol-name groups, not inclusive subsystem
costs. Generic hashing and serialization may serve several callers; the scratch
percentage does not bound the cost of scratch storage. Summed rounded rows total
101.80%, so these figures are approximate.

`receiver-chains.txt` retains a call-graph report from the same trace. It still
does not establish useful Rust caller attribution. The host's `perf version
--build-options` confirms DWARF and libdw unwinding support; do not install a new
profiler or repeat sampling on the assumption that support is missing. Report
units `burst-retirement-profile-relative-20261019` and
`burst-retirement-profile-chains-20261019` both exited 0 in about 3 seconds under
the runtime caps. The scenario unit is inactive with MainPID 0. No build or
scenario ran during this analysis.

The trace maps to the retained contracts executable, build ID
`8e19f718b0f2e3ddbe9b72971288ae84f8b2f8fc`. That file still exists and contains
`.eh_frame_hdr` and `.eh_frame`; missing executable paths or absent unwind
sections do not explain the caller limitation. `stack-inspect.txt` now shows
empty user stacks, short stacks and an invalid caller address matching hash
state (`a54ff53a5f1d36f0`); kernel call chains do unwind. The capped read-only
inspection exited 0 in 2.6 seconds. These samples do not support reliable Rust
caller attribution. Do not repeat the scenario unchanged or patch a guessed
hotspot. The BRN mutation batch above addresses an independently demonstrated
whole-state budget defect; it is not claimed to fix this timeout. The profile
does not yet attribute the
readiness failure to BRN projection, authority copying, signature verification
or scratch storage. Preserve integrity checks and the existing deadline. No
requirement became complete; cold retirement and candidate release gates remain
open, with the existing unrelated passes retained.

| Evidence | Proven scope | Next action |
| --- | --- | --- |
| `recovery-fixture-lifetime-20261019/build-receipt.json` on VM1 | Lifetime-fixture candidate compiles; unchanged production inputs | Retain unaffected evidence; affected test files were rebuilt in the contracts batch |
| Same root, `incremental_installed_recovery_and_successor_do_not_require_snapshot_creation.log` | Live cached imports, rejection/retry and retired-history restart pass in 2.00s without increasing stack limits | Reuse until relevant inputs change |
| Same root, `receipt.json` and `completed-tests.json` | Terminal incomplete run: 40 group cases passed, one download-fixture failure, two ignored helpers, one interrupted retirement case | Reuse completed cases; isolated cold retirement now failed its readiness deadline and needs diagnosis |
| `recovery-remaining-fast-20261019/receipt.json` on VM1 | Terminal failed run: 15 additional passes, three failures, one ignored helper in 25.47s; unchanged executable | Retain passes and failure evidence; the three affected cases pass in the contracts batch |
| `recovery-fixture-contracts-20261019` on VM1 | Three-file test batch compiled; all four affected cases passed; production inputs unchanged | Retain journal and crash-recovery evidence; two long retirement journeys remain unqualified |
| `recovery-retirement-cold-20261019/receipt.json` on VM1 | One selected cold retirement case failed: checkpoint 117/130 at 90s; complete job 439.12s, exit 101 | Diagnose current validation-profile catch-up cost; no unchanged rerun, larger deadline or build implied |

The contracts build ended with exit 0/MainPID 0, invocation
`72ffb347b4244140aea280013c7823af`. Build receipt
`0f2d886737fe601a1817eab4a1f8096f56a310b47ad12f3a3ef092bc8e451933`
verifies. Runtime invocation `a6fd53d74f60400ea314e40ed76e6f78` ended
successfully, with MainPID 0 and no remaining tasks. `result.json` records pass,
exit 0 and four completed cases. Both retirement cases remain unqualified.
The runtime receipt verifies with fingerprint
`0e162a439429b938c85483e43418870699430133ee32158e22a76a5e73cb10c5`.
The two payment cases took 3.89s and 3.92s, publication crashes 3.07s, and
download crashes 11.99s. These results close the four fixture failures, not
C02/C03. No build or scenario was repeated to reconcile this handoff.

The broad runtime reached its 20-minute deadline. Its main process ended with
SIGTERM; remaining processes in that owned unit were killed after cleanup did
not finish. The unit is now failed/timeout with MainPID 0 and no remaining tasks.
Receipt `0dd994d8636de6569b4730b2dbe6ceaa47e2032be4c7f75fcc2fde194d56a40e`
verifies as incomplete. The stage audit retains the passing build. No retirement
completion follows from the interrupted run. Subsequent remote jobs use the
existing desktop wrapper's 10-second stop grace as well as the unchanged
20-minute runtime cap.

The shorter-case runner initially stopped before creating a start record because
the finalized receipt was owned by root. `preflight-error.log` retains that
failure. Receipt ownership and generated-script quoting are corrected; shell
and embedded JavaScript syntax are checked before starting tests. No scenario
was rerun for these orchestration corrections.

The download-crash matrix reused a mutable source across scenarios. Its first
scenario resumed payments; the next observed `total_burned=17` against the
original snapshot's `16`. The fixture now copies the same immutable LMDB source
for each scenario using the existing `copy_to_file` pattern. Exact snapshot,
BRN, pending-claim and successful-payment assertions remain. This local test-only
fix is included in the current batch. Do not change the economic expectation to
accept cross-scenario contamination.

The shorter run's failed receipt
`66d6aa1f1aca247d9c9b0e7e327b817d30ec79efe86c76393f25c8ebe6709cde`
verifies. Both payment cases failed at a positional block-hash comparison after
adding the second burn and rejected return. Independent blocks need not retain
their local arrival order. The fixture now requires identical block sets, checks
the causal edges, and compares every selected economic table by block hash.
Pending return provenance, reopen and rewind assertions remain. The publication
crash case looked up the retired aggregate TRST metadata key; it now compares
the live engine with the validated storage loader's exact wallet encoding.
These corrections and download isolation passed as one test-only build/run batch.
The two retirement journeys still need individual capped runs. No capability or
release gate is closed by these fixture changes alone.

Still blocking C02/C03: complete projection integration, BRN/mint/global-state
delta migration, whole-state bounds, mixed-history and interruption qualification.
The wider capability and release gates in the completion plan remain open.

### Retained projection evidence

C02/C03 recorded projection promotion, 2026-10-19. The production source now
uses transition records for per-block accounts, frontiers, pending claims and
lifecycle metadata. It reuses the existing TRST delta traversal to include
holders whose expiry/revocation caches change without an account write. Both
paths use one shared account-normalization function. Full projection remains
for final publication and as a test-only differential oracle, not a second
per-block production strategy.

New checks cover expiry extension, revocation and portfolio removal without
changing signed balances, plus 1,024 unrelated accounts/claims whose rows must
not enter per-block projection encoding. Two existing integration groups cover
installed recovery and certified installation. This batch is not yet verified.
Its plan uses the latest recorder receipt, stable host source/target and unchanged
resource caps; no desktop compiler or daemon build. Evidence will be under
`/home/burst/recorded-projection-20261019`. That first build failed with E0433
at two `WalletRecord` references: the type is exported from `burst_trst::engine`,
not the crate root. Failure receipt fingerprint
`fae32b86b218639b2043f02c594f1285b9654083c480286702b40ba54a1c8638`
is retained. The dependent runtime command exited 1 before any scenarios because
there was no successful build/runtime plan. A receipt validity check alone had
incorrectly been used as the start gate. The runner and workflow now also require
`outcome=pass` and terminal exit 0.

Both module paths are corrected together. The retry uses the latest failed
receipt and unchanged integration scope at
`/home/burst/recorded-projection-fix-20261019`. Build
`burst-recorded-projection-fix-build-20261019` passed in 1m04s, terminal exit 0,
MainPID 0, invocation `c53daf6885754ab3a4ae33fb37821765`. Its receipt verifies
fingerprint `06177766be5637683b5b59180391bdc3ed544d32f5abe0c2371d0fec763cf9cb`.
Runtime `burst-recorded-projection-fix-runtime-20261019`, invocation
`619410d2209047bb9ebb479def76626e`, is terminal, MainPID 0. The saved scenario
exit is 134 and the runner exit is 1. Receipt fingerprint
`ab9e9837c7ee0a26c5c4449795ca1eb9891a4e81c122f2c47f5e4af23f728180`
verifies as a failure. The three cache/growth regressions passed; the certified
installation group passed 10 cases, with three subprocess helpers ignored.
The installed-recovery group aborted before its final summary. Do not rerun the
passing stages or rebuild this executable to diagnose the abort. The group log
shows eight completed passes before
`compact_runtime_rotates_only_after_checked_economic_installation` reports a
test-thread stack overflow. One capped existing-binary debugger capture is
completed as `burst-recorded-stack-diagnostic-20261019`, invocation
`a2fee0598859439b97a879f38498ede2`; evidence root
`/home/burst/recorded-stack-diagnostic-20261019`. Terminal exit 0/MainPID 0
means capture succeeded, not recovery. The trace stops at SIGSEGV in
`advance_target_from_peers`, nested through `advance_from_peers`,
`ReplayCache::recover`, metadata recovery and compact runtime startup.

The shared `recover_records_from_peers` loop now returns a heap-owned future,
so startup, cache recovery and tip refresh do not each embed its suspension
state. No stack, memory, deadline or assertion limit changed. The same batch
extends the existing payment fixture with burn rejection and returned receipt,
exact pending-provenance comparison, reopen and rewind. This closes a missing
positive assertion in the recorded plan, not a new protocol requirement.
`/home/burst/successor-replay-frame-20261019` retains originals and the build/run
plan. Build `burst-successor-replay-frame-build-20261019` passed in 1m04s,
terminal exit 0/MainPID 0, invocation `3a5f1b3749e449d0bd6d822ef4b99b24`.
Receipt `19a7e48fe41cf1fdeb43d62a7a375af6a390f758cc09fdfe3cdf018d7a3b6435`
verifies. Runtime `burst-successor-replay-frame-runtime-20261019`, invocation
`d3bd80fa74ad49ee89c3ffc4a89b9500`, is terminal with scenario exit 134 and runner
exit 1/MainPID 0. Receipt
`97784b02467aeac91b5052505a915ab5dde62395a39fa2d3e883f061c047dd25`
verifies as failure. Certified installation again passed all 10 cases in 10.23s.
Installed recovery advanced past the earlier rotation failure but aborted at
`incremental_installed_recovery_and_successor_do_not_require_snapshot_creation`
with another test-thread stack overflow. The expanded payment cases were not
reached and remain unverified.

Two group attempts exposed the same failure class. The current-binary audit at
`/home/burst/recovery-frame-audit-20261019` completed. It traces the second
failure through tip authentication into the fixture's poll body. Inspection of
the pinned `tokio-macros` 2.6.1 source confirms the test macro stack-pins that
body. Disassembly shows the failing fixture reserves 1,268,056 bytes in its
poll frame plus 223,608 bytes in its entry frame before nested recovery calls.
The streamed inventory checked 118 entry functions and 191 poll functions;
the next-largest poll frame is 747,048 bytes. `frames-streamed.json` and
`poll-frames.json` retain the assembly. An initial buffered symbol scan failed
at its output buffer; streaming fixed the diagnostic without increasing caps.

Only this measured outlier now runs as a heap-owned Tokio task on the existing
single worker. Its join failure propagates to the test; every assertion remains.
The test-only batch at `/home/burst/recovery-fixture-task-20261019` first checks
that exact failure, then runs remaining installed-recovery cases only if it
passes. Previously completed cases are skipped using their retained log and
unambiguous full-name matches against the new executable's test list. No daemon
or production source changed in this batch. The build terminated with exit 101;
runtime never started. Its JSON compiler diagnostics identify an LMDB write
transaction retained in the fixture future across an await, violating the
spawned task's `Send` bound. The transaction now ends in an explicit lexical
scope before that await, preserving the original rollback and assertions.
The corrected source hash matches the remote copy. Build
`burst-recovery-fixture-scope-build-20261019` started with invocation
`186acb94aa784b9a9bc4c64ddf915ede`, using the latest failed build receipt and
the same capped host and compiler cache. Evidence is under
`/home/burst/recovery-fixture-scope-20261019`. Build passed with terminal exit 0
and MainPID 0; receipt fingerprint
`713217380e752258b3d745a55d749f2849a856f0ce059b705f059cbb7f8a2e15`
verifies. Runtime `burst-recovery-fixture-scope-runtime-20261019` started with
invocation `d5d945d47b5e4a148fd8e5458db5c3a7`. It is terminal with MainPID 0,
runner exit 1 and scenario exit 134. The exact fixture still overflows the
Tokio worker stack; moving the fixture to a heap-owned task did not resolve
its poll-time stack use. No assertion completed. The remaining group was
correctly skipped. Preserve this executable for diagnosis; do not repeat the
unchanged run or raise stack limits. The next check must identify the nested
poll frames on this candidate before changing execution layout again.
That capture completed at `/home/burst/recovery-scope-frame-audit-20261019`,
unit invocation `195c2a6e7ff1486cbac28f72af3c2ab8`, terminal exit 0/MainPID 0.
It locates the overflow in `advance_target_from_peers`, nested through tip
authentication, `Session::import` and the fixture's combined live/restart body.
The fixture now ends its live-import task before starting retired-history
recovery on the same store and certificates. Assertions and resource limits
are unchanged. Build `burst-recovery-fixture-phases-build-20261019`, invocation
`fe8d2b958fb8446eb241ad32d64f346a`, uses the latest diagnostic receipt and
existing target directory. Evidence is under
`/home/burst/recovery-fixture-phases-20261019`. Build passed, receipt fingerprint
`8850cb5b55e35d9ef08973d116ef21e9d1c72f71ff2acf95562652973be17ddd`.
Runtime invocation `24798c8d10e149d097d383b593ba4601` ended with exit 101,
MainPID 0. The test passed its live-import phase without stack overflow but
failed in retired-history authentication because the split dropped the source
temporary directory. This was a fixture regression, not corrupted node data.
The directory guard now survives both phases. That correction is staged under
`/home/burst/recovery-fixture-lifetime-20261019`, using the preceding failed
runtime receipt. Build invocation `5cd5b6e17a304566b039720b416b2b4c`
passed with terminal exit 0/MainPID 0; fingerprint
`0c7d288bb1dbf57b9536e19ca035c40f861dfba8fd75f6165d6b7aeb4ad5c824`
verifies. Runtime invocation `a1da97e01a2d4b8d96ad087c90e7c29b` ended at the
deadline; its incomplete receipt and retained cases are listed in the handoff.
No production inputs changed. Passing cache
regressions and completed integration cases remain retained evidence. The promotion is not yet
integration-qualified. Remaining
BRN/mint/global projection work, total-state bounds and release qualification
are still open.

### Previous recorder batch, verified before promotion

C02 transition-recording implementation started, 2026-10-19. Payment replay's
account write, pending removal, rejection return and outgoing pending insertion
now use the existing verification ledger adapter. The adapter gains a fallible
`take_pending` operation with storage and replay implementations. This removes
direct payment-map writes from that path so recording can cover the same writes
as lifecycle processing. No guessed affected-wallet list or new replay engine
was added. A parity regression covers exact pending contents, single consumption,
returned-credit preservation and storage abort. The coherent batch compiles and
the ten planned checks pass below. Whole-state capture remains active until
the connected replacement is complete.
The verification and governance replay adapters now accept an optional shared
mutation recorder. It stores first-before/final-after rows keyed by table and
key, removes net-zero changes, checks continuity between repeated writes and
bounds retained rows/bytes. Adapter writes record before mutating their maps;
errors propagate to the candidate caller. Added checks cover collapsing writes,
deletion, discontinuity and exact account/pending/metadata adapter results.
The recovery observer is now connected. Recording resets before each observed
block and is enabled only in test builds. Production recovery still uses the
existing full projection. Tests compare the recorder's normalized account,
frontier, pending and lifecycle changes against that full projection; the
orchestrator snapshot remains owned by its engine. The interrupted edit's
undefined append helper is supplied. Empty values no longer disappear merely
because their recorded byte size is zero. The selected payment, merge, identity
arrival-order and live-crash integrations pass. This verifies those recorder
paths, not complete mutation coverage or release approval.

Resumption check, 2026-10-19: no local validation service exists. VM1 responds
through Azure Run Command; direct SSH timed out. The remote unit inventory
contains the retained failed token-recovery service, not an active build.
VM1 has 3.1 GiB disk free and 7.2 GiB available memory. Rustfmt parsing and
`git diff --check` pass. The previous live recovery receipt was checked against
the unchanged remote source before deployment and still passes fingerprint
`a7d46bf21b46a0726a8119ad60cb7652011fbbb7911fcda38aeae148c23c786a`.
One incremental test build passed in 1m04s in the existing workspace and target,
unit `burst-transition-recorder-build-20261019`, invocation
`e8e96ddce72947afa7742fdee7934ac4`. Its plan and original changed files are at
`/home/burst/transition-recorder-20261019`. Limits remain one CPU, 6 GiB RAM,
zero swap, 256 tasks and 20 minutes. No daemon build or new VM.
Build receipt fingerprint
`57df68c9e04a494c3e4fd83adbb67c44669697389e8a1bb3cf9f70c2ac543002`
verifies. A control-command receipt check initially used the wrong working
directory and was refused before scenarios started. Correcting the directory
reused that build; no recompilation. The ten planned filters passed in
`burst-transition-recorder-runtime-20261019`, invocation
`c7f7d9ef2cc54fb1b35bd73c3d3147d0`, with the existing 3 GiB runtime cap.
Both jobs are terminal with exit 0/MainPID 0. Runtime receipt fingerprint
`9208de39a766503311448f0d980b3e32e63fe9db07562c6f8f8ee1f5762958a5`
verifies against source, executable and saved logs. Each filter ran one test.
The live scenarios retain their SIGKILL boundaries and durable-state checks.
The optional control-command log summary could not use `rg` on VM1; receipt
verification and the saved scenario results completed before that command.

Next review cache-only account effects and lifecycle mutation coverage before
promoting the recorder. Reuse this executable for additional relevant fixtures;
rebuild only if their code or production dependencies change. C02/C03 remain
open for whole-state scans, bounded recovery and broader acceptance. Production
still uses full projection and all release gates remain closed.

Next architectural batch, 2026-10-19: transition-owned recovery deltas. Source
inspection confirms `derive` still encodes the whole projection after every
applied block and charges it against a fixed work budget. The prior disk-backed
changes do not remove that growth-dependent availability limit. The completion
plan now maps the existing payment, verification and governance mutation owners
and required differential/integration evidence. No protocol capability becomes
complete from this review. Next implement recording through those shared writes,
then replace per-block capture as one connected change. Do not rerun prior
passes or build a separate replay engine. No validation jobs were started here.

C02 live-worker stack diagnosis, 2026-10-19. One bounded debugger run reused
the existing node executable. Backtrace evidence is
`/home/burst/token-stack-diagnostic-20261019/debugger.log` on VM1, invocation
`10e4734dbb04456b94d614bcc0b82275`. It stopped at SIGSEGV in
`installed::metadata::recover_metadata`, called through compact authority and
compact runtime startup. The diagnostic capture succeeded; the underlying
scenario remains failed. Static disassembly via `burst-token-stack-frame-20261019`
shows a poll-frame reservation of `0xa2000 + 0x698` bytes (665,240 bytes), with
the fault in the entry stack probe. This supports cumulative nested-future stack
pressure, not a failed token assertion. No stack limit or timeout was increased.
Both shared metadata entry points now heap-pin the recovery future to reduce
caller frame sizes. The incremental build `burst-token-stack-fix-build-20261019`
passed in 1m06s, MainPID 0, invocation
`e6be91e5e9cb40dcab24d1b4033a69c9`, under the unchanged host build cap.
Evidence root `/home/burst/token-stack-fix-20261019`; prior failed receipt and
exact prior source were checked before deployment. Build receipt fingerprint
`cbf10e9c9bb79e9747a86f3d588682182bbd141fdfbf6fcc8437c30c3a7b35bd` verified.
Runtime `burst-token-stack-fix-runtime-20261019`, invocation
`b50be6b2196248949423ef9917d30982`, passed both previously failed live cases,
terminal exit 0/MainPID 0. Its receipt fingerprint
`a7d46bf21b46a0726a8119ad60cb7652011fbbb7911fcda38aeae148c23c786a` verified.
Both cases retain four SIGKILL publication boundaries, durable-state comparison
and successful restarted admission. The non-governance case took 3.12 seconds.
Stack sizes, resource caps, assertions and deadlines were unchanged. Retain the
nine preceding scoped passes and the failed batch receipt. This closes the
observed stack-overflow regression, not all recovery or public-release gates.
Debugger setup and both diagnostic jobs are terminal. Gdb was installed on the
existing validation VM; no additional VM was created.

C02 certified token replacement batch, 2026-10-19. Recovery publication no longer
collects all old holding keys or graph keys/order maps. It reuses storage's
256-key deletion batches for the two allowed token namespaces, then streams the
authenticated engine records into the same writer. Existing old-row validation
is retained. Ordinary persistence, ordering slots and block undo are unchanged.
The storage regression passes multi-batch replacement, abort, reopen, unrelated
key preservation, namespace rejection and refusal inside ordinary block undo.
Evidence: `/tmp/burst-token-projection-clear-20261019.log`, one test passed after
a 2.84-second bounded build. This is storage evidence, not recovery sign-off.

Next coherent integration batch combines this with expiry sharing below.
VM1 build `burst-token-recovery-build-20261019` passed in 3m14s, terminal
MainPID 0, invocation `c2653a1e55694322a1934cb42c7fa7e7`. Evidence root
`/home/burst/token-recovery-batch-20261019`. Prior projection receipt and all five
remote file hashes were checked before deployment; originals are retained under
`original/`. This is the first `validation` profile build in the stable target,
with 1 CPU/6 GiB/no swap/256 tasks/20 minutes. Its build receipt verifies with
fingerprint `c9d821eb9eb8080302dc4397fe96cf43e7c74338c27083986745983e305a0a3e`.
Runtime `burst-token-recovery-runtime-20261019`, invocation
`4e219f001900418e933beb70e6102f2c`, ran under the separate
1-CPU/3-GiB/no-swap/256-task/20-minute cap and is terminal failed (exit 134).
Nine cases pass: normalization, count publication, both payment formats,
merge/arrival rollback, graph migration, ordinary graph ordering and aborted
conversion. Both live publication-crash cases abort with `tokio-rt-worker`
stack overflow. Their logs contain no successful scenario summary. The failed
receipt verifies with fingerprint
`7523ca4e0399b6536551d0c87a7868473acb490d265bb17196de19f800591168`.
Preserve the executable and nine passes; do not rebuild or repeat the full batch.
The subsequent diagnosis and two-case fix verification are recorded above.
Do not repeat this failed batch or relabel its original receipt.
The host has neither gdb nor coredumpctl; core_pattern uses Ubuntu apport and
no source-directory core file was found. No compiler or runtime job remains.

| Changed dependency | Requirement/check | Prior evidence | Action and reason |
| --- | --- | --- | --- |
| TRST normalization | C02/decision 6.9 normalized recovery, payments, restart | TRST crate log below; prior projection node receipt | Reuse crate evidence; run affected node recovery because production normalization changed. |
| Token projection clearing and node publication | C02/C03 atomic holdings/graph replacement and matching undo | New storage log; prior projection node receipt | Reuse storage evidence; run merge/payment/arrival-order and live publication recovery with the changed callers. |
| Unchanged scratch indexes and sequences | C02 authenticated private records | Projection runtime receipt below | Reuse scoped tests; no need to rerun standalone index/sequence cases. |
| Functional validation profile | Build configuration only | Runner regressions | One capped host build for this batch; no daemon or wallet rebuild. This does not qualify a release artifact. |

C02 expiry normalization, 2026-10-19. `TrstEngine::set_expiry_period` previously
requested mutable access to every shared portfolio, detaching unchanged holdings
and defeating the existing shared-record delta fast path during recovery.
It now reads through the shared map and detaches only portfolios whose token
states or caches change. Pending and revoked states remain unchanged; expired
tokens can still revive when the configured period permits it (decision 6.9).
Whole-state scans, changed-portfolio copies and replay-owned maps remain.

One bounded local TRST build passed four focused expiry tests in 3.29 seconds
of compilation. The same executable then passed 107 crate tests, with two
ignored, in 0.69 seconds; log `/tmp/burst-trst-shared-expiry-20261019.log`.
New assertions prove storage sharing for unchanged holdings and compare the
previous normalization algorithm across active/expired/pending/revoked states,
stale caches, zero/extended/maximal periods and boundary clocks. Formatting
changed only the new test layout after execution; no rebuild for formatting.
No node or application was rebuilt. This production change still needs the
affected node recovery integration in the next coherent remote batch. The
earlier projection receipt below describes its prior source, not this change.
No complete protocol capability or release gate is signed off by these tests.

Validation scheduling update, 2026-10-19. The existing receipt audit and retry
guards remain in use; no new framework was added. Ordinary node checks now use
`tools/test-node.sh` with Cargo's `validation` profile (inherits `test`, debug
symbols disabled). Release-specific checks still require release artifacts.
Seven runner regressions passed under the desktop wrapper, including exact
selection, shard coverage, failure propagation and profile selection. These are
runner checks, not protocol evidence. Compile-time savings are not yet measured.
Reuse the completed projection build below; do not rebuild it to change profiles.
No protocol requirement became complete and no release gate changed in this edit.

C02/C03 projection-map migration integration verified, 2026-10-19. `Rows::capture` now
places encoded projection rows in existing authenticated scratch records instead
of retaining a full `BTreeMap` for each before/after state. Delta comparison and
final cache staging visit those rows through the same disk index. Row identities
bind both table and key; missing, corrupt or mismatched records propagate errors.
Journal changes retain their existing deterministic sort and operation budgets.
Account writes can alter the verified count, so final cache staging publishes
the replayed count after all account rows. A regression deliberately places the
counter before its account in hash traversal order and checks abort and commit.

Changed inputs: `scratch_index.rs`, `scratch_records.rs`, `installed/changes.rs`
and new `installed/changes/rows.rs`. New traversal releases its file lock before
callbacks so record lookups cannot deadlock; the existing index equivalence test
now exercises nested reads and early callback failure. Added projection identity
and corruption checks reuse the same storage implementation. Formatting and
diff checks passed; the remote build and twelve selected scenarios passed.
Reuse this executable and evidence for their reviewed scope. The new row tests
check identity/corruption and verified-count ordering; existing integrations
check payment, arrival-order, merge/finality and live publication recovery.
Do not rebuild the daemon or wallet, or rerun unaffected NAT/UI suites.
The replay-owned state maps, normalized TRST copies, whole-state capture work,
aggregate operation budgets and snapshot body limits remain unfinished. No
claim of bounded total memory, improved throughput or release readiness follows
from this source change.
Current build on VM1: `burst-projection-streamed-build-20261018`, invocation
`3f7c1c2b447c42e58aa4ae02232041e9`, observed terminal on 2026-10-19:
inactive, MainPID 0, Result success; `build-result.json` records exit 0/pass.
Evidence root `/home/burst/projection-streamed-recovery-20261018`. Three prior
source hashes and absence of the new file were checked before deployment; prior
files and the verified arrival-order receipt are retained under `original/`.
The same stable Cargo source/target and 1-CPU/6-GiB/no-swap/256-task/20-minute
build cap remained. Build receipt verified with fingerprint
`b4af4e761659629beee5ff423bd0ecdfdfb6b493de8d2dae734db3f72990f660`.
Runtime `burst-projection-streamed-runtime-20261018`, invocation
`8b04870adfc4438889fd3de90aabb51d`, ran under the separate
1-CPU/3-GiB/no-swap/256-task/20-minute cap. It is terminal successful, MainPID 0.
`result.json` records exit 0 and twelve executed cases; `receipt.json` verified
with fingerprint `9025aa937fbea950121a6ca6030f7e99a6e87f02941b1084d006b3fb336ab328`.
The live cases exercise SIGKILL at four publication boundaries, durable state
comparison and successful post-restart payment admission. The merge case checks
both storage formats and reverse journal application. This is scoped integration
evidence, not large-network qualification. No daemon, wallet, NAT or UI rebuild;
no new VM. Next inspect whole-state capture and replay ownership together before
another implementation batch; do not rerun these cases unchanged.

C02/C03 undo-record migration integration verified, 2026-10-18. Replayed rollback
records now use an ordered sequence over the existing authenticated scratch
record/index implementation. Finality reconciliation and row-format conversion
consume one record at a time; storage installs a fallible stream in the original
writer. Missing/extra records, duplicate blocks, primary insertion checks and
signer-journal protections remain enforced. A late read error poisons the writer,
so earlier staged records cannot commit. Existing aggregate operation limits
remain; this is not a claim that whole-state recovery is bounded.

Change impact: `scratch_records.rs`, recovery `changes.rs`, `changes/finality.rs`,
`metadata.rs` and storage `checkpoint_projection/undo.rs` form one connected
change. Existing recovery tests retain their assertions; two test-only lookups
consume owned records instead of borrowing a temporary collection. The extended
storage replacement/abort test passed locally under the wrapper. The
scratch-order/corruption, finality, payment-delta, graph conversion and live
recovery checks share one remote node build. Reuse the preceding lifecycle and
wallet evidence for its reviewed scope; do not rebuild applications yet.
The later projection migration above removes the before/after row maps; replay
state and whole-state capture work still prevent broader recovery sign-off.
Seven original scenario logs pass; the corrected
arrival-order scenario passes separately with unchanged production inputs.
The extended storage test passed under the desktop wrapper in
`/tmp/burst-streamed-undo-storage-20261018.log`: build 3.10 seconds, one test
executed. It proves late input failure aborts the writer and preserves the
previous journal, alongside the existing complete-inventory, invalid-record and
rewind assertions. No local validation process remains. Reuse this storage
result. The remote build and scoped integration evidence are recorded below.
Build passed in 7m05s: VM1 `burst-undo-streamed-build-20261018`, invocation
`4c908d79531b4f97a382f8288a4e8f66`, terminal MainPID 0.
Evidence root `/home/burst/undo-streamed-recovery-20261018`; stable source/target
unchanged. All seven prior source hashes and the preceding lifecycle receipt
were verified before replacement. Prior source files and receipt are retained
in `original/`; the previous immutable executable remains at its earlier root.
Build limits remained 1 CPU/6 GiB/no swap/256 tasks/20 minutes. Its receipt
fingerprint `77e67b690fd4a670a521dcf705cdefb9b3ab740c7a7df395b789d3eb6a60a875`
verified. Runtime `burst-undo-streamed-runtime-20261018`, invocation
`d451cbf048bc48b983d26a766ee7e633`, is terminal failed, exit 1/MainPID 0.
Seven test logs pass; arrival-order recovery fails at the assertion reading
the removed aggregate `trst_engine_wallets` key after row-format publication.
The unchanged production loader supports both formats. The runtime receipt
`4fed0e3748635a6722822eba4ba3859d17d7506fe89eb3c9718f13fa1182dabf` verifies as
failed. Its assertion counter says five because progress output splits the two
passing live-test lines; retain the receipt and use the original logs for those
scoped results. Do not rerun the seven passes to fix reporting.
A baseline diagnostic using the preserved pre-change executable is in
`burst-undo-arrival-baseline-20261018`, invocation
`67b844c1de7242faa25ae8f2268a0416`; log `baseline-arrival.log` at this root.
That baseline also failed at the exact same aggregate-key assertion, terminal
exit 101/MainPID 0 in 0.35 seconds. This establishes a pre-existing test mismatch,
not an undo-migration regression. The local test now loads persisted TRST with
the production format-aware loader and compares the same persisted wallet fields.
All arrival-order, finality, lifecycle and rewind assertions remain. The single
test-only correction compiled successfully in 7m06s in
`burst-undo-arrival-ready-build-20261018`, invocation
`c9647ebec2814bdb8b9832ef77d2dad2`, terminal MainPID 0 on VM1.
Evidence root `/home/burst/undo-arrival-ready-20261018`; previous failed receipt
and baseline diagnostic are preserved in `original/`. The prior source hash was
checked before replacement. No production code changed in this correction.
Build receipt `2cdcdf8fcf0d02ed1856c2b4c8401f87d5e31e2fcbd0a49ec5b402a39b1e58ae`
verified. Runtime `burst-undo-arrival-ready-runtime-20261018`, invocation
`d08625b5ded249088921fba32d4c422b`, passed with exit 0/MainPID 0 under the
1-CPU/3-GiB/no-swap/256-task/20-minute cap. Arrival-order recovery executed once
and passed in 0.80 seconds, covering all three fixture orderings and rollback.
Runtime receipt `9ea5bac4dbf27afb59ba6093cb29b5ebf21cbe8db447c7ccec089d7fb267fc3b`
verified. All seven prior scoped passes and both failure logs remain preserved.
The new runner checks the executed test name
and successful one-test summary separately, allowing progress output between
them while still rejecting zero tests, failed summaries and nonzero exits.
The failed receipt remains unchanged. Build limits remain 1 CPU/6 GiB/no swap/
256 tasks/20 minutes, with the same stable source and Cargo target.
No daemon/wallet rebuild or new VM. This batch is not release acceptance.
Next remove whole-state before/after projection capture without changing the
verified rollback, finality or publication behavior. Do not rerun this batch
unchanged. No validation job from this batch remains active.

C02 lifecycle comparison verified, 2026-10-18. Recovery's engine-cache
check previously collected replay-owned metadata under a 131,072-row/64-MiB inventory
cap, although replacement already supports larger inventories. The change uses
the existing transaction-bound metadata iterator and compares each row directly
with replay state. Exact counts reject missing rows; key/value comparisons reject
extra or altered rows. The collecting compatibility API retains its limits.

| Changed dependency | Affected check | Reuse / next validation |
| --- | --- | --- |
| Storage lifecycle traversal | Large replacement, abort/reopen and visitor error propagation | Existing extended storage test passed; reuse its result. |
| Installed engine-cache comparison | Large lifecycle equality, missing/extra/altered rows | Focused comparison and existing installed recovery integration passed on VM1. |
| Production node/storage code | Previously built daemon and release acceptance | Prior binaries remain historical evidence, not qualification of this change. Build a candidate only after focused integration passes. |
| Unchanged wallet and receipt tool | UI and workflow regressions | Reuse existing scoped evidence; no rerun for this backend edit. |

These focused checks pass. The change does not remove
whole-state replay maps, snapshot body limits, or the other C02/C16 blockers.
The local node check was stopped before tests ran after measured memory
thrashing: 153,959,069 file-page refaults and 230,318,295 scanned pages at the
unchanged soft cap. Invocation `19856cb9aa16476d9acc9a1a49b6265e` is terminal;
the owned service is inactive with MainPID 0. Log:
`/tmp/burst-lifecycle-streamed-recovery-20261018.log`. This is incomplete
validation, not a pass, despite the service-stop command returning zero.
Do not repeat the node build unchanged on the desktop. Use the existing remote
workspace and capped build for the new comparison regression plus
`installed_recovery_rechecks_economic_caches_without_repairing_them`.
The smaller changed storage regression
`lifecycle_replacement_crosses_old_scan_limit_atomically` passed under the desktop
wrapper: build 3.79 seconds, test 0.25 seconds, one test executed. Log:
`/tmp/burst-lifecycle-streamed-storage-20261018.log`. It checks traversal of
131,073 rows, callback-error propagation, abort, replacement and reopen.
No local validation process remains. Reuse this storage result.
Remote build passed in 7m05s on VM1 in `burst-lifecycle-streamed-build-20261018`,
invocation `c9e5af9c85e94b858e1fa61998ced91c`, terminal MainPID 0.
Evidence root: `/home/burst/lifecycle-streamed-recovery-20261018`.
The existing stable source/target and 1-CPU/6-GiB/no-swap/20-minute build limits
are reused. Only `checkpoint_projection.rs` and `installed/caches.rs` changed;
their prior hashes were checked before replacement. Previous sources, receipt
and executable are retained under `original/`. The new build script copies its
emitted executable to `test-executable` before recording evidence, so a later
Cargo build cannot overwrite this attempt's artifact.
Build receipt fingerprint `66ea5c3693613380e7fabb39e4a5dfd281891bc7ed4b6c698a96dfe6507a2a28`
verified before runtime. Runtime unit `burst-lifecycle-streamed-runtime-20261018`,
invocation `5811941ad7484867849e070e2dde5068`, passed with exit 0/MainPID 0 under
1 CPU/3 GiB/no swap/256 tasks/20 minutes. Both selected tests executed and passed,
0.27 seconds each. Runtime receipt fingerprint
`ad524e6d0b05a36c10a37ca0e63bcd2fcdba1e6de9dba67540d64314e00dd418` verified.
This closes the lifecycle inventory comparison gap, not all C02/C03/C16 or
large-network acceptance. No daemon or wallet was rebuilt and no VM was created.
Next replace whole-state recovery-delta capture and its downstream installation
collections as one coherent change; reuse these tests at the relevant boundary.
The recovery-delta `changes.rs::Rows::capture` still collects lifecycle rows;
passing the new comparison test alone must not close large-state recovery.

Live governance publication recovery passed on 2026-10-18. The existing live
worker crash test now covers the encrypted scoped-delegation governance fixture.
It passes SIGKILL at committed, economics, authority and complete publication
boundaries, durable projection comparison, recovered delegated-key access and
revocation, stale-epoch refusal and new-epoch payment admission. The expected
projection also checks the two proposals' vote counts and cooldown state.
VM1 unit `burst-live-governance-publication-ready-runtime-20261018` is terminal
success, exit 0, MainPID 0; the focused case took 2.32 seconds. Evidence is
`/home/burst/live-governance-publication-ready-20261018/`; checked receipt
fingerprint `14022761d04e2b3aed7b7b73248a4be7f50e937702ddb242fa978f5620d5f86f`.
The first attempt failed because its governance fixture lacked initialized
genesis authority. The corrected setup uses production encoders and initialization;
no production guard was weakened. That failure and its executable remain retained.
The completed corrected build was reused, and the three earlier passing cases
were not repeated. No daemon or wallet rebuild was needed. This closes the live
governance publication crash scenario within C03/C12/P3/P4, not those entire
requirements or distributed public acceptance. Next implementation priority
remains the recorded C02/C16 whole-state and lifetime limits. P2-P5 stay open.

Independent installed-wallet recovery passed on VM2, 2026-10-18, using wallet
`dcfb91f5...` and daemon `2afb7a79...`. All three source nodes proved the saved
first vote was excluded by epoch closure. The wallet archived it without signing
a replacement, then completed seven separately reviewed votes, verification,
backup restore, BRN accrual, payment, TRST transfer/receive and spending guards.
The original funding, participation and endorsements were reused. This is a
composite recovered journey, not uninterrupted fresh acceptance. The original
RPC timeout and missing-test-module attempt remain failed evidence.
VM2 unit `burst-independent-panel-wallet-ready-20261018` is inactive, exit 0,
MainPID 0. Evidence is `/home/burst/current-independent-panel-ready-20261018/`;
receipt fingerprint `987df02e5097fb5283e1048a11daea4c4e57c23711ea643a53d754517d8e2742`
was checked from `/home/burst/current-wallet-install-20261018`.
All three source nodes pass the same report, including original endorsements,
participation, representative choice, exact received balances and provenance.
The crosscheck used the inspected verifier SHA-256 `cb1bcd78...`, exited 0 in
188ms, and is saved at `/home/burst/current-independent-source-20261018/panel-crosscheck.log`.
The owned source job is stopped with MainPID 0 and no remaining P2P/RPC listeners;
VM2 is confirmed deallocated. No binary was rebuilt. P2-P5 remain open.

Stage reuse audit, 2026-10-18: `validation-reuse.mjs audit STAGES` now checks
existing stages independently. A failed runtime cannot erase a verified build
pass. Missing or changed evidence is unverified, never automatic permission to
rerun. The 20 focused receipt-tool tests passed under `limited-run.sh`, including
mixed passing/failed/incomplete stages, missing evidence and changed inputs.
Only the receipt tool and workflow instructions changed; no daemon or wallet
build was needed. AGENTS.md requires this audit before multi-stage retries.
This completes the stage-audit safeguard, not a release requirement. P2-P5 stay
open. The audit still depends on accurate declared inputs and latest receipts.

Validation reuse safeguard, 2026-10-18: the existing preflight now refuses
unjustified retries after identical inputs are copied or renamed. Relocation
also preserves the retry budget. Path-sensitive receipt checks remain strict;
changed contents permit new validation, not an inherited pass. All 18 focused
checks in `tools/validation-reuse.test.mjs` passed under `limited-run.sh`,
including both relocation regressions. No application rebuild or cloud run was
needed. This closes a workflow loophole only; no release gate changed.
The NAT runner also requires existing binaries and an existing inspected image;
it no longer rebuilds prerequisites during acceptance. Its container has explicit
1 CPU / 3 GiB / zero swap / 256-process / 20-minute limits. The combined reuse
and NAT-runner safeguard check passed all 22 assertions under `limited-run.sh`
on 2026-10-18. README instructions now match that behavior. This proves runner
safeguards only. Broader NAT acceptance and P2-P5 remain open; do not
repeat passed lifecycle, governance or snapshot stages to validate this change.

Current-candidate isolated NAT qualification passed on 2026-10-18. Daemon
`2afb7a79...` passes outbound-only NAT, firewall interruption/reconnect, multiple
same-port and distinct-port clients, double NAT, manual forwarding and UPnP.
Signed burns, receives, merges, transfers, refusals and returns converge across
nodes. UPnP also passes lease renewal, crash expiry, mapping conflict protection,
automatic peer discovery and shutdown cleanup. The lab uses Linux namespaces
and an IGD emulator, with one Dev representative for transaction scenarios.
It does not qualify physical routers or distributed certified quorum.
VM1 unit `burst-current-nat-runtime-ready-20261018` finished successfully with
exit 0 and MainPID 0; its container was removed. Evidence is
`/home/burst/current-nat-ready-20261018/`, scenario `burst-nat.FSrJJe8V`;
checked receipt fingerprint
`3d9266ff4f7dcf123e02c90bfdaaa3ad16f0e35041a0ba409110a49cc752809e`.
The existing daemon and lab image were reused. Only changed helper dependencies
were compiled, in 46.76 seconds. Missing-checker and Docker-permission preflight
failures remain recorded; neither ran a scenario. P2-P5 remain open.

Latest live candidate, 2026-10-18: `2afb7a79...`, full SHA-256
`2afb7a79b7ba65226573327d7df3385f22be3970f0515fb0ef3bce4d3339383e`.
Fresh empty-directory catch-up PASSES in 152,852 ms under the unchanged
180-second deadline. After SIGKILL, recovery PASSES in 37,581 ms. Both stages
match the pinned epoch, confirmed heads and online signing readiness, then pass
lifecycle/payment and governance checks. The three source nodes pass the same
state checks after restart. The unit `burst-suffix-observer-20261018` is terminal
success, exit 0, MainPID 0. Build time was 3m40s using the existing source/target.

This completes the private loopback fresh-observer/abrupt-restart scenario, not
all of P4 or public release. Do not rerun it unchanged. Next reconcile and qualify
the remaining P2-P5 journeys against this candidate: installed-wallet flows,
in-flight lifecycle operations, snapshot/connectivity cases, platform handoff and
creator-owned network identity. Keep the full completion plan and gates intact.

Evidence: `suffix-build-result.json`, `suffix-observer-result.json`,
`suffix-observer-state.json` and `suffix-batch-observer*.log` under
`target/validation-evidence/recovery-cut-20261017/`. Remote artifacts and verified
receipts remain under `/home/burst/suffix-batch.vEo5wkoe/`. Acceptance fingerprint
`d6fcbc7bfe248931204c80fccc0908f67c50800e3d82e6bfda9dd90df962c8cb`.
Known-key private fixture only; do not distribute it.

Updated installed Linux wallet qualification passed on 2026-10-18. Package
SHA-256 is `7165429a2227bfb0e016312a017842c85fce071d37b38dfa11759b2784466b83`;
the installed executable is
`dcfb91f5dcaa4884c958b87e00c39fd3102552e047e8f42196120726695396d1`.
It includes the frontend lock fix and current address decoder. The existing
eight-driver embedded Dev regression passed with daemon `2afb7a79...`, including
backup/onboarding, signer binding, payments, UI-to-native locking, returns,
automation and group policy checks. Unit `burst-wallet-package-acceptance-20261018`
finished successfully, exit 0, MainPID 0; launch invocation was
`1e986b3136394e728a847f85deff367e`. Evidence is in
`/home/burst/wallet-lock-package.HkOxebqD/`, with `acceptance-receipt.json`
fingerprint `4fb0b859f27aada756bca8944d574e4752e2d71d1f60bee4fa626592165ee38a`.
The disposable profile is `profiles/burst-native-wallet-bsmtgS` there.

The wallet compiled once in 2m23s. Two post-build checks failed on validation
assumptions, not compilation: the tar member has no leading `./`, and Tauri
patches its bundle marker from `UNK` to `DEB`. The final check verifies every
executable byte after that one documented substitution. Original failed receipts
remain intact. No daemon or node UI rebuild was needed. This closes the updated
package's installed Dev regression. Certified continuation evidence follows;
independent-host recovery now passes as recorded above; uninterrupted fresh
acceptance and P2-P5 overall remain open.

The same installed wallet and daemon now pass the retained certified consumer
continuation and exact three-node SIGKILL recovery. Receipt hashes were checked
on VM1 in the original working directory. Unit
`burst-wallet-certified-panel-ready-20261018` is terminal success, exit 0,
MainPID 0. Its `panel-ready/receipt.json` fingerprint is
`af394ff0c93a65eff02f7c0246389c104043584fd84fb98050ced70c3efecf00`, under
`/home/burst/wallet-lock-package.HkOxebqD/certified-onboarding/`.
Fresh reviewed votes, verification, accrual, BRN payment, TRST transfer/receive
and exact balances/provenance pass before and after restart on all three nodes.
The earlier synchronization refusal and unopened-receiver setup failure remain
recorded. Funding and endorsements were not repeated. This is composite recovery
evidence, not uninterrupted fresh acceptance or independent-host qualification.
Keep the stopped `panel-ready/node{0,1,2}` baseline and `onboarding.json` for P3;
use copies for mutable runs. No unchanged rerun or rebuild is needed.

Current-wallet challenge/appeal recovery also passes as a composite journey.
The fresh prefix submitted a fraud challenge, spent before judgment (decision
17.3), cast seven reviewed votes and checked origin-only revocation. After an
unsigned synchronization refusal, the continuation submitted the exact-outcome
appeal, cast its seven votes and checked restored rights/TRST. Its final payment
UI timed out. Before any replacement, the final continuation verified the old
subject head, restored balance, empty recipient pending list and absence of an
active signed journal on all three nodes. One reviewed payment/receipt then
passed, followed by exact lifecycle state and balances after three-node SIGKILL.

Unit `burst-wallet-package-lifecycle-finish-20261018` ended with exit 0 and
MainPID 0. Receipt hashes verified; fingerprint
`c63ce15e16789f357c4c5b909e9fff81413a2728b61c92d96cd66078f2055c56`.
Evidence is VM1's `certified-onboarding/package-lifecycle-finish/` beneath the
wallet package root above. Its receipt scope and logs distinguish reused appeal
state from the final fresh payment; the review text inherited earlier wording.
The local `package-lifecycle-handoff.json` records that distinction and all failed
attempts. Preserve those failures. This closes composite fraud/meta-challenge
recovery on `dcfb91f5...`/`2afb7a79...`, not uninterrupted acceptance or all C08.
Governance/constitution/delegation qualification now also passes as recorded
below. No binary rebuild or lifecycle repeat is needed.

Current-wallet governance passes in one fresh journey on the retained network:
numeric proposal and certified activation, exact multilingual constitutional
content, 20 direct ballots plus one delegated ballot, and post-count delegation
revocation without changing the certified tally. Two pre-sign readiness refusals
were handled by the existing driver with unchanged-head checks and fresh reviews.
All three nodes retain the exact governed values, document and delegation state
after SIGKILL. The prior lifecycle verification and balances also match.
Unit `burst-wallet-package-governance-20261018` ended with exit 0/MainPID 0;
receipt hashes verified, fingerprint
`dc6762107c0885350a3ad8909e3474c8dc46f122da0f62c3d8b6ca05a9211ab8`.
Evidence is `certified-onboarding/package-governance/` beneath the VM1 wallet
package root. Reuse its stopped nodes and `governance.json`, with the earlier
`package-lifecycle-finish/lifecycle.json`, for further combined-state checks.
This closes this combined candidate journey, not all C10-C12 fault cases or P3/P4.

Latest-epoch loopback snapshot acceptance passes on the same `2afb7a79...` daemon.
Epoch `0a3f17873324cfd58af21f3db162267694d6fd204513837732dcba60c0d17d99`
was attested, exported, independently verified and restored; altered-archive
rejection, SIGKILL recovery and disconnect/reconnect passed. Exact saved
lifecycle and governance state matched throughout. A new signed payment
`da9261686a6c663fe6283d27fb44e60f4a85b9e288d20466ead87c8b3f359c76`
confirmed on source and receiver with the same pending amount.
Unit `burst-package-latest-snapshot-20261018` is terminal success, exit 0/MainPID 0.
Verified receipt fingerprint
`629774d4679f64090b5873961ecc043c82c69e6c08c4047eb322587849098e7e`;
archive SHA-256 `b1f9ee38ed316ac8336bbaf0eb13dcb66c1649342f148520103448350e6c07a6`.
Evidence is VM1's `certified-onboarding/package-latest-snapshot/`, runtime
`snapshot-runtime-tqwKzx`. Reuse that archive and stopped post-payment ledger
for further checks. Independent-host follow-up now passes below; no re-export
or loopback rerun is needed.
Separate older consumer/in-flight reports were not supplied; this qualifies
latest lifecycle/governance snapshot recovery, not independent-host, NAT or
in-flight recovery. Those gates remain open.

Latest-epoch independent-host follow-up passes on VM2. The exact archive above
was transferred over the private Azure network with hashes pinned through the
authenticated control connection. VM2 verified/imported it, fetched the
post-snapshot payment `da926168...` from VM1 and checked its exact pending amount.
Lifecycle verification, balances, governed values, constitution and delegation
match before and after SIGKILL. Unit `burst-latest-independent-peer-20261018`
ended with exit 0/MainPID 0. Receipt hashes verified; fingerprint
`e22df29392bb8fdb50fd2c35e80988d379002e851ca15ca1cfa593cdde61e438`.
Evidence is `/home/burst/latest-independent-20261018/` on VM2, with the source
copy under the same path on VM1. This closes latest-epoch LAN snapshot/suffix
recovery, not NAT or separate in-flight signing. No new VM, build or export was
needed. Keep these results; next reconcile the remaining P4 evidence.

Current retained-fixture snapshot qualification passed on VM1 in
`burst-suffix-snapshot-20261018`, invocation
`5d9bf8044f094c67a498398191dbd938`, terminal success, exit 0, MainPID 0. Evidence root is
`/home/burst/suffix-snapshot.Ce1B0oF8/snapshot-runtime-ZHWtg6`.
The current candidate's build receipt passed before launch. The unchanged driver
uses an immutable copy of the older retained tip-snapshot fixture and verifies
its source baseline before export. This is neither latest-epoch qualification
nor an independent-host test. Export, exact-epoch verification, tampered-archive
rejection, install, SIGKILL recovery, reconnect and a new payment all passed.
Lifecycle/governance/consumer checks passed at the required recovery stages.
The fixture epoch is `0be65f15cc1f50a99b32b87ec9bdbbdcbfb00940057c645b63e4c426e74dae62`.
The optional in-flight-action report was not supplied, so its result explicitly
records `recovered_actions_preserved=false`. No unchanged rerun is needed.

Its original 4 GiB disk preflight was met by removing only the unused Docker
image `rust:1.90-bookworm`, digest
`sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f`.
The registry manifest was verified before removal; the image can be pulled by
that digest. Native toolchains, targets, Docker cache volumes, fixtures and logs
remain intact. The separate cache archive transfer was interrupted; its process
is missing and the local archive is incomplete. Verify a complete backup before
deleting any of its source volumes or archive.

Independent-host follow-up passed on the same daemon. VM2 restored that snapshot,
caught the later confirmed payment
`311ee4d65e1a5fbd051a1e12e75053136ec75dd51a6c3f86c2218e576e1d899f`
from VM1, and repeated lifecycle/governance/consumer/payment checks after SIGKILL.
`burst-independent-peer-20261018` is terminal success, exit 0, MainPID 0;
invocation `17a8014ef77f4d1ea3c4f175a2f9f7f0`. Evidence and receipt are in
`/home/burst/suffix-independent-20261018/` on VM2. The source baseline and log are
in the same path on VM1. This closes the retained-fixture independent-host
snapshot/catch-up/restart scenario, not latest-epoch, NAT or in-flight recovery.
No new VM or build was needed. The existing VM2 was started only for this check;
its verified receipt was copied locally as `independent-peer-receipt.json`, and
Azure now reports VM2 deallocated. The temporary source is stopped; the normal
VM1 validation service remains active. The temporary artifact server was
stopped before protocol validation. Six transferred files matched SHA-256 values
obtained independently over SSH; no signing keys were served.

### Current reuse decisions

C19 reconciliation, 2026-10-18: directory publication/discovery is implemented,
not missing functionality. Decision 22.1 selects a public web/IPFS listing;
`groups/src/bin/burst-group-directory.rs` exports one and
`GroupClient::fetch_directory` imports it through the bounded registry parser.
The native `load_group_directory` command and wallet
`group-policy-settings.tsx` provide discovery and explicit selection/save.
`directory_publication.rs` checks exporter-to-HTTP-client discovery and
`tests/native-groups.mjs` checks installed Settings without automatic trust or
membership queries. Reading these assertions does not establish that they pass.

The historical acceptance manifest in
`target/validation-evidence/directory-acceptance-20261017/inputs.sha256` pins
daemon `a41e26c9...`, wallet `38c004a5...` and exporter `8c4fe2e1...`.
That local folder currently contains the runner, manifest and directory files,
but no terminal log. The earlier claim below that local logs are saved there is
not substantiated by this inspection. Its runner names remote evidence root
`/home/burst/directory-acceptance.bRpCsIvY` on VM2. Recover that existing log
before repeating historical acceptance; no VM was started for this review.
Current candidate qualification remains open regardless of that historical
result. Batch it into the final installed-wallet groups journey, using the
existing driver rather than adding another suite. No C19 gate is closed here.

Initial in-flight run is retained as a terminal failure:
`burst-inflight-recovery-20261018`, invocation
`e7ad61ea4852423cad81e7505d7f8c09`, exit 1, MainPID 0. Remote evidence and failed
receipt are `/home/burst/inflight-recovery.7q9uxGsp/`. The fresh dropped payment
survived a real native app restart byte-for-byte; the signer restarted locked
and duplicate signing stayed blocked. Ordinary governance activated, the old
epoch draft was rejected, and native resolution excluded it and cleared its
active journal. The final new payment was refused by the node synchronization
guard before submission. This does not prove end-to-end followthrough.

The retained followthrough subsequently passed in
`/home/burst/inflight-followthrough.h7d2MbN6/`, unit
`burst-inflight-followthrough-20261018`, invocation
`2c036d4a75a448d8ad2e51aa82f1e5aa`, terminal success, exit 0, MainPID 0.
It copied the resulting three ledgers and app profile, verified the archived
draft against its original SHA-256, the unchanged account slot, activated
proposal and cleared journal, then waited for `peer_matches` at epoch
`5bd66361...`. One separately reviewed payment `719c9614...` confirmed on all
three nodes. Preparation, proposal creation and governance were not repeated.
The receipt chain preserves the initial failure; it does not rewrite it as a pass.

Together these runs complete this tested dropped-submission/app-restart,
certified-exclusion and reviewed-payment scenario. They do not establish node
SIGKILL-in-flight, all operation types, or new frontend package acceptance.
The existing driver now saves a `.resolved` checkpoint and offers an explicit
followthrough mode; no fabricated checkpoint was needed for the old run.
Production admission and resource limits are unchanged. Do not repeat these
completed stages unless their dependencies or the required scenario change.

The current wallet `dcfb91f5...` and daemon `2afb7a79...` now also pass the
node-SIGKILL variant in `/home/burst/current-wallet-inflight-20261018/` on VM1.
While one dropped payment remained uncertain, the runner killed all three owned
nodes. They recovered the same account slot and epoch. The reopened wallet was
locked, retained the exact signed draft, and refused duplicate signing. Normal
governance then certified epoch closure; the old block was rejected, its journal
archived, and a separately reviewed payment confirmed on all three nodes.
Unit `burst-current-wallet-inflight-20261018`, invocation
`c56bba4e28e2484d81157512cbdf798f`, exited 0 with MainPID 0. Checked receipt
fingerprint is `cfe64f47d0d5fe422d468e859b5ce698e25db9e4b029952d191a28f95e7eb242`.
No binaries were rebuilt. This completes this current-package recovery scenario,
not every operation type or a crash during consensus/ledger commit. P2-P5 and
the full completion plan remain open.

| Artifact/evidence | Scope and status | Invalidation / next action |
| --- | --- | --- |
| `suffix-batch.vEo5wkoe/receipt.json` on VM1 | Grouped protocol regressions passed; one compiled test executable | Recheck declared source/toolchain inputs before reuse; no unchanged rerun |
| `suffix-batch.vEo5wkoe/build-receipt.json` on VM1 | Candidate `2afb7a79...` built | Production dependency, toolchain, feature or build configuration changes require a new build; wallet-only edits do not |
| `suffix-batch.vEo5wkoe/observer-receipt.json` on VM1 | Private loopback fresh join and abrupt restart passed | Reuse only the recorded candidate/scenario; does not cover multi-host networking or public identity |
| `suffix-snapshot.Ce1B0oF8/receipt.json` on VM1 | Retained-fixture authenticated snapshot, crash/reconnect, lifecycle/governance/consumer state and new payment passed | Same candidate and fixture scope only; independent-host follow-up passed in the next row. Latest-epoch and separate in-flight recovery are not covered |
| `suffix-independent-20261018/receipt.json` on VM2 | Independent-host retained snapshot, live post-snapshot payment catch-up and SIGKILL recovery passed | Same candidate/fixture over Azure LAN only; does not prove NAT, latest-epoch or in-flight recovery |
| `recovery-cut-20261017/wallet-lock-shell-receipt.json` in local validation evidence | Three mocked-IPC lock regressions pass: sidebar/settings/mobile pending, duplicate suppression, failure state and successful retry | Source/test/browser dependency changes invalidate this scope; updated installed package now also passes the Dev regression below |
| `/home/burst/native-lock.CwLcWz0I/receipt.json` on VM1 | Installed wallet `eae8f097...` passes real IPC signer binding for 11 actions, locking/cancellation and positive fixture restore | Historical artifact scope only; newer package passes the grouped Dev regression below. Certified full journeys remain open |
| `/home/burst/wallet-lock-package.HkOxebqD/acceptance-receipt.json` on VM1 | Installed wallet `dcfb91f5...` and daemon `2afb7a79...` pass all eight existing embedded native Dev drivers, including UI-to-native lock | Reuse exact package and reviewed scope; no unchanged Dev rerun |
| Same root, `certified-onboarding/panel-ready/receipt.json` | Certified consumer continuation and exact three-node state after SIGKILL on the same wallet/daemon | Reuse stopped baseline copies for P3. Changed binaries or relevant inputs need qualification. Independent installation, broader lifecycle and public identity remain open |
| Same root, `certified-onboarding/package-lifecycle-finish/receipt.json` | Composite fraud challenge, appeal restoration, restored payment/receive and exact three-node SIGKILL recovery | No repeated challenge/appeal/panel. Combined governance baseline now available below; wider C08 fault/outcome cases remain open |
| Same root, `certified-onboarding/package-governance/receipt.json` | Fresh parameter/constitution/delegation journey; exact governance and retained lifecycle state after three-node SIGKILL | Latest-epoch loopback snapshot now passes below. Independent installation and wider C10-C12 cases remain open; no unchanged governance rerun |
| Same root, `certified-onboarding/package-latest-snapshot/receipt.json` | Latest lifecycle/governance epoch authenticated export, tamper rejection, install, SIGKILL/reconnect and new payment | Independent-host follow-up passes below; no re-export. Broader in-flight recovery remains open; dropped-payment recovery and isolated NAT are qualified separately |
| `/home/burst/current-wallet-inflight-20261018/receipt.json` on VM1 | Current installed wallet retains an uncertain dropped payment across three-node SIGKILL/app restart, certified exclusion and reviewed replacement | Same candidate/driver/fixture scope; not every action type or mid-commit crash. Reuse completed preparation and governance |
| `/home/burst/current-nat-ready-20261018/receipt.json` on VM1 | Current daemon isolated NAT/firewall/UPnP scenarios, signed Dev transactions, automatic mapping discovery and cleanup | Reuse same candidate/helper/image/driver identities; not physical-router or distributed-quorum acceptance |
| `/home/burst/latest-independent-20261018/receipt.json` on VM2 | Latest-epoch authenticated snapshot, live post-snapshot payment/pending claim, exact lifecycle/governance state after SIGKILL | Azure LAN only; not NAT or in-flight signing. Reuse exact archive/receipt; no unchanged rerun |

Wallet lock UI verification is terminal success, exit 0, three tests in 5,141 ms.
The first full-Chrome launch failure and the affinity-only page crash remain in
their incomplete receipts. Matching Chromium 1243 headless shell passed a smoke
check, then the unchanged wallet tests passed under the original wrapper caps
with CPU0 affinity and UV/Go workers limited to one. No daemon rebuild or repeated
type-check was needed. Reuse `check-wallet-lock-shell.sh` and its plan when
planning related browser checks; do not repeat environment troubleshooting.
The earlier TypeScript completion remains historical stage evidence, not a
separate prospective receipt. Native follow-up is terminal success, exit 0,
MainPID 0, unit `burst-native-lock-20261018`, invocation
`b1d690ac1e1d479c891c093d1d15f2de`. The existing native signer driver now checks
all 11 actions after lock, automation cancellation before any manual action can
cancel it, positive signer restoration and a fresh automation session. Its new
`--signer-only` selection reuses the isolated native runner without repeating
unrelated payment/onboarding suites. No binaries were rebuilt. Native evidence
is in `/home/burst/native-lock.CwLcWz0I/` and
`/tmp/burst-native-wallet-yFni9f/` on VM1. These checks close the tested lock
scenarios, not all C18/P2; P2-P5 and the full release remain open.

### Recovery change and prior evidence

Previous live candidate, 2026-10-18: `fba9f1f6...`, full SHA-256
`fba9f1f671667dd1e87722877b42720f3a09736a519843aa3d38641d899f5237`.
The metadata-handoff group passed 28 tests using one compiled executable.
The daemon build passed in 3m43s. Three-source restart checks passed for exact
epoch, heads, signing readiness, lifecycle/payment and governance state.
Fresh catch-up still FAILS the unchanged 180-second deadline; abrupt restart
did not run. The observer completed 43 installations. All three jobs are
terminal, not active. No public requirement became complete.

Evidence: `metadata-handoff-result.json`, `metadata-build-result.json` and
`metadata-observer-result.json` under
`target/validation-evidence/recovery-cut-20261017/`. Remote artifacts and
receipts remain under `/home/burst/metadata-handoff.h7YCsybe/`.
Metadata authentication totaled 3,779 ms; installation preparation totaled
57,014 ms. These overlapping wall times are not additive CPU measurements.
Saved witness timings show 283 suffix jobs taking 58,156 ms; 41 slow jobs account
for 57,093 ms. All 283 peer batches have distinct starting hashes. Another cache
does not address those requests. Silent server throttling is a plausible source
of the one/two-second pauses, but the old logs cannot prove that attribution.

The current worktree batches single-block account suffixes through the existing
exact-head transport, up to its existing 64-head limit. Longer account suffixes
retain their bounded chain walk. Both paths share byte-budgeted tail insertion.
Certificate-derived cursors, proof authentication, content recovery and replay
remain required. No wire format, resource cap or release gate changed.

The existing throttle test now checks cached service at the load limit and a
valid deferred request succeeding after window reset. The existing long-suffix
fixture also checks exact-head retrieval, duplicate rejection and no local block
installation. New debug events identify exhausted server budgets.
Bounded `cargo check --tests` passed in 9.40s. The subsequent remote batch
`burst-suffix-batch-20261018` is terminal success, exit 0, MainPID 0. Six witness
tests, long-suffix retrieval, snapshot-free full-proposal catch-up, four-stage
SIGKILL recovery and historical TCP restart passed. One compilation took 7m02s;
the remaining filters reused that executable. Logs show two four-head batches
and longer walks across the recovery sequence; no single logged checkpoint
combines both paths. The tests check recovered content and payment operation,
not just rejection of malformed input.

Remote evidence is `/home/burst/suffix-batch.vEo5wkoe/receipt.json`, verified with
fingerprint `e6b52c5e52858a3f1f52994971f8f0bcb46968d8e534222f66893ae0368edfbb`.
Local `suffix-batch-result.json`, `suffix-batch-state.json` and
`suffix-batch-run.log` retain the result, handle and logs in the evidence folder
above. No unchanged test retry is needed. These edits are included in candidate
`2afb7a79...`; no full release requirement became complete.

The subsequent candidate build and fresh acceptance passed as recorded above.
No unchanged test or acceptance retry is needed. Public release gates remain closed.

### Earlier evidence

Previous live candidate, 2026-10-18: `fe3350ce...` built in 3m42s and passes
three-source restart with exact epoch, heads, lifecycle/payment and governance
state. Fresh catch-up FAILS the unchanged 180-second deadline at sampled epoch
`63e5764d...`; abrupt restart did not run. The observer completed 41 installations.
The unit is terminal (exit 1). `panel-observer-result.json` and its verified remote
receipt preserve this failure under `/home/burst/panel-reuse.K7U5DR4K`.

Preparation totaled 58,300 ms and preparation plus publication 65,761 ms.
Ninety metadata epoch authentications totaled 70,554 ms; 88 already had a caller
cache. These are overlapping wall times, not additive CPU. Metadata recovery was
not consuming the committed replay handoff already used by checkpoint import.
It now prefers that existing handoff when caller progress is absent or behind,
then runs the normal durable-history, journal and publication checks.
`burst-metadata-handoff-20261018` subsequently completed under the existing
remote limits, with source and target unchanged.
The extended authority regression checks no-cache and older-cache reuse,
same-epoch reuse without another fork, exact state and corrupt-history rejection.
Its results and the subsequent candidate acceptance are recorded above.
No full requirement is newly complete; release gates remain closed.

Previous candidate `bd93ca7c...` includes TCP_NODELAY on both
production peer-connection paths. Eight connection tests and the bounded node
compile pass. The remote build passed in 3m54s using the stable cache. Three-source
restart preserves the pinned certified epoch, confirmed heads, lifecycle/payment
state and governance. Fresh catch-up still FAILS the unchanged 180-second
deadline at sampled epoch `1e5c9cba...`; abrupt recovery did not run. The service
is terminal (exit 1), with no candidate processes left.

Evidence: `peer-latency-observer*.log` and `peer-latency-observer-result.json`
under `target/validation-evidence/recovery-cut-20261017/`; remote root
`/home/burst/peer-latency.eXiS8ZxO/`. The observer completed 38 installations.
Parent recovery totaled 1,436 ms; preparation 60,474 ms; preparation plus
publication 67,728 ms. Its 250 suffix witness jobs totaled 49,951 ms. These
overlapping wall times are not additive CPU costs. The service used 231.70 CPU
seconds including source startup.

The bounded profile is complete. The retained partial observer reached the exact
pinned lifecycle/payment/governance state in 166,368 ms and recovered the same
state and signing readiness after SIGKILL in 49,523 ms. This is retained-state
recovery evidence, not fresh-node acceptance. Observer-only CPU samples show
32.57% Blake2b compression, 7.38% base32 encoding and 6.02% address decoding.
Stacks were insufficient for reliable parent attribution. See
`replay-profile-result.json` and `replay-profile-hotspots.json` in the evidence
directory above; raw samples remain on VM1 under `replay-profile.5pqm2A9q`.

The next batch replaces redundant address re-encoding with the equivalent
four-padding-bit check and reuses one authenticated selection certificate within
each replay scope. Checksums, panel identity/context checks and durable-history
guards remain. Address differential tests pass. The grouped run is terminal
(exit 101): 123 passed, two failed, three ignored subprocess helpers. Those
helpers run through passing parent crash-recovery tests; they are not missing
standalone coverage. The failed receipt and logs are retained under
`/home/burst/panel-reuse.K7U5DR4K` and verified before any source update.

Two test setups are corrected: duplicate checkpoint rejection now supplies
authenticated witnesses after separately checking missing-witness rejection;
dependency lookup checks replay-local certificate reuse and rejects unreadable
selection sources in a new scope. No production checks were relaxed. Remote
input comparison shows only those two test files changed since the failed run.
`burst-panel-followup-20261018` finished successfully. Both corrected tests,
deadline recovery, 18 durable-authority cases and the second-successor crash test
pass. The crash test checks five SIGKILL stages, recovered state/caches and payment
admission. `panel-followup-receipt.json` verifies inputs and logs; successful tests
from the first batch, including historical TCP recovery, were not repeated.
This clears the focused-test prerequisite, not P4 acceptance.

The candidate build finished successfully as `fe3350ce...`; its live result is
recorded above. Neither that build nor its acceptance should be repeated unchanged.
No requirement is newly complete and all public release gates remain closed.

Previous candidate `3f31daa8...` built successfully after the
scheduled-shutdown interruption, using the same source/target cache in 3m58s.
Three-source restart preserves the pinned certified epoch, confirmed heads,
lifecycle/payment state and governance. The fresh observer still FAILS the
unchanged 180-second deadline at sampled epoch `48ce9442...`; crash recovery did
not run. The test is terminal (exit 1), and its candidate processes stopped.
Evidence: `installed-replay-observer*.log` and the result JSON under
`target/validation-evidence/recovery-cut-20261017/`.

The observer used the committed replay handoff 35 times and completed 36
installations. Parent recovery totaled 1,348 ms across 36 imports. Downloads
totaled 1,466 ms across 37 transfers. Installation preparation took 53,028 ms;
preparation plus publication took 60,419 ms. These wall-time measurements
overlap other stages and cannot be added as CPU costs. Remaining work lies
after parent recovery during import and in installation preparation. Inspect
suffix/admission and authenticated history replay before changing production
code. Do not repeat unchanged acceptance. No requirement is newly complete.

Previous candidate `6317387f...` passes the selected stored and
installed recovery, inventory, live publication-crash and historical TCP restart
regressions. Three source nodes preserve combined lifecycle/payment/governance
state after restart. Fresh catch-up still FAILS the unchanged 180-second deadline;
abrupt observer recovery did not run. The test unit is terminal and its temporary
nodes stopped. Evidence and checked receipts are under `history-traversal-*` in
`target/validation-evidence/recovery-cut-20261017/`.

The same live run recorded 1,407 ms across 33 completed downloads and 126,853 ms
between download completion and import completion for 32 imports. Installation
preparation plus publication totaled 42,574 ms. These are wall times, including
scheduling and proof waits, not CPU attribution. Recovery timings overlap them.
Removing a duplicate selection-history traversal did not establish an end-to-end
speedup. The measured next target is reuse of the installed replay parent in import.

The current batch adds that handoff to the existing single-entry projection cache.
It binds the replay to store/network/parameters/base/epoch and retains independent
durable-record checks. The new integration regression requires an actual importer
handoff and separate successor installation. Bounded compilation and the grouped
runtime checks pass, including writer failures, live two-epoch payments/restart,
five-stage SIGKILL recovery and historical TCP restart. The checked input/evidence
receipt and log are `installed-replay-receipt.json` and `installed-replay-tests.log`.
The original candidate build was interrupted by the VM's enabled daily 02:00 UTC
shutdown. Its saved journal records a stop at 02:00:57 while compiling; no
candidate was produced. This is incomplete, not a passing build. Only VM1 was
restarted; VM2/VM3 remain deallocated and the shutdown schedule is unchanged.
The original input-checked receipt is `build-interrupted-receipt.json` on VM1.
The successful resume ran as `burst-installed-replay-resume-20261018`,
invocation `1377d4d55c234d918190496e390ca82a`, using the same source/target and
1 CPU/6 GiB/zero-swap cap. Its log is `build-resume.log` under
`/home/burst/installed-replay.bt3jl4jb/`. No focused regressions were repeated.
The build and subsequent live check are now terminal; the live failure is above.

No requirement is newly complete. C02/C03/C14/P4 and the other recorded release
blockers remain open. Candidates are known-key private fixtures, not public
releases. All release gates remain closed.

Validation workflow correction, 2026-10-18: follow `VALIDATION_WORKFLOW.md`
before the next expensive run. Root `AGENTS.md` now requires stable build paths,
batched edits and an explicit reason to rerun. `tools/validation-reuse.mjs`
checks declared inputs and saved evidence before reuse; its fourteen focused tests
pass under `limited-run.sh`, and CI includes them. Its preflight now refuses
unchanged attempts against a prior receipt. With unchanged inputs, a changed
scope or command now requires an explicit reason and attempt budget. The new
regression covers label-only retries and missing budgets; the bounded tool suite
passed in under a second. This budget is recorded, not enforced across plans.
The writing skill kept
the policy scoped to measured waste and evidence limits. This changes the work
process, not protocol completion or release approval. No additional Rust build
or live acceptance run was started for this policy change.

Catch-up follow-up, 2026-10-18: `burst-observer-profile-20261018` exited 1.
The copied partial observer advanced from height 22 through height 30, but missed
the unchanged 180-second reference deadline. Its crash/restart stage did not run.
Evidence: `observer-profile.log`, `observer-profile-observer-1.log` and
`observer-profile-reference.json` under `target/validation-evidence/recovery-cut-20261017/`.
The logs show repeated metadata recovery between installations. Source inspection
also found that each retained beacon triggered a complete authenticated selection
history scan. The working tree now batches those attachments in one scan with
disk-backed inputs and staged updates. Authentication and the deadline are unchanged.
The new batch test passes valid attachment and duplicate input, and rejects forged,
conflicting, unknown-selection and truncated input without partial publication.
The installed-authority group and mixed historical TCP transfer/restart regression
also pass. Evidence: `beacon-batch-build.log` and `beacon-batch-run.json` in the same
evidence directory. The local compile was stopped after sustained memory-pressure
stalls under the unchanged cap; it supplied no test result. The existing Azure host
ran the passing checks and built daemon `24a9c7c6...` in
`burst-beacon-batch-20261018`, which exited successfully. The source manifest is
`47ffa818...`. Live run `burst-beacon-batch-observer-20261018` now checks combined
source-state restart, an empty-directory observer and abrupt observer recovery
under the unchanged shared 1 CPU/3 GiB limit and 180-second catch-up deadline.
Source restart checks passed, but the fresh observer FAILED the 180-second deadline
after 22 installations, last observed epoch `b5eb9245...`. The crash stage did not
run. All temporary nodes stopped; the partial observer ledger is retained. Evidence:
`beacon-batch-observer.log`, its source/observer logs and reference JSON. The shared
job peaked at 182,190,080 bytes of memory. The batch change does not resolve live
catch-up. Next work must address repeated recovery across installation, checkpoint
workers and peer serving before another acceptance run. C02/C03/C14/P4 remain open.
The candidate remains a known-key private fixture, not a release.

CPU profiling of the copied observer and three sources found BLS arithmetic and
hashing among the largest costs. Source inspection found repeated HTTP-client
construction and pinned-key validation inside selection replay. Replay now uses
an immutable drand policy without transport. The approved chain, proof checks,
selection inputs and signing deadlines are unchanged. Focused drand and selection
tests pass, including valid restored selections and invalid-proof cases. Evidence:
`replay-policy-run.json` and `recovery-cpu-profile-*.log`. Build
`burst-replay-policy-build-20261018` passed, producing daemon `209a387f...` with
source manifest `6278b48f...`. Live run `burst-replay-policy-observer-20261018`
passed combined source-state restart, then FAILED the unchanged fresh-observer
deadline after 26 installations, last observed epoch `71bd4e28...`. The crash
stage did not run. Its logs and reference JSON are saved under
`replay-policy-observer*` in the evidence directory. Temporary nodes are stopped.
The working tree now retains genesis-backed export replay per sync driver and
uses the shared checked advance routine for later selections. Snapshot-backed
exports retain their validation path; handoff clears genesis export progress.
Repeated checks of existing valid beacons no longer append private rows. Local
compile checks passed. Remote job `burst-export-replay-build-20261018` passed
the installed-authority, retained-export, checkpoint-sync and historical TCP
restart tests. These check valid export equivalence, changed-proof rejection,
snapshot handoff and repeated-beacon storage stability. The daemon build passed,
producing `b11c570c...` with source manifest `bc8f3318...`. Evidence:
`export-replay-build.log` and `export-replay-run.json`. Live run
`burst-export-replay-observer-20261018` passed combined source restart, then FAILED
fresh catch-up after 26 installations, last epoch `71bd4e28...`. The unchanged
180-second deadline expired; abrupt recovery did not run. Logs and the reference
are saved under `export-replay-observer*`. All temporary nodes stopped. This
change has not resolved catch-up. A timing diagnostic on a copied partial observer
found a 23,919 ms source export at height 27, longer than the receiver's five-second
peer timeout. Repeated installed recovery also remains visible, but the first
handoff gap is in sync startup: imports receive the authenticated replay while
exports start empty. Exports now take an independent fork of checked genesis-backed
startup replay, with existing observation checks and snapshot handoff preserved.
The extended regression covers byte-identical seeded exports and changed-proof
rejection. Remote tests and build passed in `burst-export-handoff-build-20261018`,
producing daemon `c8058a34...`. Its live run passed combined source restart, then
failed the fresh-observer deadline at sampled epoch `608f1b0a...`. The log has
28 installations, including one during shutdown. Abrupt recovery did not run;
temporary nodes stopped. Evidence: `export-handoff-run.json`, the build log,
`export-handoff-observer*` and
`installed-recovery-timing*`. The diagnostic missed its deadline and is not fresh
acceptance. Its temporary nodes stopped before the new build began.
C02/C03/C14/P4 remain open. No gate is open.

Earlier lifecycle/governance candidate: daemon `9790cad8...` (wire 35),
installed wallet `eae8f097...`.
The saved native appeal now passes without resubmitting its confirmed votes.
Verification and originated TRST are restored, the revoked-state attempt resolves
without resubmission, and a new payment/receive completes. All three representatives
match the final verification state and receiver balance categories. Evidence:
`lifecycle-sequence.log`, `lifecycle-sequence.json`, `lifecycle-sequence-source.log`,
and `sequence-guard-run.json`. Runtime `burst-sequence-lifecycle-20261017` exited
successfully and stopped its temporary nodes. This completes the resumed appeal
scenario only. The resulting three-node orderly restart also passes exact saved
lifecycle state, receiver balances and confirmed payments, plus common-reference
and signing-readiness checks. Evidence: `lifecycle-sequence-restart.log`,
`lifecycle-sequence-restart-source.log` and `lifecycle-sequence-restart-reference.json`.
Fresh uninterrupted lifecycle, crash/topology recovery and remaining P3/P4/P5
requirements still need evidence. Native governance on this candidate FAILED in
`burst-sequence-governance-20261017` while waiting for certified parameter activation
at `native-certified-governance.mjs:59`. The submitted proposal is
`6a60a145f179854148df8d6201eebf872b61f8f6c8dcfaeeac80b99a017bec0f`.
A read-only RPC sample showed certified Exploration with one yea vote. Constitutional
and delegation steps did not run. Evidence: `governance-sequence.log`,
`governance-sequence.json.progress`, and `governance-sequence-node0-restart.log`.
The read-only restart inspection found a 30-second proposal window, 180-second
voting window, 15-second cooldown and 5-second propagation buffer. All three
nodes reported projected Activation while the parameter remained unchanged.
That inspection stopped three seconds after node0 finished startup, so it does
not establish a stalled activation worker. `burst-governance-watch-20261018`
observes the retained proposal for 180 seconds after common-reference readiness,
without submitting transactions. At 21:06:36 UTC on the validation host, all
three RPCs reported `Activated` with `phase_projected: false`. A subsequent
read-only check found `min_verification_age_secs = 2` and matching parameter
hash `fc551af91d3788b73c6b6288a626211eff8d46d6e7c33439ae7b796ca0762cd7`.
This proves recovery of the saved parameter proposal, not timely fresh governance.
The observation job exited successfully and stopped its temporary nodes. Local
evidence is `governance-sequence-watch.log`, its source log and three node restart
logs under the evidence root. The original 240-second native activation failure
remains a failure. Preserve the proposal; do not submit a duplicate on
resume. The resumed native run `burst-governance-resume-sequence-20261018`
passed the retained parameter check and published amendment
`160e98691367dc662b3950525fbd3c2356f587fa159269e400738d91255fff59`,
then failed before the delegation review dialog opened. No ballots were submitted.
Evidence: `governance-resume-sequence.log`, its `.json.progress` file, and profile
`burst-native-wallet-Nmh3AU/0-native-certified-governance.mjs.log` on VM1.
The temporary runtime stopped with exit 1. A separate review-only diagnostic
`burst-delegation-review-20261018` passed review and cancellation against the
saved amendment without changing the account head. It exited 0 and stopped its
nodes. This isolated result does not explain the failed full journey. The next
diagnostic, `burst-delegation-review-switch-20261018`, also passed after reading
the saved proposal as its proposer before switching to the delegator. Both jobs
stopped successfully; neither signed a delegation or published a proposal.
`burst-governance-retry-20261018` stopped before new submissions because the
previous amendment's closure was still projected immediately after startup.
The prerequisite now waits up to 180 seconds for certified state; the original
activation deadline remains unchanged. `burst-governance-closure-retry-20261018`
passed that prerequisite, then failed publication at the pre-sign readiness
guard after a governance transition. No amendment hash or ballots were recorded;
the job stopped with exit 1. The wallet retains the draft and offers Back to
editing. The native test now exercises that existing recovery path once for
this specific pre-sign error: unchanged account head, preserved full article,
readiness restored and fresh review. Other errors and ambiguous sends are not
retried. `burst-governance-draft-recovery-20261018` published amendment
`b77ec2f75ce4bf994ff922107875d06d9f1e8b84144414a1cc497ce7eca0b4e5`,
confirmed delegated/direct ballots, and passed certified adoption with exact
Unicode text and 21 yea votes within the existing phase deadline. It then failed
the final delegation revocation at the same pre-sign synchronization guard.
The job exited 1; full acceptance remains failed. Its progress file preserves
all 21 voters. The shared native review helper now covers one safe pre-sign
recovery for both publication and delegation, checking the unchanged head and
reviewed content before retry. `burst-governance-adopted-resume-20261018` PASSED
from the saved adopted state without recreating proposals or ballots. The native
wallet rechecked exact multilingual text and 21-vote tally, revoked delegation,
and verified the tally was unchanged. The read-only verifier checked expected
parameters, exact constitution, tally and revoked delegation on all three nodes.
The job exited 0 and stopped temporary nodes. Local evidence:
`governance-adopted-resume.json`, `governance-adopted-resume.log` and
`governance-adopted-resume-source.log`. This completes the resumed governance
slice, not fresh uninterrupted acceptance or combined crash/topology recovery.
`burst-combined-restart-20261018` subsequently passed both saved lifecycle/payment
and governance checks on the same three restarted nodes, including common
reference and signing readiness. It exited 0 and stopped temporary nodes.
Evidence: `combined-restart.log`, `combined-restart-source.log` and
`combined-restart-reference.json`. The next job, `burst-combined-observer-20261018`,
added an empty-directory observer under the existing shared 1 CPU/3 GiB limit.
Source-state checks passed, but the observer FAILED the unchanged 180-second
catch-up deadline at epoch `71007ee245e55694913eee47ba1904516a9adb0ea81935a75dceda02878fdef9`.
It continued installing checkpoints and did not claim signing readiness at an
older epoch. The abrupt-stop stage did not run. The job stopped with exit 1;
the partial observer ledger is retained. Local evidence: `combined-observer.log`
and `combined-observer-observer-1.log`. Inspect per-stage recovery costs before
rerunning; authority iteration already has a bounded signature cache, so adding
another signature cache is not justified. C02/C03/C14/P4 remain open.
This is loopback topology; it cannot establish NAT or independent-host recovery.
The failed original timing checks remain failures.
No application fix
is claimed from the isolated checks. Constitution/delegation
acceptance remains open under P3; do not treat this as a new product requirement.

Previous daemon `d69ccd8c...` retained appeal resume FAILED before wallet startup: the three representatives
missed baseline signing-readiness convergence. No new wallet action was submitted.
Nodes1/2 deferred checkpoint installation because local signer journals changed
during recovery. Node0's installed epoch differed from its peers, which repeatedly
could not export requested height 40. Sampled cached epoch authentication on node1
took about 2.4-2.8 seconds, but installation still did not complete. Evidence is in
`lifecycle-enrichment*.log`; investigate journal churn and installation together
before another run. C03/C17/P3/P4 remain open. Temporary test nodes stopped.
A read-only comparison of node1's stopped pre/post-run ledgers found no checkpoint
round entries, unchanged final promises/timestamps/closed epochs, and one changed
signing sequence. Undo records also differed across restart. This does not locate
every write within a preparation interval. The next change therefore preserves
byte-exact guards for all other journals and permits only nondecreasing sequence
counters, retaining their captured high-water marks on disk. Local compilation
passes; checkpoint prepare/commit with advancing counters passed. The first run
stopped on a legacy stress assertion that still expected sequence growth to
invalidate the byte fingerprint. The revised stress test keeps 131,073 signers,
checks monotonic counters separately and preserves changed-promise rejection.
The recheck in `burst-sequence-guard-recheck-20261017` passed authority, journal
and TCP transfer/restart checks. This includes actual checkpoint installation
across counter advancement and the large-journal rejection checks. Its daemon
build completed as `9790cad8...`; the resumed appeal and orderly restart results
are recorded above. See `sequence-guard-run.json`. These checks do not close
the broader C03/C17/P3/P4 requirements.
The preceding `d13b1445...` run reached certified revocation and origin-specific TRST
invalidation. All seven appeal votes confirmed, but restoration missed the
unchanged 180-second deadline. P3 remains failed. Unit
`burst-beacon-lifecycle-resume-20261017` exited 1 and stopped its temporary nodes.
Evidence on VM1 is under `/home/burst/independent-wallet.Q2eenbT7/`:
`lifecycle-beacon.log`, `lifecycle-beacon-node0-restart.log` through node2,
and `profiles/burst-native-wallet-d6hBNL/0-native-certified-lifecycle.mjs.log`.
Node0 rejected cached observations for checkpoint 38 after a late beacon changed
its stored bytes. Two recovery calls then took 96,311 and 92,530 ms to authenticate
the epoch. Publication finished after runner shutdown began. These logs establish
a replay bottleneck, not a successful appeal or proof that it is the only blocker.
The current unqualified edit accepts only authenticated beacon enrichment of an
otherwise byte-identical selection and refreshes recovered beacon receipts.
The corrected regression proves valid reuse without peers and rejects forged
proofs and changed selection fields. Existing corrupt-history checks and the TCP
transfer/restart check also pass. Evidence is the `run.log` in VM1's frozen source
root `/home/burst/beacon-refresh.HkVPOzts`; the daemon build and hashes passed.
The local copy is `beacon-enrichment-build.log`. Runtime unit
`burst-beacon-enrichment-lifecycle-20261017` resumes the saved appeal without
resubmitting confirmed actions, after preserving the three stopped ledgers.
The initial regression failures are retained in `beacon-enrichment-initial-failed.log`.
Next, the retained appeal must complete with exact TRST restoration and payment
checks. No release requirement becomes complete from these supporting checks.

Previous daemon `0b648f63...` has the following accepted evidence.
The installed VM2 wallet completes onboarding, and all three representatives
pass exact consumer-state checks and common-reference convergence within the
existing deadline. Four-node orderly restart also passes. The observer reaches
the pinned epoch, confirmed heads and signing readiness in 74,380 ms after restart.
The three representatives retain the same reference and pass consumer checks.
Evidence: `tip-fairness-onboarding.log`, `tip-fairness-onboarding.json`,
`tip-fairness-crosscheck.log`, `tip-fairness-restart.log`, and `tip-fairness-live/`.
This verifies this candidate's journey and orderly restart, not all C03/C17 or
P2/P4 cases. Broader recovery/topology cases and public identity/platform
qualification remain open. Earlier failures below
remain evidence; this successful run does not establish their exact cause.

P2's second independently installed wallet now passes on VM1 against the same
daemon and wallet hashes. It uses a fresh isolated app profile and verifies backup
restore, paid endorsements, seven reviewed votes, verification, BRN payment,
TRST transfer/receive and rejection of unfunded/disconnected/locked spending.
All three representatives pass exact consumer-state and common-reference checks.
Evidence: `independent-wallet-install.log`, `independent-wallet.log`, and
`independent-wallet-onboarding.json`. Together with VM2's accepted journey, this
completes the two-installation consumer-journey slice. It is not clean-OS or public
network qualification. Current-candidate challenge/appeal acceptance FAILED on
the retained VM1 ledgers: all seven challenge votes confirmed, but revocation
missed its original 180-second outcome deadline. No appeal was submitted. Logs
and the saved challenge context are in `independent-lifecycle/`. Test nodes were
stopped by the runner. Fresh tip observations continued while checkpoint heights
differed; the bottleneck is not established by the available phase logs. C03/C14
checkpoint recovery progress and P3 outcome acceptance remain open. Do not replay
the paid challenge or confirmed votes when investigating the retained state.
Earlier installed-wallet and snapshot rows below used daemon `9f9df270...`.
Evidence root: `target/validation-evidence/recovery-cut-20261017/`.
This is a known-key private fixture and must not be distributed.

The investigated blocker within C03/C14/C17/P2 was interrupted signing readiness during
live checkpoint recovery. Daemon `2dd1a703...` passed full installed onboarding,
all three consumer-state checks and four-node restart. Its first common-reference
check nevertheless missed 180 seconds; later convergence does not replace that
failure. The diagnostic repeat passed but showed an import losing its parent
replay cache after a concurrent record change. Evidence is in
`witness-onboarding-run.json`, `witness-onboarding-crosscheck.log` and `live-trace/`.

Daemon `c272bc24...` retains that parent on failed acceptance. Its authority
regressions passed rejection without durable writes, valid retry and stale-record
checks (`import-retry-tests.log`). Live onboarding then FAILED at the fifth
reviewed verification vote because synchronization readiness was unavailable.
The wallet refused signing correctly. `import-retry-live/` records the failure;
the source peer was replaying a known checkpoint with a beacon while tip handling
shared its blocked loop. The pending refactor selects one owned import future
alongside network input and suppresses new checkpoint downloads until it finishes.
Observation freshness and certificate checks are unchanged. Its library type-check
passed locally. Remote component regressions passed, including active-import tip
service without another download. `sync-service-build.log` records the 36 selected
checks and successful daemon build at VM1 `/home/burst/sync-service.RYI93C92`.
The real-TCP mixed-history transfer, installation and restart regression passed
against those frozen sources (`sync-service-tcp.log`, 1.71 seconds). This tests the
service loop, but does not deliberately delay imports beyond observation freshness.
Installed onboarding on `a610852b...` FAILED on the third reviewed vote with the
same readiness rejection. Initial observer readiness passed in 14,298 ms; wallet
creation/restore, participation and three paid endorsements passed before that
failure. `sync-service-live/` retains the source, observer and wallet logs. The
observer's beacon refresh finished before the failure, so an inline import stall
does not explain this run on its own. The failure's readiness phase was not logged.

Code inspection found a separate C17 fairness defect: one peer's tip request
consumed the whole node's one-second reply allowance. The candidate uses a
per-peer cooldown with at most 256 retained identities/replies per cooldown,
tests independent peers and capacity expiry, and logs readiness transitions.
It does not relax freshness or signing checks. The 37 selected component tests,
real TCP transfer/install/restart check and daemon build passed at
`/home/burst/tip-fairness.JSjoWaXr` (`tip-fairness-build.log`). The live acceptance
above also passes. This defect's role in earlier failures remains unproven.
Deliberately delayed-import service coverage and the broader recovery matrix
remain open.
No release gate is newly signed off.

| Requirement slice | Verified result | Evidence / remaining gap |
| --- | --- | --- |
| C17/C18 signing readiness | Installed wallet rejects unready signing without submission/journal; reviewed payment and receive match all three representatives. | `native-readiness.log`, `native-readiness.json`. |
| C18/P2 onboarding and orderly restart | Two independently installed wallets complete verification and payments on daemon `0b648f63...`; three-node state checks pass for each. VM2's journey also passes four-node orderly restart. | `tip-fairness-onboarding.log`, `tip-fairness-crosscheck.log`, `tip-fairness-restart.log`, `independent-wallet.log`. Restarted observer readiness took 74,380 ms. Broader recovery and distribution qualification remain open. |
| C18/C20/P5 installed Linux apps | Wallet and node Dev journeys pass on VM2 with package/source checks. | `accept-wallet.log`, `accept-node.log`. Not clean-OS or other-platform qualification. |
| C03/P4 snapshot recovery | Export/authentication, altered-archive rejection, restore, SIGKILL, reconnect and new payment pass with lifecycle/governance/consumer state. | `tip-snapshot-result.json`; VM1 `tip-snapshot.af8H9247/snapshot-runtime-1bfAzt`. The unrelated recovered-action branch is not covered. |
| C03/P4 independent snapshot | Empty-directory restore and post-SIGKILL checks pass, including exact lifecycle/governance/consumer state and signing readiness. | `tip-snapshot-peer.log`; VM2 `tip-snapshot-peer.Y4QNLwpT`. This covers the current branch, not every transfer-interruption or topology case. |
| C17/P2 fresh sequential join | Original 180-second run failed; retained progress and clean restart pass. | `resume-diagnostic.log`, `restart-diagnostic.log`. Do not turn resumed success into a fresh-start pass. |

Current work: the installed full onboarding journey passed on VM2
`tip-onboarding-peer.tFv9T7lp`, profile `burst-native-wallet-VUj4ot`. This includes
new wallets/backups, three paid endorsements, seven reviewed votes, verification,
restored signing, BRN payment, TRST transfer/receive and rejected unfunded,
disconnected and locked spending. Local report: `tip-onboarding.json`.
Cross-node convergence missed its initial deadline. On VM1 `tip-onboarding-source.mtq7QRpO`,
the first representative passed but the second failed the 60-second check for
payment `0377609f...`. Nodes 1 and 2 remained at epoch `711d59bd...` with six
votes while node 0 and the observer had verified the subject at `f53445a3...`.
Node 1 later installed that epoch at 15:08:27 UTC. A separate read-only check
passed on all three representatives in `crosscheck-after-catchup.log`; no payment
was resubmitted. This is eventual convergence, not a pass for the original
60-second check. The four-node orderly restart now passes against the saved
`onboarding.json`, including confirmed payments and exact received balance.
The fresh sequential-join requirement remains open. Runtime caps are unchanged.
Original logs and ledger data are retained. Investigate the delay before rerunning
the full wallet journey; existing INFO logs do not establish its cause.
The focused snapshot-to-current probe passes on the same binary in 34,470 ms.
It starts an empty observer from height 33 and recovers through height 35 from
one caught-up peer, without new wallet submissions. Installation/publication
takes 1,360 ms at height 34 and 3,473 ms at height 35. Evidence is
`tip-catchup.log` and `tip-catchup-observer.log`; source logs remain at VM1
`tip-catchup.WDt8t6J4`, observer at VM2 `tip-catchup.c5KUgNvD`.
This does not reproduce the live multi-minute delay. Investigate live checkpoint
propagation next; do not infer that the original delay is fixed or caused by CPU
limits. The local wrapper refused the script check because available memory was
below 4 GiB; remote syntax checks and the capped runtime passed instead.
The three-node catch-up probe also recovers the exact saved consumer state;
see `mesh/run.log` and `mesh/node{0,1,2}.log`. Its assertions run sequentially,
so 141,967 / 43,531 / 198 ms are individual check durations, not three cold-start
latencies. VM1 root is `tip-mesh.cdjMFB8C`; VM2 seed is `tip-mesh.ihGnAtbm`.
Both temporary services are stopped. No new transactions were submitted.
Node 1 rejected an installation attempt because persisted verification state
differed from its applied replay. The next attempt had lost its replay cache and
spent 73,386 ms authenticating the installed epoch, then completed successfully.
This identifies wasted retry work within C02/C03/C14, not the cause of every
earlier delay. Installation now uses the existing progress-retaining recovery
path. Its runtime regression passes failed journal checks followed by successful
engine publication and preserved signer promises. The existing recovery regression
also passes invalid-journal rejection, retained progress and changed-base refusal.
Each executed one test; `retained-retry-tests.log` records the names and results.
Uploaded source hashes match the worktree. The local compile was stopped without
a test result (`retry-local-attempt.json`). VM1 build
`burst-retained-retry-build-20261017` passed at `retained-retry.TmuvZXkX`;
daemon SHA256 is `aeeb5597aeeb909935a9fb35974c20d9acd14ef5d3feccc9e0131418499d5cf8`.
`retained-retry-build.log` and `retained-retry-candidate/` retain the build evidence.
The changed-binary mesh FAILED its unchanged 180-second first-node deadline.
Node 0 remained at epoch `711d59bd...`; height 35 had downloaded at 15:49:27 UTC
but was not installed before cleanup at 15:50:49. Individual historical suffix
proofs repeatedly waited five or ten seconds. Inspect sequential witness recovery
and peer selection before attributing all delay to CPU or the fixed retry cache.
Logs are in `retained-retry-mesh/`. Retained ledgers are VM1 `tip-mesh.m8FEno88`
and VM2 `tip-mesh.5il3s0sV`. Both temporary services are stopped; normal validation
services are untouched. This candidate has component passes, not mesh acceptance
or distribution approval.

The C03/C14 follow-up gives historical witness requests a short-lived preference
for a peer that supplied an authenticated proof. Timeout, expiry or disconnection
returns selection to round-robin; proof acceptance and deadlines are unchanged.
Both `cargo check -p burst-node --lib` and `cargo check -p burst-node --tests`
passed through the desktop resource wrapper. The witness unit and transfer
regressions also passed, including authenticated positive recovery, forged-proof
rejection, provider preference, timeout fallback and cancellation. Uploaded source
hashes match the worktree. `witness-routing-build.log` records the successful VM1
build at `/home/burst/witness-routing.5qDaMf01`. Daemon SHA256 is
`2dd1a7037a54f435beb305d9d394af72ae2b12c14edb7f636d2a76eeb1fefbf7`.

The changed-binary three-node recovery reproduction now passes. All three
180-second waiters start concurrently, removing the extra time previously given
to later nodes. They reached the pinned certified epoch and confirmed account
heads in 102,207 / 126,715 / 134,402 ms. All then passed verified-subject,
selection-context, confirmed-payment and exact received-balance checks. Evidence
is in `witness-routing-mesh/`; retained ledgers are VM1 `tip-mesh.D4jni9iC` and VM2
`tip-mesh.jumj0W7O`. Both temporary services are stopped. This closes this C03/C14
reproduction, not fresh-genesis join, live onboarding propagation, the broader
recovery matrix or public distribution. Next run the installed-wallet journey
against this daemon, including its original cross-node convergence checks.

No whole C01-C23 or P2-P5 row is newly signed off. Remaining release blockers
include independent-wallet qualification, convergence, outstanding recovery/topology cases,
creator-owned network identity, platform qualification and the full plan's
unverified protocol requirements. Detailed historical records follow.

P3/P4/C18 active interrupted-action failure (2026-10-17): on `0d085b35...`,
the installed online-exclusion test retained the deliberately dropped payment
and refused duplicate signing. Its subsequent governance proposal was accepted
but did not confirm within the native client's 30-second deadline. The observer
started at 08:24:57 UTC; the three representatives only completed startup at
08:25:29-30. Peer connectivity was not sufficient test readiness. The retained
proposal `f9057ae304b58576c7d2835d5f4ed67dea0d4244d72804e1ffc0389d8cb87266`
also remained unresolved through the existing rendered recovery journey after
node/app restart. That journey retained the exact hash and blocked duplicate
signing, but failed its 120-second resolution deadline. Neither run passes.
Trace accepted-block persistence, asynchronous rejection and rebroadcast before
changing recovery behavior; do not infer exclusion from a missing block. Preserve
the separate dropped payment and proposal journals. Evidence is in
`target/validation-evidence/export-action-20261017/`; retained ledgers are at
VM1 `/home/burst/export-action-source.vZGHqy6X` and VM2
`/home/burst/export-action-peer.yDs3l6we`. Both test services and the recovery
service stopped. Existing passing snapshot evidence is unchanged. This failure
blocks interrupted-action acceptance within the existing plan.
Diagnostic follow-up: a copy of the observer ledger contains the exact proposal
as account head at block count 122, with confirmation height 121. It survives
restart and the backlog scanner rebuilds an election. Explicitly reconnecting
that observer to the representatives delivers the same block, but representative
admission rejects it as too old under the 60-second timestamp rule. The source
trace records repeated rejection of the exact hash. This rules out missing
persistence and missing retransmission in this reproduction. RPC `accepted`
alone does not prove persistence, since queued responses also use that field;
the ledger read establishes persistence here. The wallet correctly refuses to
discard an unresolved action. Finish certified stale-action settlement/exclusion
without relaxing timestamp validation, inferring exclusion from absence, or
signing a replacement while the original can still win. Evidence is
`diagnostic.log` and `node0-resume.log` in the same evidence directory. The original
failed ledger and journals remain unchanged; diagnostic replay used a copied
ledger and the identical existing signature. No requirement is newly complete.
The worktree now adds an on-demand recovery purpose to authority-only cuts.
Authenticated stale blocks at an unchanged canonical account slot record one
epoch-scoped hint. Hints grant no authority and later hints cannot postpone the
first demand. Full and compact discovery may propose an unchanged electorate
under a separate recovery commitment after a minimum 60-second cut interval.
Existing replay, old-electorate certification, promise fences and installation
remain responsible for closing the epoch and reconciling unconfirmed state.
Ordinary timestamp admission is unchanged. Protocol generation is now 34 so
generation-33 peers cannot silently disagree about the new certified event.
This is an implementation choice for interrupted-action recovery, not a new
whitepaper requirement or a completed capability. Local compilation first found
a metadata API mismatch, now corrected. The next local build was stopped under
memory pressure, without a test result. Frozen input and the bounded VM build are
at `/home/burst/recovery-cut.0Wti39mj`, unit `burst-recovery-cut-build-20261017`.
That build stopped before producing a candidate. The demand and recovery-domain
component tests passed; the certified-vote group had 112 passes, four failures
and three ignored subprocess helpers. `run.log` retains the failures.
One failure selected snapshot-replacement fault mode 7 for a forged-domain test
whose source has no snapshot base. The test now leaves the service unmodified
for that case and still requires rejection of the forged manifest. Three crash
checks expected aggregate TRST/graph keys despite startup's row migration.
They now compare complete decoded holdings and provenance through the storage
loaders, retaining the remaining projection and post-crash admission checks.
The capped `burst-recovery-cut-integration-20261017` job runs these corrected
checks and the new stale-action certification/snapshot regression. Its frozen
inputs and output are `integration-inputs.sha256`, `integration-source/` and
`integration.log` under the same VM directory. These checks passed: nine roster
tests including certified stale-action exclusion and snapshot replay, the
forged-domain regression, and three crash-publication tests. The live-worker
test recovered at committed/economics/authority/complete boundaries and checked
restarted payment admission. Subprocess helper tests remain intentionally ignored.
The same job produced daemon
`cc85e9a2372830075841cbdcd7dd32eb22021119e0a90193d01bb29456de6151`.
On copies of the retained four-node ledgers, the installed wallet resolved the
exact uncertain proposal as excluded. It retained duplicate-signing protection,
archived the signed bytes unchanged and made no process submission or replacement.
The observer's existing verified status, certified context, confirmed payments
and exact received balance also passed. Evidence is in
`target/validation-evidence/recovery-cut-20261017/`, including `live-peer.log`,
`action-recovered.json` and the candidate `SHA256SUMS`. Remote ledgers remain at
VM1 `/home/burst/recovery-live-source.e09rpbuI` and VM2
`/home/burst/recovery-live-peer.NkfF2OUg`. Both temporary services are stopped;
the source was deliberately terminated after the observer finished. Original
failed ledgers remain unchanged. Follow-through on the same candidate also
passed: the installed wallet submitted a separately reviewed proposal after
exclusion, resolved the separate dropped-payment journal as excluded, preserved
its signed bytes and confirmed a newly authorized payment. All three
representatives and the observer confirmed the same proposal and payment before
and after orderly restart. The payment's canonical predecessor names only the
new payment as successor, and its burn delta is exactly 1. The observer's earlier
consumer checks still pass. Evidence includes `followthrough.log`, `restart.log`,
`action-followthrough.json.followthrough` and frozen test hashes in the same local
directory. Representative checks passed in
`burst-recovery-followthrough-check-20261017` and
`burst-recovery-restart-check-20261017`; the tested ledgers remain in the same VM
directories. Temporary nodes are stopped. This closes the retained uncertain
proposal and dropped-payment recovery scenario, including a new authorized
action and orderly restart. It does not close P3/P4/C18 as a whole. Public
distribution and the remaining full-plan gates stay closed.

P4 recovery-cut snapshot acceptance on `cc85e9a2...` also passes. A copy of the
post-recovery representative ledger exported and authenticated epoch
`9d1e29d92d45794cdb204e3b86b5650fa360acfcbf3796e55024b4d2b9e8e18d`.
An altered archive was rejected. A fresh receiver restored it, survived SIGKILL,
reconnected and confirmed a new payment. The recovered proposal and reviewed
payment passed exact successor/amount checks after restore, crash and reconnect,
alongside retained lifecycle, governance and consumer checks. This used two
processes on VM1, not an independent host. Evidence is at
`/home/burst/recovery-snapshot.8bzvWs85/snapshot-runtime-MzWf20`; local
`target/validation-evidence/recovery-cut-20261017/snapshot-run.log` and
`snapshot-result.json` record the outcome. The temporary service exited
successfully. VM2 then restored the same archive into an empty ledger, connected
to VM1 and matched lifecycle, governance, consumer and recovered-action state
before and after SIGKILL. Its two verification rounds passed on the same daemon
hash. Evidence is `/home/burst/recovery-snapshot-peer.XRezVQMp` and local
`target/validation-evidence/recovery-cut-20261017/independent-snapshot.log`;
the remote directory retains input hashes and both startup logs. The receiver
exited successfully and the temporary source was stopped. This closes the
independent-host snapshot/crash scenario for this recovery history, not the
other P2-P5/full-plan requirements. The distributable founder
identity is still unresolved; the user was asked for a public identity or a
local key-generation preference. No private seed was requested.

P5 bundle preparation: `/home/burst/recovery-bundle.sSweVVe0` now contains
the tested `cc85e9a2...` daemon and newly built Linux wallet/node Debian packages.
The protocol source matches the daemon build's frozen hashes. Current app sources
were archived separately; their lockfiles matched the existing dependency trees.
Both app builds passed sequentially under the remote build caps. Whole-bundle
preflight passed source hashes, artifact/metadata checksums, package versions,
architecture and declared runtime dependencies. Evidence is `prepare.log`,
`wallet.log`, `node-gui.log`, `preflight.log`, `candidate/SHA256SUMS` and
`candidate/installation.LvlcJZ1F`; local `bundle-preflight.log` is retained in
`target/validation-evidence/recovery-cut-20261017/`. No new packages were installed
by these steps. Installed-app qualification remains required for the rebuilt
binaries. The known-key fixture label remains unchanged and non-distributable.
The packages subsequently installed on VM1, with the previous package files
retained. Installed wallet hash is
`fb81a78937e3fd13eef5b67f0c4bb97a99f77eb61871753912353774919ca0bd`;
node app hash is
`af592ffde598e47b35f41c581c467e269d669e73d06d216818681fb54da2fd59`.
Both installed Dev suites passed against `cc85e9a2...`. Wallet evidence includes
onboarding/backup, signer binding, reviewed payments, returns, cancellation,
pending pagination and group policies. Node evidence includes page navigation,
telemetry, verification/governance views, explorer/focus, constitution and local
logs. Logs are `accept-install.log`, `accept-wallet.log`, `accept-node.log` and
`installed-after.sha256` in the bundle root and local evidence directory.
Disposable profiles are under `candidate/installation.pH5sNbQW` and
`candidate/installation.5qeUlYmr`. These passes qualify those Dev journeys on
VM1, not independent installation, certified-network journeys on the rebuilt
wallet, all UI states or public distribution. No release gate is opened.
VM2 subsequently passed transfer hash verification, bundle preflight, package
replacement and both installed Dev suites with identical executable hashes.
Its frozen bundle is `/home/burst/recovery-bundle-peer.dHS5UBQB`; installed
wallet/node evidence is under `candidate/installation.O8vgzoaU` and
`candidate/installation.EggL9Dr9`. This is independent-host replacement, not a
clean operating-system install.

P2/C17 readiness blocker on the rebuilt bundle: fresh certified onboarding at
VM2 `/home/burst/rebuilt-onboarding.Pf22ky59` failed its first payment because
the observer advanced epochs between signing and admission. The runner's
`verified_count >= 50` predicate allowed signing during historical catch-up.
Original payment `ce89522c3418f4d765fee73c2d34a9034912b331826fdd5e0f5805aac5d5ec34`
was correctly archived as superseded by confirmed successor
`2208790acac837785c4d7e658282ce3be6890af8034b0afd4529137ded3a4ffd`.
The recovery test expected exclusion and failed that narrower assertion.
A subsequent explicit superseded-recovery check passed the archived bytes and
successor checks, but the newly reviewed payment did not confirm before its
UI deadline while the observer continued installing historical checkpoints.
That payment `3801522b769abb4dafc8c4dfa5b335206834ef1c5872cb911247048650ad0604`
and its attempt marker remain in the original `burst-native-wallet-9P7CY7/data`
profile. Do not resubmit or sign another replacement without resolving it.
Preserve `run.log`, `recover.log`, `settled.log`, the three observer logs and
profiles. Source copies are at VM1 `/home/burst/recovery-live-source.MuWeB1r7`.
The onboarding runners are stopped. Resolve the catch-up/readiness contract before
another onboarding attempt; peer count and verified-wallet count do not prove
current signing readiness. This run does not pass P2 or qualify the rebuilt
wallet's certified-network journey.

Read-only catch-up check: all three source representatives agreed on epoch
`ec93b388caa847bdcecbbcb4d94f3659697645427c3b2e46d3f2e45679a76dea`
and fixture 42's confirmed head
`10dc2cd5653b42bf97815ddd8360a1e5d976a6aa23a15d9e4d39965983e250b5`.
The retained VM2 observer reached both in 111,580 ms, within the unchanged
180-second check, and stopped cleanly. No wallet action was submitted.
`target/validation-evidence/recovery-cut-20261017/pinned-readiness.log` records
the pass and the subsequent missing-block response for `3801522b...`. Absence
does not establish exclusion. The wallet journal remains unresolved. This proves
catch-up to that operator-pinned state, not production readiness or fresh join
from an empty ledger. P2/C17 stays open; the node and wallet still need an honest
catch-up/readiness contract. No requirement is newly complete. The source service
`burst-readiness-source-20261017` remains bounded by its 20-minute runtime cap.
The retained payment's slot has confirmed successor
`0f035252001fcde29823c7ab669036c92cbe4d2f95efea4b0e7c23bb460b18de`.
The installed wallet then passed rendered recovery against the caught-up observer:
it blocked duplicate signing, displayed supersession, archived the exact saved
bytes under that successor, and sent no process request during reconciliation.
This reused the action-recovery suite with explicit Burn/proposal UI distinctions
and an exact expected-successor check. A copied profile was used; the original
journal remains intact. Evidence is `retained-recovery-payment.log` and
`retained-recovery.json` in the local recovery-cut evidence directory, with full
profile/logs under VM2 `profiles/burst-native-wallet-v1lB70`. Two earlier attempts
failed in test setup/assertions: wrong copied-suite directory, then a proposal-only
button assertion applied to a payment. Neither is counted as a pass. The passing
service exited successfully. This closes this retained-payment recovery scenario,
not P2/C17 readiness or the complete onboarding journey. Online readiness work
must preserve decision 31.9(b)'s fully offline signing functionality.

P2 rebuilt-wallet consumer journey now passes from the operator-pinned,
caught-up state on `cc85e9a2...` / wallet `fb81a789...`. The resumed encrypted
wallets completed reviewed funding, receipt, representative choice, verifier
participation, three paid endorsements, seven assigned votes, verification,
restored signing, BRN payment and TRST transfer/receipt. Cancellation remained
unsigned; transfer provenance and exact balances matched. Unfunded, disconnected
and locked spending were rejected. All three representatives passed
`verify-consumer.mjs` against the same report. Local evidence is
`target/validation-evidence/recovery-cut-20261017/pinned-onboarding-ready.log`
and `pinned-onboarding.json`; VM2 profile is `burst-native-wallet-AoZsyC`.
The first runner was stopped before wallet execution because source startup
was incomplete. The successful run required live checks of all three source
RPCs and their pinned epoch before starting the observer. Its service exited
successfully; the source was deliberately stopped afterward. This qualifies
the resumed consumer flow, not new wallet creation in this run, automatic
fresh-node readiness, or public distribution. P2/C17 remains open for the
sync/RPC/wallet readiness contract; no whole C/P requirement is newly complete.

Readiness architecture finding: checkpoint transfer currently returns silence
when a requested next record is unavailable. RPC reports the installed epoch
but no catch-up phase. Neither silence, peer count, a stable timer nor wallet
count proves a peer's latest state. Implement bounded peer-tip discovery and
explicit unknown/catching-up/observed-state reporting, integrated with online
signing and recovery. Peer reports must not authorize installation or confirmation,
permanently block progress, or replace certificate validation. Keep offline
signing from prepared inputs. Verify delayed exports, disconnected/stale peers,
epoch changes during review, restart and normal online/offline positive flows
before closing this blocker. The pinned helper now also rejects zero connected
peers; its focused test passed under the desktop wrapper. This helper remains
test coordination, not the production fix.
The worktree now has bounded tip query/observation messages and the advisory
`checkpoint_sync_status` RPC. Discovery keeps one pending query and one expiring
observation, checks peer/nonce/deadline, rotates peers after silence, and drops
disconnected observations. A peer claim cannot install state or become a
persistent height floor. The RPC distinguishes unknown, pending installation,
peer difference and peer match; a match requires equal epoch and record height.
Published observations expire and cannot survive a local epoch change. Wire
generation is 35; ledger/signature encodings are unchanged. The initial compile
found missing entries in the hand-maintained protobuf model. After correction,
`cargo check -p burst-node --lib` passed under the desktop wrapper. Focused tip
tests did not finish: compilation reached memory-pressure throttling (2.67 GB
peak, repeated `memory.high` events, no OOM kill). The owned validation service
was deliberately stopped. This is not a test pass; move the focused test build
to the existing capped validation host without increasing desktop limits.
Frozen source SHA-256 is
`18d5a5166c24ca0d75de2bceff5054cf5b1ed16d84916c8e3cdf083273393d54`.
The existing VM1 is running `burst-tip-readiness-tests-20261017` from
`/home/burst/tip-readiness.g3gBnpJQ`, limited to one CPU, 6 GB, no swap and
20 minutes. Extraction excludes nested build outputs. `run.log` and
`inputs.sha256` retain the test inputs and result; inspect the service before
restarting any build. No new VM was allocated.
The worktree now carries `online_signing` in `params_hash`, bound to the same
installed epoch. Only Dev permits local mode. The shared wallet parameter check
rejects unknown/differing/catching-up status before online signatures, including
stake-release batches and prepared-work submission. Read-only recovery and
offline signing from prepared inputs remain separate. The 20 signed-client
tests passed under the desktop wrapper, including disconnected offline signing,
exact-byte submission, recovery, batching and rejection without work/submission
when unready. The RPC readiness test passed, and the node library compiles with
the combined changes. These are focused checks, not release acceptance.
`handlers.rs`, `signed_client.rs` and `build-tip-integration.sh` are staged
separately at the VM path above. The integration runner refuses to start until
the frozen discovery-test service has exited successfully; no overlapping build
was started. Installed-app integration and live delayed/restarted-peer acceptance
remain undone. This is
not release-qualified, and the generation-34 consumer pass does not qualify it.
The frozen discovery job has now passed all four focused tests and exited
successfully. Local evidence is `tip-discovery-tests.log` in the recovery-cut
directory. The pinned-reference checker additionally tests online readiness:
it waits for a matching observation and rejects a match reported for an older
pinned epoch. Its reference, online and premature-readiness cases passed under
the desktop wrapper. These checks do not substitute for live discovery.
The independent protobuf-schema test now includes both tip messages. `protoc`
and its two runtime libraries were installed on VM1 in a bounded job; no
existing service needed restarting. `burst-tip-integration-build-20261017` is
running the schema test and private-testnet daemon build with the same one-CPU,
6 GB/no-swap/20-minute cap. Frozen combined inputs are `integration-inputs.sha256`
and `integration-source/`; output is `integration.log` at the same VM path.
The unchanged discovery unit tests are not repeated in this job. No whole
requirement is newly complete and release gates remain closed.
The first combined run failed schema decoding because the fixture hard-coded
wire generation 33. The decoder correctly rejected it. The fixture now reads
the current generation; existing older/newer-generation rejection tests remain
unchanged. Original `integration.log`, inputs and failed schema source are
retained. The corrected independent schema check passed in
`integration-v35.log`. `burst-tip-integration-v35-20261017` is now building the
daemon under the same caps, with output destined for `candidate-v35/`.
The live runners are staged at VM1 `/home/burst/tip-live-source.6XrhutQ5` and
VM2 `/home/burst/tip-live-peer.PzS8cqke`; neither has started. They copy retained
source ledgers, capture agreement across all three representatives, check fresh
observer readiness against pinned epoch/heads, then restart the observer and
verify retained consumer state. The checker logs readiness transitions. Local
syntax checks and its three focused cases passed. Live acceptance and rebuilt
installed-wallet acceptance remain required; no whole requirement is complete.
Generation-35 daemon
`9f9df270b9aadfed61d0c60bfde672b28e672d09cc4b9846197dc4c7ab53a516`
has now built successfully and passed artifact checksums on both hosts. The
source service `burst-tip-live-source-20261017` is running on copied ledgers;
all three representatives agreed on epoch
`0be65f15cc1f50a99b32b87ec9bdbbdcbfb00940057c645b63e4c426e74dae62`,
confirmed consumer heads and matching readiness. `burst-tip-live-peer-20261017`
has started the fresh/restarted observer check on VM2 under a one-CPU,
3 GB/no-swap/600-second cap. Its result is pending in `run.log` at the staged
peer path. Current app sources and the reusable bundle runner are staged at
VM1 `/home/burst/tip-bundle.p2NkbHiu`; app builds have not started. They must not
overlap the live source runtime. This is not installed-wallet acceptance.

P2/C17 generation-35 live result: the fresh observer exceeded the checker's
180-second budget while advancing through certified epochs. The service failed
and shut the daemon down cleanly. The original `run.log` remains at the VM2
peer path; the fresh/restart aggregate is not a pass. The source epoch stayed
at the pinned value. A separate bounded diagnostic resumed the saved ledger,
reached that epoch and its confirmed account heads in 43,299 ms, and passed
`verify-consumer.mjs` for verification, payments, provenance and exact received
balance. A subsequent clean restart passed the same checks in 13,712 ms.
Logs are `resume-diagnostic.log` and `restart-diagnostic.log` in the local
recovery-cut evidence directory. No premature `peer_matches` was observed.
The 180-second budget is an acceptance-runner policy, not a whitepaper deadline.
These results establish interrupted catch-up and restart for this fixture, not
fresh-join performance or installed-wallet enforcement of the signing guard.
Decisions 11.1-11.3 and 26.3 still require authenticated snapshot bootstrap;
successful sequential catch-up does not replace that requirement. Next work is
candidate-specific installed-wallet acceptance and the remaining fresh-join and
snapshot evidence. No whole C/P requirement or public release gate is complete.
The temporary source service was stopped after both diagnostics. The normal
validation node was untouched. Bundle preparation passed source/artifact hashes
at VM1 `/home/burst/tip-bundle.p2NkbHiu`. Wallet packaging is running there as
`burst-tip-bundle-wallet-20261017` with one CPU, 6 GB RAM, no swap and a
20-minute limit. `wallet.log` is the result source; packaging is not acceptance.
The installed test runner now has `--certified-readiness`. It uses a real
unready Test RPC and a separate synchronized Test RPC. It requires the native
signer to reject payment without a process submission or pending journal, then
uses the rendered review, payment and receive flow to check a positive transfer
and exact balance. Both changed JavaScript files pass syntax checks under the
desktop wrapper. This test has not run in the installed app yet; it supplies no
new release evidence until that run succeeds.
Wallet packaging completed successfully and its saved checksum passed on VM1.
The node app is now building as `burst-tip-bundle-node-20261017` under the same
limits, after the wallet job exited. Its output is `node-gui.log` in the bundle
directory. Installation, native readiness acceptance and bundle preflight are
still pending. No desktop build or live source fixture overlaps this job.
Both packages and bundle preflight now pass on VM1. The frozen candidate and
matching source are transferring to VM2 `/home/burst/tip-native-peer.10Tb1iuI`.
The installed readiness test lives separately in `wallet/tests` there, with its
inputs recorded by the runtime runner; it does not replace the frozen source
manifest. VM1 `burst-tip-native-source-20261017` starts three copied ledgers at
`/home/burst/tip-native-source.haIpwnXP`, after packaging finished. Both source
and planned native runtime use one CPU, 3 GB RAM, no swap and a 20-minute cap.
Installation and the readiness journey are still pending. No release gate opens.
The first VM2 preflight refused six missing `installation.rs` files. The archive
command's `installation.*` exclusion matched source basenames as well as old
installation evidence directories. Neither installed binary changed. The six
files were transferred from the frozen source without overwriting existing
files, and the failed log was retained as `accept-install-missing-source.log`.
The corrected install runs as `burst-tip-native-install-fixed-20261017` and must
recheck the original source manifest before package replacement. This was a
transfer error, not a protocol failure or permission to weaken source checks.
Corrected installation passed. Installed wallet SHA-256 is
`eae8f0973939355bd525b750c5808a7059e7c5f235e37908ea9cfc93a11d985c`,
and its bytes match the extracted package. `burst-tip-native-readiness-20261017`
is running the real-node check on VM2. Its report is `readiness.json` and its
log is `run.log` at the peer path. No runtime result is claimed yet.
The installed readiness journey now passes on that wallet hash. The unready
node refused a native payment with the synchronization error, no `process`
submission and no pending journal. A separately reviewed payment through the
synchronized node confirmed, and the rendered receive flow produced exactly
0.001 active TRST. All three source representatives independently confirmed the
payment, identical recipient head and exact balance with no expired, revoked or
pending balance. Evidence is `native-readiness.json` and `native-readiness.log`
in the local recovery-cut directory; the disposable app profile is VM2
`profiles/burst-native-wallet-VLtxj6` under the peer path. This verifies this
signing-readiness/payment path, not all C17/C18 or fresh-node/snapshot bootstrap.
The temporary source nodes are being stopped. Standard installed-wallet
regression is running as `burst-tip-native-wallet-regression-20261017` under
the existing runtime caps. Its result is `accept-wallet.log`; no pass is claimed
until that service finishes successfully. Full onboarding and other release
gates remain open.

Generation-35 installed-wallet regression passed in VM2 `accept-wallet.log`.
The installed journey covers backup/restore, onboarding backup controls, account
binding, payments, review/cancel, returns, scoped automation, group policies and
pending pagination. Evidence is under `candidate/installation.7fIPH5bf` at the
current peer path. This is Dev acceptance, separate from the certified Test
readiness/payment pass above. The node-app journey is running as
`burst-tip-native-node-regression-20261017` on VM2. On VM1,
`burst-tip-snapshot-20261017` reuses the existing snapshot runtime acceptance
against the generation-35 daemon and a copy of the latest source ledger at
`/home/burst/tip-snapshot.af8H9247`. It checks retained lifecycle, governance,
consumer state and resolved actions, altered-archive rejection, crash recovery,
reconnect and a new payment. Both jobs retain the one-CPU, 3 GB, no-swap,
20-minute runtime limits. Their results are pending; no full C/P row is closed.
The installed node-app journey passed in VM2 `accept-node.log`, with evidence
under `candidate/installation.zgzqjEad`. Both installed Dev logs are copied to
the local recovery-cut directory. The snapshot run exported and authenticated
the archive, rejected an altered copy, restored lifecycle/governance/current
consumer state, then failed the optional recovered-action report. That report
belongs to `recovery-live-source.e09rpbuI`, not this ledger branch. Direct lookup
on the original copied source also reports both referenced hashes missing; its
account has zero total BRN burned. The failed run remains at
VM1 `snapshot-runtime-itO1uE` with its inputs and logs. Do not call it a pass.
The runtime checker now verifies supplied reports against the source before
export; its syntax check passed under the desktop wrapper. A corrected run will
use the lifecycle, governance and consumer reports for this branch. Current-
candidate snapshot preservation of the other branch's recovered actions remains
separate and open, not silently dropped from acceptance.
The corrected run is active as `burst-tip-snapshot-current-ledger-20261017`.
Its log is `current-ledger.log` at the same VM1 snapshot root; the original
script, failed log and inputs remain unchanged. The runtime limits are unchanged.

C02/P4 active correction (2026-10-17): the detailed fresh-join run on `b8d3ec76`
stopped during early checkpoint catch-up with `StaleRead: checkpoint inventory
changed during installed recovery`. No wallet was opened. Logs and exact inputs
are in `target/validation-evidence/traced-join-20261017/`. This is a concurrent
recovery failure within the existing join requirement, not new product scope.
The worktree now retries explicit stale-read evidence with fresh authentication,
even if the transaction counters sampled around that attempt match. Unchanged
corruption still fails. A separate idle-installation path now retains its checked
replay cache instead of dropping it when no successor is available. Cache reuse
continues to check current storage; it grants no signing or installation rights.
Bounded node-library checks pass. The idle-cache publication regression,
stale-read classification, all 15 installed-authority tests and publication
checks pass in `burst-idle-recovery-build-20261017` on VM1. They cover positive
late-beacon recovery and authority rotation as well as changed-input rejection.
Frozen inputs and logs are under `/home/burst/idle-recovery.6GKuo2qZ`; the candidate
daemon build passed. Artifact
`083aee8d2ae9e58a1d8d0f4795223f6d4e4a088e78f6bfa4580a8671ff790c32`
matches on both hosts. Fresh joining under `burst-idle-join-wallet-20261017`
failed the original retained-payment deadline. It reached checkpoint 27 and
shut down cleanly; the installed wallet never launched. The trace contains 19
authority-recovery retries as installation advanced, with individual uncached
epoch recovery attempts taking up to 15.2 seconds. The retry prevents the earlier
fatal stale-read exit in this run, but repeatedly reconstructing authority while
catch-up changes its inputs competes with catch-up itself. Next work is to preserve
authenticated recovery progress across those retries or share checked publication
state, without relaxing current-storage and journal checks. Logs, exact input
hashes and the acceptance script are in
`target/validation-evidence/idle-recovery-20261017/`. Both temporary test services
are stopped; normal Dev nodes were untouched. Neither fix closes P4. Existing
consumer/restart evidence below remains valid for its stated artifact and scope.
The follow-up now retains a fork of authenticated replay before checking tails,
installed economics and signer journals. Authority retries reuse that progress
only through the existing record/base revalidation path. Failed metadata checks
still return no authority. The added regression checks that invalid journals
remain rejected across retries, repaired journals recover, and a changed base
cannot use retained progress. It passes with the installed-authority,
publication and staged-storage regressions in `burst-recovery-progress-build-20261017`.
Candidate `a00b04dd1db1bc7f2c28e14ef4f5b17ec711c5e58bfc606cd67770a8cc08529d`
built successfully and matched on both hosts. Its empty-node run still failed
the original retained-payment deadline before the wallet launched. The observer
reached 351 blocks at uptime 117 seconds and 363 at 175 seconds. Checkpoint 27
published at 07:31:26 UTC; no later import appears before the 07:32:25 shutdown.
The current trace does not identify the remaining transfer/scheduling delay.
Inspect that interval with sync diagnostics before another implementation change.
Evidence is in `target/validation-evidence/recovery-progress-20261017/`; frozen
build inputs remain at `/home/burst/recovery-progress.0nydAkSE`.
The capped source service was stopped after the failed observer exited cleanly.
P2/P4 and distribution remain open.
The separate diagnostic resume reached all 372 blocks and confirmed the missing
payment. Heights 28 and 29 recovered their replay inputs in 13 ms and 412 ms;
these are not end-to-end join timings. `diagnostic.log` is retained beside the
failed run. Both diagnostic services stopped. Existing sync logs still do not
explain the cold-run pause, so request scheduling, export duration and download
completion now have debug timing records. No protocol or timeout behavior changed.
The timing-only build passed at `/home/burst/sync-timing.SOXt8yzX`. Artifact
`563b0d2c63e0104484f018f81aca70db01ef9db4982c9ae200a5d6c1c14c24ac`
failed the unchanged fresh-join deadline. Its paired logs identify repeated source
exports: height 18 was rebuilt four times at roughly 6-9 seconds each; height 27
four times at 13-16 seconds each while the observer's height-28 requests timed out
with zero bytes. The export cache stamped its one-second lifetime before the
expensive export. It was already expired on completion, so queued retries rebuilt
the same record and delayed newer requests. Evidence is in
`target/validation-evidence/sync-timing-20261017/`. Temporary services stopped.
The worktree now timestamps a completed export after authentication and digesting.
Proof checks, cache lifetime, transfer limits and acceptance deadlines are unchanged.
A regression covers queued duplicate reuse and subsequent expiry. Strict bounded
node-library Clippy, the sync regressions and candidate build pass.
Candidate `0d085b3588121c6f6d626c456ab53afdcd7b56f879b0985812c5f5dc5efaff33`
now passes empty-ledger joining within the original deadline, followed by the
installed-wallet journey without resuming a failed run. The source exported
checkpoint 27 once, then served 28 and 29. Wallet creation, encrypted restore,
delegation, opt-out/in, three paid endorsements, seven reviewed votes,
verification, restored signing, accrual, payment and TRST transfer/receive pass.
Unfunded, disconnected and locked spending are rejected. Exact certified context,
payments, received balance and provenance match all three representatives and
survive all four nodes restarting. This completes this artifact's combined
fresh-join/consumer/ordinary-restart scenario, not P2/P4 or public distribution.
Evidence is in `target/validation-evidence/export-completion-20261017/`; frozen
source and artifact are at `/home/burst/export-completion.mkrI4n8c`.
Temporary services were stopped after verification. Next: qualify the remaining
recorded recovery and independent-installation cases on this artifact, then P5.
The known-key fixture remains non-distributable; creator custody is unresolved.
The same `0d085b35...` candidate now passes authenticated snapshot restore,
altered-archive rejection, SIGKILL recovery, disconnect/reconnect and a new
confirmed payment with the latest consumer ledger. Exact consumer context,
participation, endorsements, payments and TRST provenance pass after restore,
crash, reconnect and the new payment. Retained lifecycle and governance checks
also pass. `burst-export-snapshot-20261017` exited successfully; its evidence is
at `/home/burst/export-snapshot.XOQ0UmRr/snapshot-runtime-QfuT6W` with input hashes
in the parent directory. This closes the combined-state single-host recovery
scenario for this artifact. The independent VM2 then restored the same archive
into an empty ledger using VM1 as its peer. Lifecycle, governance and the latest
consumer checks pass before and after SIGKILL. The receiver also confirmed the
payment created after snapshot export. The receiver exited successfully. Evidence
is in `target/validation-evidence/export-independent-snapshot-20261017/` and
`/home/burst/export-snapshot-peer.qUiw0gsS`. This closes the independent-host
combined-state snapshot scenario for this artifact, not the remaining P2-P5
cases or the full completion plan.
The five recorded storage lint failures are corrected in this same batch;
bounded strict node-library Clippy passes. This closes those diagnostics, not
C21 artifact qualification or the recovery acceptance gate.

Current C02/P2/P4 blocker (2026-10-17): daemon `69606349` failed the
fresh-genesis joining check against the retained 372-block network. The joining
node reached 315 blocks before the retained-payment deadline expired. The wallet
never launched, so this run created no wallets or signed actions. Evidence is in
`target/validation-evidence/inventory-onboarding-20261017/`.
The first diagnostic resume installed another certified checkpoint before its
planned shutdown; a second three-minute observation reached 363 blocks, including
the challenge/revocation state. The required payment was still absent. This disproves
the initial diagnosis of a permanent stall at 315. Catch-up latency remains an
observed failure, not a pass obtained by extending the acceptance deadline.
Next evidence needed: locate the dominant delay, then repeat fresh-node joining
and the installed-wallet onboarding journey with the original acceptance checks.
Snapshot recovery and Activity-history passes do not cover this path. No release
gate changed.
The source's 30-second profile sampled about 24 CPU-seconds, predominantly in BLS
arithmetic. Its file is `/home/burst/inventory-onboarding.W3slaMVR/source.perf`
on VM1. This does not identify the calling replay/export path; stripped call stacks
do not support a more specific conclusion. Inspect repeated historical proof work
on both sender and receiver before selecting a fix. Diagnostic services stopped
without changing the acceptance deadline or cryptographic checks.

C02/P2/P4 correction under validation: drand BLS verification now has a
process-local, 256-entry success cache. Exact scheme, public-key, signature and
message bytes identify each result. Randomness derivation and contextual checks
still run; changed inputs and evicted entries require verification. Failures are
not cached. The bounded `burst-vrf` drand checks and strict crate Clippy pass,
including warmed-cache mutations, exact-key reuse and eviction. These checks
establish the cache contract, not successful onboarding.
Candidate `b8d3ec761aa191068498c3f32139365604c0473f5b87853a358545999da811b1`
built on VM1 in 4m01s. Its fresh observer reached 362 blocks at uptime 166 seconds,
but the original retained-payment deadline still failed before the wallet opened.
Build, source hashes and failed runtime logs are retained in
`target/validation-evidence/beacon-cache-20261017/`. No requirement became complete.
The remaining replay delay is being measured with existing debug timing logs;
do not extend the acceptance deadline or claim a release pass from this change.
The bounded diagnostic resume subsequently reached all 372 blocks and confirmed
the missing payment under epoch `e77c5fcc`. Height-28 input recovery took 13 ms;
height-29 input recovery took 995 ms. These measurements do not cover transfer,
successor application or the complete cold-join interval. The original failure
remains recorded. The separate installed-wallet journey on the resumed node now
passes with fresh app profiles: wallet creation and encrypted restore, delegation,
participation, three paid endorsements, seven reviewed votes, verification,
restored signing, accrual, payment, TRST transfer/receive and rejected
unfunded/disconnected/locked spending. All three representatives independently
match its certified context, payments, exact received balance and provenance.
The native log, result JSON, executable/input hashes and verifier output are in
the same evidence directory. The initial VM1 verifier invocation failed because
`node` was absent from PATH; the explicit installed Node binary passed. Four-node
orderly restart verification also passes with the same context, confirmations,
balances and provenance. The first representative restart check ran before RPC
startup and failed to connect; the readiness-gated check passed on all three.
This closes this candidate's resumed-node consumer/restart scenario, not P2/P4
or distribution. Fresh-join timing and the remaining recorded release gates stay
open. Temporary test services were stopped; the normal Dev nodes were untouched.

C02/C03/C16 migration preparation (2026-10-17): snapshot selection traversal
now propagates staging errors through one shared fallible visitor. Collect-all,
single-selection and validation paths retain the same authority/receipt checks.
New guards pass for staging failure/retry without ledger writes and fork-local
late beacons, including parent/sibling isolation and child inheritance. The
combined VM run also passes restoration without the source ledger, historical
parent authority, invalid receipt/beacon rejection and unchanged snapshot
encoding. Source hashes and logs are in
`target/validation-evidence/selection-stream-contract-20261017/`.
Compilation took 6m48s; the focused checks took about one second. Batch further
storage edits before another node test compilation. The desktop build was stopped
after sustained soft-memory throttling. The first VM build was stopped before
completion to batch these changes; neither stopped build counts as evidence.
Disk-staged replay now uses a shared private authenticated index for vote-source,
target and lifecycle lookups. Its root binds membership and absence; immutable
rows permit separate fork roots without a history-sized memory index. Read
errors abort replay instead of becoming absent lifecycle entries. The bounded
local node-library check passes in 8.47 seconds. VM unit
`burst-selection-index-20261017` has passed the four index integrity/fork checks
and five certified revote checks. Its mixed-history TCP transfer/restart case
failed the 90-second synchronization deadline. The challenge/outcome suite ended
with 35 passes and three failures; restoration and recovery did not run.
Logs, source hashes and the driver are retained in
`target/validation-evidence/selection-index-20261017/`. Source review found that
the transfer test routed witness packets into the
checkpoint queue instead of the dedicated proof service. The worktree now uses
the connection registry's packet routing, starts proof service before traffic,
and reports worker exits promptly. The runtime agreement fixture now installs
its parent through the existing installer fixture before asking it to sign.
The fixed-clock agreement check now compares the fresh cut against independent
replay, rather than expecting the superseded unsigned proposal. All nodes must
agree on the same checkpoint. These changes await behavioral validation.
The newer worktree moves full replay panels, votes and deadline defaults into
digest-checked variable records with disk-backed location and digest indexes.
Reloading binds each panel to its already-authenticated certificate and context.
It does not deserialize a trusted anchor. Record corruption tests and a positive
memory/disk vote-and-spending dependency comparison are added. The bounded local
library check passes in 5.78 seconds. VM unit `burst-selection-panels-20261017`
finished with passing record/index integrity, memory/disk dependency parity,
revote, snapshot restoration and installed-authority recovery checks. Compilation
took 6m50s. The outcome suite still had three TCP failures; the full log and
source hashes are in `target/validation-evidence/selection-panels-20261017/`.
The transfer fixture still lacked installed parent authority. Its replacement
checks that record receipt leaves balances unchanged, installs the agreed
outcome through `PreparedInstallation`, then continues to the next checkpoint
and repeats catch-up after restart. The runtime fixture also needs the witness
service and packet routing because installed epochs select compact recovery.
The fixed-clock test's reopen assertion still expected the old unsigned cut;
it now compares the recovered manifest against independently replayed agreed
state. Worker exits are reported before the agreement timeout. These test-only
changes were tested in `burst-selection-handoff-20261017`. Fixed-clock agreement,
journal reopen and manifest rejection passed. Transfer reached its first
installed-state comparison, which incorrectly encoded an `Option` on one side.
Runtime recovery rejected a fixture with updated account rows but stale published
verification state. The comparison is corrected, and the signed-prefix fixture
now publishes replay-derived economics, pending transfers and lifecycle state
while retaining its installed authority. Both fixes now pass in the receipt batch
below. Earlier failure logs are
in `target/validation-evidence/selection-handoff-20261017/`.
Receipt-history migration passes the bounded library check in 5.71 seconds
and the VM behavioral batch below.
Snapshot decoding stages each receipt into private indexed records. Canonical
encoding, context validation, historical-parent restoration, continuation checks
and tail dependency lookup read those records fallibly. Staged replay also moves
existing receipts to disk. In this validated batch, forks share immutable records;
the first append or test mutation copies the file without collecting it in memory.
New checks cover original wire/canonical bytes, corruption propagation and
fork-local replacement/appends. VM unit `burst-selection-receipts-20261017`
finished successfully from frozen source at
`/home/burst/selection-receipts.HdTtQaLe`. Compilation took 6m47s. The batch passes
record integrity and independent fork writes, snapshot wire/canonical parity,
historical-parent authentication, missing-source restoration and installed
authority recovery. Mixed-history TCP catch-up installs the agreed outcome,
continues to the next selection, then repeats from a reopened receiver. Runtime
agreement now passes discovery, the six-human non-quorum check, seven-human
decision, late observer catch-up and journal reopen without installing balances.
Logs, source hashes and the driver are in
`target/validation-evidence/selection-receipts-20261017/`.
These are focused integration fixtures, not a rebuilt-daemon qualification or
long-history resource measurement. No whole C02/C03/C16 requirement is signed off.
The subsequent worktree removes that full-file copy. Record forks now share an
append-only file and retain independent location/digest roots, matching the
existing index storage. Appends hold the shared file lock through publication;
failed writes poison the storage instead of exposing a partial row. The bounded
library check passes in 3.41 seconds. The concurrent-fork check now passes for
shared storage, branch-local values and corruption propagation in the source
batch below.
Replay panels and tail admission now consume `SelectionSource`, an ordered,
fallible reader that can return owned authenticated certificates. Tail admission
checks the count and every checkpoint hash before applying blocks. Panel reloads
still bind the certificate hash and vote context to the stored panel. The parity
test now covers owned certificates and read failures during construction and
dependency lookup; both pass in the combined batch below.
Snapshot continuation now uses the same reader for selection, activation,
deadline, outcome, roster and governance candidates. Boundary application no
longer needs a temporary mutable certificate list. Compact selection validates
frozen endorsements and prior attempts through one fallible ancestor traversal.
Runtime recovery passes its existing collection directly instead of allocating
reference vectors at each continuation call. The bounded
`cargo check -p burst-node --tests` passes in 17.54 seconds; this type-checks tests
but does not execute them. Publication, retained-record observation checks and
beacon inventory recovery now use the same fallible source. Beacon inventory
still matches every selection against its contiguous historical authority and
observes each durable beacon key. The latest bounded test-target check passes
in 5.69 seconds. Runtime storage itself still requires migration.
VM unit `burst-selection-source-20261017` finished successfully
from frozen source at `/home/burst/selection-source.UdehcEiE`. It passes shared
record forks, snapshot restoration, installed authority, compact selection,
panel dependency parity, revotes and challenge outcomes, including appeal
restoration, mixed-history TCP catch-up/restart and runtime quorum agreement.
Compilation took 6m53s. Logs, driver and source hashes are retained in
`target/validation-evidence/selection-source-20261017/`.
Edits made after that source freeze remove the live beacon worker's full-list
clone. Its queue owns the recovered cache and reads one certificate at a time;
it retains the latest-lifecycle filter and durable beacon checks. Historical
beacon import now reads its matching certificate without removing an entry from
the recovered history. Cache validation uses the fallible lookup too. These
three newer file versions are outside that behavioral batch. Their bounded
`cargo check -p burst-node --tests` passes in 13.12 seconds. Include live
revote/challenge beacon restart cases in the next behavioral batch. Of the
recorded source hashes, only `installed/recovery.rs` differs in the worktree,
from the documented cache lookup edit; the other two files were not in its
change archive. No updated daemon has been qualified from these changes.
No memory-bound requirement is complete.
The newer worktree replaces the runtime selection vector with `SelectionHistory`.
It stores certificates, frozen receipts and beacons in indexed scratch rows and
owns the existing authority history. Snapshot import stages through its validated
visitor; reads restore each receipt against its historical parent and verify the
certificate against the same authority stream. Ordered reads retain one
certificate at a time. Late beacons update fork-local index roots; tests now check
shared-file corruption propagates without changing durable metadata. Publication,
export, replay, beacon queues and historical import use the fallible source.
The bounded node test-target check passes in 6.03 seconds. Behavioral validation
passed in `burst-selection-history-20261017`, frozen at
`/home/burst/selection-history.r6Pnvk21`. The batch covers historical-parent
restoration, installed and stored recovery, compact selection, revotes, beacon
worker restart and challenge/appeal outcomes. TCP outcome agreement and journal
recovery pass too. The final outcome group passed in 165.84 seconds. Logs, frozen
source hashes and the driver are saved in
`target/validation-evidence/selection-history-20261017/`. This is focused
integration evidence, not an updated daemon qualification or full C02/C03/C16
completion.
Random reads still traverse authority history, and repeated panel lookups can
therefore repeat proof work. Long-history latency/resource qualification remains
open; the disk-backed representation is not a scalability sign-off.
After the history batch was frozen, panel construction was changed to reuse the
certificate hash from its authenticated input stream instead of rereading that
certificate. The existing lifecycle index now stores the prior attempt number
directly, preserving consecutive-attempt checks without another old-certificate
scan. A source-read assertion was added to the dependency-parity test. These
changes in `selections.rs`, `selections/panels.rs` and `tests/certified_votes.rs`
are not in that batch. Their bounded test-target compilation passed in 5.70
seconds; behavioral validation remains pending. The construction-only lifecycle
index is now local to construction, so the finished replay view does not retain
it. The bounded test-target compile check also passes after that lifetime change
in 5.61 seconds. Neither compile check proves behavioral parity.
The subsequent worktree replaces the selection-checkpoint vector with the same
ordered scratch-backed sequence used by receipts. Both share one generic storage
implementation. Snapshot decoding stages hashes; tail replay stages existing
in-memory hashes. Snapshot inventory comparisons still check every ordered hash
against its receipt. New checks cover vector wire-format parity, canonical
snapshot parity, fork-local append and corruption rejection. This change was made
after the block-history batch below was frozen. Its bounded node test-target
compile now passes in 19.79 seconds. Behavioral validation passed in
`burst-checkpoint-inventory-20261017`, frozen at
`/home/burst/checkpoint-inventory.klHWdzzd`. It covers inventory encoding and
fork isolation through restoration, installed/stored recovery, selection,
revotes and challenge/appeal outcomes. Compilation took 6m58s; the final outcome
group passed in 170.52 seconds. Saved source hashes match the current node
source. Evidence is in `target/validation-evidence/checkpoint-inventory-20261017/`.
Daemon build `burst-inventory-daemon-build-20261017` passed in 3m38s in the same
root, using separate `candidate-source` and `candidate` directories. It includes
the current group-directory code and lockfile, which were absent from the test
tree. The build uses the existing private fixture identity and is not for
distribution. Its executable SHA-256 is
`696063494929cf1254d1817767b7cfcf6834897f03e69f08b96a2114acf5b4df`.
`burst-inventory-daemon-runtime-20261017` passed on this executable. The mixed
lifecycle/governance snapshot restores into a fresh receiver, rejects tampering,
survives SIGKILL and reconnects after source loss. A new signed payment confirms
on both nodes with the same pending amount and no unrequested receive. The
exported archive is byte-identical to the earlier height-29 archive
`2bcf74b61b3e36f180b3df440b97093c1065609383e2d42bbb5e349c810b7d17`.
Evidence is in `target/validation-evidence/inventory-daemon-20261017/`.
Independent-host recovery now also passes on VM2 at
`/home/burst/inventory-peer.Uy08H5UJ`. The exact daemon restores the archive into
an empty ledger, connects to VM1, preserves lifecycle/governance state and repeats
those checks after SIGKILL. The unchanged installed wallet executable
`38c004a57244f92a3600738ef7eef01767ffbcf247bcf81703d99ac4610f71d6`
then passes Activity pagination against fresh archival history in both directions,
with no signing or account-state changes. Evidence is in
`target/validation-evidence/inventory-peer-20261017/`. The temporary source and
receiver are stopped; normal development services remain untouched.
These checks do not qualify fresh installed-wallet onboarding/signing on this
daemon, arbitrary history size, upgrades or public distribution. Continue P2-P5
exact-artifact acceptance before another storage optimization.
`StagedBlocks` now stores block lookup entries and insertion order in the existing
authenticated scratch records. The separate input-order vector is removed.
Block bodies retain their exact bytes and digest checks. Hash scans are fallible
through replay, transferred tails and cemented-tail authentication; causal
ordering consumes the fallible stream without an extra collection. Added checks
cover repeated ordered reads and corrupt order/lookup records, including absent
lookups. The bounded node test-target compile passes in 14.69 seconds.
Behavioral validation passed in `burst-staged-block-history-20261017`, frozen
at `/home/burst/staged-block-history.iOleuh6v`. It covers the panel optimizations,
positive memory/disk replay comparison, snapshot recovery and challenge/appeal
outcomes. Mixed-history TCP transfer serves again after restart; seven-human
outcome agreement and journal recovery also pass. Compilation took 6m58s and
the final outcome group took 192.80 seconds. Logs, input hashes and the driver
are saved in `target/validation-evidence/staged-block-history-20261017/`.
Six current source files differ from that frozen batch, all belonging to the
newer inventory migration above. This does not qualify a new daemon artifact.
Causal ordering and some
tail/dependency sets still retain their working sets; resource qualification
remains open.
This does not complete the bounded-memory migration or open public-release gates.

C19 / decision 22.1 (2026-10-17): `burst-group-directory` now validates and
exports a deterministic web/IPFS listing through the existing registry rules.
It reads at most 1 MiB plus the overflow sentinel, refuses existing output files,
and makes no network or trust-policy changes. `directory_publication` runs the
binary, serves its output over HTTP and imports it through `GroupClient`.
It also checks invalid drafts and overwrite refusal. The resource-limited
`cargo test -p burst-groups -- --nocapture` passed those checks and the existing
live-query, outage and trust-graph checks. Operator publication instructions are
in `groups/README.md`. The installed Linux wallet on VM2 now loads the exporter
output in Settings, leaves trust unchanged until selection/save, and pins the
selected endpoint. Loading and saving send no membership queries. The same
disposable Dev journey passes merged-origin payments, any/all policies, live
outage fallback, negative/malformed holds, manual acceptance and cancellation.
Hashes and logs are in `target/validation-evidence/directory-acceptance-20261017/`.
The directory and membership servers were loopback fixtures; no public hosting
or IPFS deployment is claimed. This closes the operator-export/installed-import
check, not every C19 interoperability case. Public-release gates stay closed.
There is no on-chain or centrally approved registry.

Current C02/C21 recovery evidence (2026-10-17): daemon `a41e26c9...`
exports and authenticates the combined onboarding, governance and fraud/appeal
state at checkpoint height 29. An altered archive is rejected. A fresh receiver
restores the saved lifecycle balances and governed state, survives SIGKILL,
reconnects after source loss, and confirms a new payment. This run used two
processes on one host. Logs, input hashes and `result.json` are in
`target/validation-evidence/current-mixed-snapshot-20261017/`.
The same archive also restores on VM2 from an empty ledger with VM1 as its peer.
Before and after SIGKILL, the receiver matches confirmed challenge/appeal/payment
records, verification status, exact recipient balances, governed values,
constitution, tally and revoked delegation. It then shuts down cleanly.
Independent-host evidence is in
`target/validation-evidence/current-independent-snapshot-20261017/`.
This covers the on-demand authenticated recovery path selected in decisions
11.1-11.3. It does not establish lifetime bounds, pruning safety or
latest-checkpoint discovery. Release gates remain closed.

Current C10-C12/P3 evidence (2026-10-17): daemon `a41e26c9...` and the
unchanged installed Linux wallet pass a new parameter proposal/vote/activation,
a multilingual constitutional amendment, 21 delegated/direct ballots, certified
adoption and post-count delegation revocation. The installed journey completed
without a resume. After all four nodes reopened their retained ledgers, the
governed values, full constitution, certified tally and revoked delegation match
the saved report. The earlier onboarding payment/provenance/balance checks also
pass on all four nodes. Evidence and input hashes are in
`target/validation-evidence/current-governance-20261017/`. The reused governance
verifier's source matches this bundle; its hash is recorded in restart inputs.
The first cross-check could not connect because the source unit had reached its
20-minute limit and shut down cleanly. That check remains failed, not a pass.
The explicit restart checks passed under unchanged limits; temporary jobs are
now stopped. This qualifies this current-candidate installed governance journey,
not every governed parameter, concurrency case, platform or full P3/C10-C12.
The lifecycle qualification below covers the next scenario; broader release
requirements remain.

Current C08/C18/P3 lifecycle evidence (2026-10-17): the same daemon and installed
wallet now pass a fresh fraud challenge, continued spending before judgment,
seven reviewed votes, certified revocation and origin-specific TRST invalidation.
Unrelated founder-origin TRST survives and the revoked subject cannot pay.
The appeal names the exact fraud outcome and does not restore rights on filing.
Its first vote was interrupted by an epoch transition; all three representatives
prove that vote excluded with an unchanged predecessor. The retained wallet
journal resolves it without signing, then a fresh review completes the panel.
Certified appeal reversal restores the originated TRST and verification rights;
a new payment and receive bring the recipient's active balance to 3.001 TRST.
After all four nodes restart, both saved challenge/appeal hashes remain confirmed,
exact verification status and recipient balances match, and the earlier governed
parameters, constitution and revoked delegation still match their report.
Evidence, failed first-run log, resume report, drivers and hashes are in
`target/validation-evidence/current-lifecycle-20261017/`. Temporary jobs are
stopped. This qualifies this recovered fraud/appeal journey, not uninterrupted
operation, inactivity challenges, repeated reversals, notifications or full C08.
P2-P5 acceptance remains incomplete. Public-release gates stay closed.

C18 test evidence retention: `wallet/tests/onboarding-prefix.mjs` preserves the
pre-vote representative, participation and three endorsement records for panel
resumes. Missing, truncated, identity-mismatched or duplicate records now fail
instead of silently narrowing the final report. Bounded Node tests and the
onboarding driver's syntax check pass. This is test infrastructure, not a new
production feature or retroactive coverage of older partial reports.

C21/P5 integrated Linux fixture candidate: daemon `d22ab807...`, wallet
`38c004a5...` and dashboard `c045ed0f...` now form one checksummed bundle at VM1
`/home/burst/integrated-candidate.gnYbhnnj/candidate`. The rebuilt daemon and
reused GUI packages pass artifact preflight, installed-payload comparisons,
daemon startup/restart and funding restrictions, and the installed Dev wallet
and dashboard journeys. Wallet checks include backup/restore, signer binding,
payments with review/cancel, returns, automation and groups. Source-manifest
comparison against the worktree differs only in this ledger and the plan.
Evidence, exact hashes and drivers are in
`target/validation-evidence/integrated-candidate-20261016/`. This qualifies those
installed journeys. The same set now also passes package replacement and Dev
wallet/dashboard journeys on VM2. Copies of the retained certified archival and
snapshot ledgers pass independent-host recovery, certified verification/payment
state checks and installed Activity pagination against the archival reference.
After SIGKILL of the copied receiver, the same state assertions pass again.
Evidence is in `target/validation-evidence/integrated-peer-20261016/`. The first
transfer omitted six `installation.rs` source files because an evidence-directory
exclusion was too broad. Preflight rejected it before installation. The repaired
transfer passes the unchanged source manifest; both logs are retained. This does
not repeat new onboarding, governance or challenge submissions on this artifact
set. Their change-impact acceptance, creator identity/custody and handoff remain
open. Original ledgers were not modified. The published-key fixture is not
distributable; gates stay closed.

C21 shutdown follow-up: the same daemon and copied archival ledger stop cleanly
with direct file logging, exit 0 and completed subsystem shutdown. Removing the
launcher's stdout/stderr pipe consumer reproduces exit 101. The syscall trace
shows `EPIPE` on the event write, then on tracing-subscriber's error report; Rust
panics while printing that report to closed stderr. The validation launcher now
uses direct file logging. Evidence is in
`target/validation-evidence/shutdown-logging-20261016/`. This resolves the test
launcher's shutdown failure without changing the release binary. It does not
claim that the daemon tolerates arbitrary loss of both logging streams. The
temporary listener is closed and the normal Dev service remains active.

C10-C12/P3 current-artifact acceptance ran on disposable ledger copies.
VM1 unit `burst-integrated-governance-nodes-20261016` owns the three representative
nodes in `/home/burst/integrated-governance.JDEQyAPz`; VM2 unit
`burst-integrated-governance-wallet-20261016` runs the installed wallet from
`/home/burst/integrated-peer.jEHn6357`. The wallet submitted numeric proposal
`e480d30a...` and its confirmed vote, then timed out waiting for activation. The
representative subsequently reported certified Activated. A diagnostic observer
restart installed epochs `1e888f6a...` at 12:30:09 UTC and `c445da35...` at
12:31:30 UTC. Its later runtime-limit stop does not imply recovery failure;
persisted RPC state was then verified as certified Activated with value 1.
The numeric resume path now recognizes certified activation without repeating
the vote; it still checks the governed value. The original timing check remains
failed. Evidence is in
`target/validation-evidence/integrated-governance-20261016/`. The resumed wallet
submitted amendment `74183af2...` and 21 ballots, including the scoped delegated
ballot. The representative reported certified Activated with 21 yea votes.
The observer missed the unchanged four-minute activation wait again. At 13:00
UTC it had recovered the 21-vote tally but still reported projected Activation.
Preserve the progress record and both ledgers; do not repeat the proposal or
ballots. The next task is observer checkpoint catch-up diagnosis, not another
timeout extension. The diagnostic observer installed the activation epoch at
13:04:11 UTC. Resuming the existing amendment passed the exact multilingual
constitution, certified 21-vote tally and post-count delegation revocation.
The read-only verifier checks the full saved document, parameters, certification
and delegation entry on all three representatives; it passes both before and
after their orderly restart. These results are in `governance-adopted.log`,
`integrated-governance-adopted.json`, `adopted-verification.log` and
`reopen-verification.log` in the same evidence directory. The independent
observer also passes the same state checks after orderly restart; see
`observer-reopen-verification.log`. Its startup spends 52.3 seconds authenticating
the epoch and 8.1 seconds verifying tails/inventory, versus 16 milliseconds
replaying applied economics. The timing failures remain open. Diagnostics show
repeated historical witness recovery with low CPU use, but do not yet isolate
why those proofs are repeatedly fetched. Current-artifact eventual governance
and orderly recovery pass; timely observer convergence does not.
No capability or release gate is signed off.

C03/P3 observer catch-up follow-up: disk-resident witness reads were inserting
proofs into the same bounded cache as peer-recovered proofs. More than 128 local
heads could evict the peer proofs needed by later replay. Local completion now
leaves that cache alone. The cache size, authority-context checks and transport
limits are unchanged. Focused checks pass the eviction regression, authentic
multi-batch recovery, context isolation, bounded eviction, forged-proof rejection,
cancellation and local-to-peer fallback. Evidence and source hashes are in
`target/validation-evidence/witness-cache-20261016/`. The desktop compilation
was stopped at its soft memory limit and is not a pass; the VM check passed.
Daemon `db8b4a71...` builds successfully. An identical-ledger comparison on VM2
passes exact governance state and clean shutdown with both old and new binaries.
Cold startup is 63 seconds before and 64 seconds after; epoch authentication is
50.2 versus 52.3 seconds, and tails/inventory take 8.1 seconds in both runs.
Logs, hashes and the comparison driver are in the same evidence directory.
This fixes one cache defect but does not improve the measured cold recovery or
resolve the live catch-up failure. Do not qualify a new release from this result.

The next change retains peer-validated ancestry bodies in that same bounded cache
and feeds their bytes through the current checkpoint's cursor when disk history
is missing. It never returns old-context proofs as new-context authority. Focused
checks pass reauthentication, different-head fallback and existing witness safety
cases. Daemon `87dceb8a...` recovers the identical saved ledger in 34 seconds,
versus the recorded 63-second baseline. Epoch authentication falls from 50.2 to
24.7 seconds; tails/inventory take 6.1 seconds. The saved constitution, certified
tally, governed values and revoked delegation match, and shutdown is clean.
Evidence is in `target/validation-evidence/witness-bodies-20261016/`, including
matching initial ledger hashes. This verifies cold recovery improvement, not the
previously failing live activation journey. The full installed-wallet journey is
now running with daemon `87dceb8a...` on all three representatives and the VM2
observer. Units were `burst-witness-bodies-governance-nodes-20261016` and
`burst-witness-bodies-governance-wallet-20261016`. The driver preserved the
four-minute phase wait and wrote each submitted hash to
`/home/burst/witness-bodies-comparison.8IxqlAPN/live-governance.json.progress`.
Inspect that progress before any restart; this is a new acceptance round, not a
resubmission of the earlier adopted proposals. The numeric proposal `78eeed33...`
and its vote confirmed, but the observer again missed the activation deadline.
The source installed the ballot epoch at 13:49:12 UTC and activation at 13:49:36.
The wallet timed out at about 13:50:02; cleanup then stopped its observer. The
shutdown warning about the stopped witness service is not evidence of the cause.
The observer reused authenticated epoch recovery in 39 milliseconds, then spent
19.8 seconds verifying tails/inventory before successor preparation. The next
investigation is witness reuse between those stages; cold-start improvement did
not close live convergence. Logs and the progress record are retained in the same
evidence directory. The temporary source cluster is stopped, and both ledgers
remain available for recovery without resubmitting the proposal or vote.

Successor preparation now selects requested witnesses from the recovered applied
and cemented tails before contacting peers. Reuse checks the current history
context, account head and height; missing proofs still use authenticated peer
recovery. Publication guards and tail budgets are unchanged. The initial library
check passed. The focused reuse test compilation reached the desktop soft memory
limit and was stopped, so it is not a passing test. A second bounded library
check including the request-validation guard passed in 3.59 seconds.
No live result is claimed for this change.
Next evidence needed is the reuse/context regression suite on the existing
validation host, then the retained live governance journey without resubmission.
The regression now covers complete reuse without a service, duplicate inputs,
incomplete evidence refusal, different-context refusal and peer completion of
missing proofs. VM1 unit `burst-witness-reuse-20261016` passes these cases and
the existing witness safety suite, plus preparation guard checks. Evidence is in
`target/validation-evidence/witness-reuse-20261016/tests-build-in-progress.log`;
the recorded input hashes match all three current source files. The private Test
daemon build passes; its hash is `401c9ed4...`. Full build output is `run.log` in
the same evidence directory. Its source, script and full remote output are in
`/home/burst/witness-reuse.AIH1gZOO`. It is bounded to
one CPU, 6 GiB, zero swap and 20 minutes. Inspect this unit before restarting work.
SSH access required replacing the stale developer /32 in `AllowSshFromDeveloper`
with the desktop's current /32. No new VM or broader access rule was created.
The live driver ran at VM2
`/home/burst/witness-reuse-live.F3Ea5E6W/run.sh`. It copied the failed
observer ledger and checks proposal `78eeed33...` is certified Activated before
resuming the wallet with that hash. It does not submit the numeric proposal or
vote again. The previous representative logs are preserved as `prior-node*.log`
in VM1's witness-reuse directory. The original four-minute live phase wait stays
unchanged. The initial source launcher was the old fresh-only script; it refused
the existing ledgers before starting nodes. The reviewed resume launcher then
started all three nodes with the matching daemon hash. This late source startup
makes the observer's cold-start duration unsuitable for a timing comparison.
The observer started, then failed the pre-UI verifier's 60-second recovery wait
with proposal `78eeed33...` still projected Activation. It submitted no amendment
or vote. Cleanup stopped the observer cleanly, and the temporary source cluster
was stopped afterward. `live-governance.log` and `live-node.log` in the evidence
directory retain the failure. Cached epoch authentication took 38 ms and tail/
inventory checks 8.25 seconds; successor catch-up still did not complete before
the verifier stopped the run. This is not a passing live convergence result.
The copied ledger remains at VM2 `witness-reuse-live.F3Ea5E6W/ledger`.
P3 convergence remains unresolved. Release gates stay closed.

C03/P3 replay ownership follow-up: `checkpoint_beacon::selections` discarded its
authenticated accepted-tip state after each scan. Any database revision, including
an unrelated write, could trigger another full peer-backed replay. The worker now
retains a `ReplayCache` and advances it through checked successors. Each reuse
still verifies the archive commitment and prior record/beacon observations;
stale observations fall back to authentication. Final tip and publication-input
checks remain in place. The local bounded library check passes in 3.75 seconds.
The new regression covers unrelated-write reuse without new peer requests,
snapshot replacement, corrupt-base refusal and corrupt-record refusal without
metadata mutation. Those cases and the existing base-replacement, beacon and
cache-advance regressions pass on VM1. Evidence is in
`target/validation-evidence/beacon-replay-cache-20261016/tests-build-in-progress.log`;
the input hashes match the four current source files. Unit
`burst-beacon-replay-cache-20261016` built daemon `6a03f493...` successfully from
`/home/burst/beacon-replay-cache.uCu43677`,
under the existing one-CPU/6-GiB/zero-swap/20-minute limits. Inspect the unit before
restarting. Full build evidence is `run.log` in the same evidence directory.
The live driver ran at VM2 `/home/burst/beacon-cache-live.pBCqZ7Sd/run.sh` with
the matching binary and original timeouts. All three source RPCs were ready with
two peers each before observer launch. The observer installed ballot epoch
`4420128b...` in 8.44 seconds, but the 60-second pre-UI recovery check again ended
with projected Activation. No amendment or duplicate vote was submitted. Both
temporary runtime units are stopped; the copied ledger and failed logs remain.
`live-governance.log`, `live-node.log` and input hashes are in the evidence
directory. Installation logging shows no later candidate before shutdown.
Source review found that checkpoint-sync startup discards its authenticated tip
before creating an empty import session. Next work is that ownership handoff,
preserving the import session's commitment and observation checks. This does not close
C02 lifetime state limits or P3 convergence, and no release gate changed.

C03/P3 checkpoint-sync handoff now moves startup's authenticated `ReplayCache`
into its import session instead of discarding it. Successor import still uses
the existing commitment, record-observation and semantic replay checks. The
unused discard-only tip recovery wrapper was removed. The existing no-snapshot
successor/reopen test now exercises this ownership transfer, including single
consumption and duplicate import. The bounded local library check passes in
3.47 seconds. VM1 passes the extended successor/import/install/reopen test,
related authority and corruption checks, sync scheduling/transfer checks, and
accepted-tip cache invalidation. Evidence is
`target/validation-evidence/sync-replay-handoff-20261016/tests-build-in-progress.log`;
the input hashes match all six current source files. Unit
`burst-sync-replay-handoff-20261016` built daemon `7f6edc1e...` successfully from
`/home/burst/sync-replay-handoff.it7ON61b` within the same one-CPU/6-GiB/zero-swap/
20-minute limits. Full build evidence is `run.log`. The unchanged live driver ran
at VM2 `/home/burst/sync-handoff-live.jcKMCAMu/run.sh` with matching binaries and
all three source RPCs ready with two peers each. Ballot installation completed in
7.47 seconds, but the 60-second pre-UI recovery check again failed at projected
Activation. No amendment or duplicate vote was submitted. `live-governance.log`
and `live-node.log` preserve this failure in the evidence directory.
A separate no-submission diagnostic passed on that copied ledger using the
existing `verify-snapshot` recovery verifier. RPC was ready at 16:33:17 UTC;
certified activation and the saved constitution, tally, governed values and
delegation state passed at 16:34:48 UTC. Activation installation took 368 ms.
The 91-second wait precedes installation; recovery progresses, but the failed
60-second check remains failed. `diagnostic.log`, `diagnostic-node.log` and
`diagnostic-inputs.sha256` are retained in the local evidence directory.
VM2 unit `burst-sync-handoff-resumed-20261016` passed the installed-wallet
journey from a copy of this recovered ledger at
`/home/burst/sync-handoff-resumed.5vmbStAu`, without repeating the numeric
proposal or vote. With the unchanged live phase timeout, new amendment
`87dd3b12...` reached certified adoption with 21 votes. Exact multilingual
content and post-count delegation revocation passed; the tally stayed unchanged.
The saved document, tally, governed values and delegation entry also pass on
all three representatives. Evidence is under `resumed/` in the same local
evidence directory. The wallet unit stopped cleanly with exit 0.
VM1 source unit `burst-sync-handoff-governance-nodes-20261016` was then stopped
cleanly with exit 0. Both temporary jobs are inactive; the normal Dev service
remains active. Source logs are retained under `resumed/`. Inspect the handles
before any recovery run and preserve amendment `87dd3b12...` and its ballots.
This qualifies the resumed live governance journey on daemon `7f6edc1e...`,
not the earlier cold catch-up timing checks, all recovery boundaries, or the
whole P3 capability. Release gates stay closed.

The adopted amendment also passes orderly restart on all three representatives
and the independent observer. Observer startup took 65 seconds; the saved-state
assertions passed immediately once RPC opened. `resumed/reopen.log`,
`resumed/reopen-node.log`, input hashes and `resumed/reopen-verification.log`
retain this evidence. This completes that journey's orderly-restart boundary.
The source cluster is now active under `burst-governance-reopen-20261016`.
VM2 unit `burst-current-onboarding-20261016` ran exact-artifact P2 acceptance
from the recovered ledger, using the installed wallet and its existing embedded
onboarding driver. Its output is `sync-handoff-resumed.5vmbStAu/onboarding.log`;
encrypted test profiles remain in that directory's `onboarding-profiles/`.
Creation/restore, representative selection, participation and all three paid
endorsements passed. The observer received the selected committee late and signed
vote `55886b17...` under epoch `60ff9ae3...`, while representatives had installed
`456c2029...`. The source did not have the vote. The driver's assumption that the
pending vote must become confirmed timed out; this run failed.
The driver now supports explicit excluded-vote recovery through the existing UI
reconciliation helper. It requires the exact saved hash, preserves the head during
reconciliation and requires another reviewed action before signing. Syntax check
passes under the desktop wrapper; end-to-end recovery is not yet established.
VM2 unit `burst-current-onboarding-resume-20261016` passed using the retained
profile and committee `d6da39fb...`, without repeating funding or endorsements.
The UI proved exclusion, cleared the saved vote without signing, and required
fresh review. All seven votes confirmed. Verification, restored signing, accrual,
payment, transfer/receive and unfunded/disconnected/locked refusal passed.
The independent observer and all three representatives match the saved certified
context, confirmed payments, exact recipient balance and transfer provenance.
`resumed/onboarding-resume.log`, its public report/input hashes and
`resumed/onboarding-verification.log` retain the result. The original pending-vote
timeout stays failed. This qualifies the interrupted and recovered consumer
journey on daemon `7f6edc1e...`; it does not establish prompt catch-up or every
P2-P5 requirement. The wallet job is inactive with exit 0.
All three representatives and the observer then passed the consumer report after
orderly restart. Observer startup took 74 seconds; once RPC opened, both saved
governance and consumer assertions passed. Evidence is
`resumed/consumer-reopen.log`, `resumed/consumer-reopen-verification.log`, the
observer log and input hashes. Units `burst-consumer-reopen-20261016` and
`burst-consumer-observer-reopen-20261016` are inactive with exit 0. The normal
Dev service remains active. No new VM was created. Release stays closed.

C03/P2-P4 startup replay ownership: startup handed its authenticated recovery to
the installer, but sync and beacon workers each began another full recovery.
Startup now forks independent scratch-backed views for both workers before
opening admission. Their existing commitment, observation and accepted-tip checks
still run; stale views require reauthentication. The installer retains its own
view. This reuses the existing replay-fork mechanism and changes no protocol,
snapshot policy, timeout or trust rule. The bounded local library check passes
in 4.30 seconds. Regressions now cover dropped-original fork reuse, successor
import, changed/corrupt inputs and beacon-worker restart using a startup view.
The VM regression batch passes successor/import/reopen, dropped-original view
reuse, changed-base/corruption checks, and beacon refresh/restart. The beacon
filter also selected a public NTS/future-drand test that remained ignored; it is
not counted as evidence. Logs and matching hashes for all twelve source inputs
are in `target/validation-evidence/startup-replay-views-20261016/`.
VM1 unit `burst-startup-replay-views-20261016` built daemon `38f5503a...` at
`/home/burst/startup-replay-views.kkqGwDPx`, with one CPU, 6 GiB, zero swap and a
20-minute limit and finished with exit 0. The first read-only A/B comparison ran
under `burst-startup-views-comparison-20261016` at VM2
`/home/burst/startup-views-comparison.jwO5Iu00/compare.sh`. It uses byte-identical
copies of `sync-handoff-live.jcKMCAMu/ledger`, old daemon `7f6edc1e...`, the same
peers, the latest saved constitution, and the unchanged 60-second verifier.
VM1 source unit `burst-startup-comparison-sources-20261016` uses the unchanged
old daemon for both comparisons. All three source RPCs were ready before launch.
Both runtime units have one CPU, 3 GiB, zero swap and a 20-minute limit.
Per-version statuses determine the result, not successful script completion.
That driver failed immediately when the newer proposal was absent, so it did not
measure catch-up. `comparison-initial/` retains both failures. The corrected
driver at VM2 `startup-views-timed.dpSI30iv` waits for certified activation and
requires the full saved-state verifier within the same total 60-second window.
It still uses byte-identical inputs and the same source nodes. Old startup took
41 seconds and new startup 40; both missed the 60-second catch-up check and shut
down cleanly. `comparison-timed/` retains logs, input hashes, statuses and driver.
No live performance improvement or release capability is established.
The new daemon log shows another installed-epoch authentication lasting 43.98
seconds after RPC startup, with `had_cache=false`. Source inspection identifies
`checkpoint_service::compact_authority::recover_once` calling the uncached
metadata recovery path at runtime creation and authority rotation. Sync/beacon
startup reuse does not cover that agreement-worker ownership path. The next
C03 catch-up task must include initial authority, rotation and idle refresh,
preserving installed-epoch, branch-advance and metadata checks; do not merely
extend the verifier deadline. Both comparison jobs and the source cluster are
inactive. Full release gates remain closed.

C03 agreement recovery now retains an immutable installed-authority view with its
base commitment. Startup supplies an independent replay copy; height rotation and
idle refresh fork checked views instead of always starting from no history.
All paths still run installed-metadata recovery, branch-advance and unchanged-read
checks. Changed bases/observations can still require full reauthentication. The
existing public entry points share the implementation; no new signing privilege
or snapshot policy was added. A local compile exposed a shadowed variable; after
repair, bounded library checks pass, most recently in 2.35 seconds.
The authority regressions now exercise cached startup, installed-only rotation,
late-beacon idle refresh, dropped-original ownership, corrupted base/epoch refusal
and no metadata writes on recovery. All twelve authority regressions and nine
agreement-service checks pass. The live compact batch is still running; its
appeal-continuation test failed because the shared signing assertion omitted the
authenticated selection-beacon write that activation deliberately persists.
The test correction checks the exact expected beacon bytes and permits only that
key alongside the existing archive writes. All other metadata equality checks
remain. The corrected appeal-continuation test passes in 23.26 seconds after
the VM rebuild; the original failure is retained. Remaining live cases are running.
VM1 unit `burst-agreement-replay-views-20261016` reached its 20-minute limit
during the live compact batch. It did not build a daemon. The original run and
input hashes are retained in
`target/validation-evidence/agreement-replay-views-20261016/original/`.
Child-process failures during shutdown remain unresolved, not passing evidence.
After that service fully exited, `burst-agreement-beacon-check-20261016` started
at `/home/burst/agreement-beacon-check.0m9yRKLh`. It checks the corrected appeal
and unfinished/failed live cases before building. It records exactly which
previous passing cases it skips. The boundary helper has no tests of its own;
the actual governed-activation and deadline callers are selected in a separate
bounded follow-up. A zero-test filter is not acceptance evidence.
The limits remain one CPU, 6 GiB, zero swap and 20 minutes. No desktop compiler
is active. That continuation finished with three recovery tests passing and one
failure, plus two ignored subprocess helpers. The failure reproduced outside
the timeout: the second-successor crash assertion still read the aggregate
`trst_engine_wallets` key, which certified replay deliberately removes when
publishing row-format holdings. The assertion now uses the existing format-aware
loader and compares the complete persisted engine with the running engine.
Production persistence is unchanged. Original and corrected source hashes are
retained. `burst-agreement-final-check-20261016` now runs the corrected crash
case and four actual activation/deadline boundary cases. They pass: the crash
case checks prepared, committed, economics, authority and complete boundaries,
authenticated recovery, persisted holdings and restarted payment admission.
The daemon build passed, producing `7b274bec0bc4ba5422263de1b633c764d6ec808cabfcf5cab47fb2e1d6c98106`.
It used the same limits and source directory after the previous job exited;
`final-check.log` and `final-inputs.sha256` identify this continuation.
No capability became complete; live catch-up, artifact qualification and the
other release blockers remain open.

C03 live comparison of `7b274bec...` still fails the unchanged catch-up gate.
VM2 `agreement-views-timed.OTZw22p8` used the identical input-ledger copy, old
`7f6edc1e...` source peers and saved governance oracle. RPC became ready after
41 seconds; certified activation was still absent at the 60-second catch-up
deadline. The receiver shut down cleanly. Per-version status is 1 even though
the comparison script completed successfully. Logs, driver and input hashes are
in `target/validation-evidence/agreement-replay-views-20261016/comparison/`.
Cached installed-authority authentication appears in the log, but historical
witness recovery still repeats with 6-12-second waits. Source inspection found
that exact-head local reads lack the raw-body cache fallback used by account
prefixes. This is a diagnostic lead, not an established cause of the live waits;
identify request kinds and repeated bodies before another performance change.
The build and comparison units are inactive. The owned source cluster was stopped
cleanly; the normal Dev service was left alone. No VM was created or deleted.

C03 witness trace narrows the remaining delay. Diagnostic daemon `874ed411...`
changes logging only and passes its bounded library check and VM build. The same
copied-ledger comparison still fails: 40-second RPC readiness, 60-second catch-up
failure, clean shutdown. Exact-head batches finish in milliseconds. Three
successive general plans for 31 requested bodies each receive 45 batches with
511 decoded bodies, including account-discovery responses and ancestry. Logs,
comparison status and input hashes are in
`target/validation-evidence/witness-trace-20261016/`. The temporary source cluster
is stopped. The exact-head fallback lead is not the measured bottleneck and was
not changed.

Cache completion now refreshes only requested peer-fetched proofs after the
ancestry walk. Existing matching entries become most recent. Each retained proof
tracks whether it came from a peer, so disk-resident results do not evict peer
proofs. The 128-block/1-MiB limits, cursor authentication, cancellation and transport
budgets are unchanged. A new regression walks 192 blocks for 32 requested bodies
and requires immediate reuse without peers. The bounded library check passes in
2.03 seconds. All 21 witness-transfer tests pass, including that regression,
context isolation, forged-proof refusal, eviction bounds and ancestry reuse.
VM1 unit `burst-witness-requested-cache-20261016` built daemon `fb5c320d...` and
exited successfully. Its unchanged live comparison still fails: 40 seconds to
RPC readiness and no certified activation within the next 60 seconds. Shutdown
is clean. One epoch-installation step falls from about 6.3 seconds to 197 ms;
later 31-body plans still take about 12 seconds. Evidence is in
`target/validation-evidence/witness-requested-cache-20261016/{build,comparison}/`.
The comparison and its owned source cluster are inactive.

Completion-only refresh does not protect requested proofs from a subsequent
ancestry walk. The cache now marks requested peer results and evicts unrequested
ancestry first within the same 128-block/1-MiB bounds. When all entries are
requested, ancillary insertions are skipped; new requested proofs still evict
older requested entries at the bound. Context matching and local-read policy
are unchanged. The regression now places another 140-block ancestry walk between
the first plan and its repeat. The bounded library check passes in 2.17 seconds.
VM1 unit `burst-witness-cache-priority-20261016` passes the 21 witness tests and
builds daemon `db834a86...`. Its live comparison still fails after 41-second RPC
readiness and the unchanged 60-second catch-up window; shutdown is clean.
Evidence is in `target/validation-evidence/witness-cache-priority-20261016/`.
The cache-retention regression passes, but eviction policy alone does not solve
the measured delay.

Logging-only daemon `7a27c936...` identifies a mixed-source request failure.
Repeated 31-body requests have the same authenticated context; their first cache
miss is `02c89f9c...`, which the next log entries read successfully from local
storage. Cache lookup requires every requested proof to be cached, so that
intentional disk-only miss also discards reusable cached results. The request
then repeats peer ancestry walks. The diagnostic comparison still fails at the
same deadline. Evidence is in
`target/validation-evidence/witness-context-trace-20261016/comparison/`.
All completed comparison/source units are stopped.

Request assembly now retains each matching authenticated cached proof and asks
the existing local/peer path for the remainder. Exact-head cursor sets exclude
already satisfied hashes; suffix recovery still requires its complete walk.
Cached bytes count toward retained-byte limits. No context check or capacity was
relaxed. A regression repeats mixed local/cached general and exact requests,
requires complete results without peers, and checks unchanged storage. The
bounded library check passes in 4.07 seconds; test type-checking passes in 28.10
seconds. VM1 unit `burst-witness-partial-cache-20261016` passed all 22 witness
tests and built daemon `2d7e920d...` within the existing host limits. The unchanged
live comparison reached RPC readiness in 41 seconds and recovered certified
amendment activation, the exact constitution and vote tally. Full acceptance
still failed because the later delegation revocation had not arrived. The
receiver was preparing checkpoint 26 when the verifier stopped it; shutdown
was clean. Evidence is in
`target/validation-evidence/witness-partial-cache-20261016/{build,comparison}/`.
This does not prove revocation loss or complete C03/P3. A separate diagnostic
reopens the same receiver ledger and checks saved governance and consumer state
without submitting transactions. Its longer recovery allowance cannot replace
the failed timing check. The proof server's four database loads per retry window
remain unchanged. No capability became complete; catch-up and other recorded
release blockers remain open.

That diagnostic reached RPC readiness in 65 seconds and passed the saved
governance assertions immediately, including the generation-2 revocation.
The consumer verifier still failed at `block_info` for a saved payment after its
60-second wait. The receiver installed another checkpoint and shut down cleanly;
the temporary source cluster is stopped. Diagnostic logs and input hashes are
retained beside the original comparison. Eventual governance recovery does not
qualify timely catch-up or the full consumer journey.

The trace also showed duplicate downloads when account discovery returned the
authenticated head itself. That response now goes directly through the existing
cursor validator. Discovery below the head still starts a full authenticated
walk. A regression requires one response for a genuine head and rejects a
hash-matching response with a corrupted signature. Test type-checking passes in
4.03 seconds after fixing private-helper access in the first test draft.
VM1 unit `burst-witness-discovery-check-20261016` passed all 23 witness tests
in 5.11 seconds after compilation under the unchanged host limits. Evidence is
in `target/validation-evidence/witness-discovery-20261016/`. No new daemon or
live acceptance is claimed. The consumer verifier now preserves the public
block hash and RPC error instead of reporting only `block_info failed`; its
syntax check passes. An uninterrupted read-only probe uses existing daemon
`2d7e920d...` and the same receiver ledger to distinguish slow catch-up from
history lookup failure. Units `burst-continuous-recovery-sources-20261016`
and `burst-continuous-recovery-observer-20261016` own that probe. Its eight-minute
observation window does not replace the failed 60-second timing comparison.
The probe completed successfully: RPC readiness took 76 seconds and exact
consumer state passed at 93 seconds total. Verification context, confirmed
payments, provenance, received balance and saved governance all match. Shutdown
was clean; its owned source cluster is stopped. Logs are retained beside the
earlier failures. This demonstrates recovery on that incrementally advanced
ledger, not convergence from the original older copy or a newly built artifact.
VM1 unit `burst-witness-discovery-build-20261016` built daemon `ddd5b638...` in
3m 23s. Its hash matches the VM2 copy. Units
`burst-discovery-convergence-sources-20261016` and
`burst-discovery-convergence-observer-20261016` now run the tested candidate at
VM2 `/home/burst/discovery-convergence.I7VWIr1E`. The receiver starts from the
original older ledger once, records the unchanged 60-second result, then checks
complete recovery and an orderly reopen without resubmitting any operation.
That run passes exact governance and consumer state after catch-up and orderly
restart. Initial RPC readiness took 40 seconds; complete state checks passed
441 seconds later, 481 seconds total. The unchanged 60-second comparison fails.
Restart took 87 seconds to reach RPC readiness, then both verifiers passed
immediately. Both shutdowns were clean. Evidence is in
`target/validation-evidence/discovery-convergence-20261016/`; temporary units are
inactive. This qualifies the saved-history recovery scenario on `ddd5b638...`,
not the full C03/P4 capability or usable initial-join performance.

The detailed verifier exposed requests for a payment outside the receiver's
then-authenticated prefix. Refusal was correct; the generic corruption wording
does not establish corrupt storage. Checkpoint 27 preparation took 147.8 seconds.
Code inspection found that `snapshot_transfer::Session::import` excludes every
known-record refresh from replay reuse, even a selection refresh with no new
beacon. The next C03 investigation is receipt-based reuse for that no-change
case, with exact certificate/manifest, base commitment and retained-record
checks. New beacons must still attach through their existing authenticated path;
stale evidence must fall back to recovery. This is a candidate cause of repeated
work, not a measured attribution of the whole delay. Release gates remain closed.

Known selections without a supplied beacon now reuse an authenticated receipt
when its base and retained-record observations still match. The refresh checks
the certificate against that receipt's parent and compares the exact checkpoint
and manifest under the storage guard. Missing or stale evidence falls back to
recovery. New-beacon responses keep the existing replay/attachment path. The
regression covers repeated imports after the peer service stops, forged
signatures, changed manifests, unrelated metadata, stale records, changed base
bytes and no storage writes on successful reuse. Library checking passes in
7.15 seconds and test type-checking in 6.50 seconds after correcting two test
storage API calls. The initial VM build was stopped before test execution when
review found that successful import retains the replay parent. The refresh now
uses existing current-tip recovery to authenticate that successor once before
checking the older receipt. Otherwise stale parent observations would prevent
reuse in the live path. The integration regression imports a successor, refreshes
it, then requires another unchanged refresh with no proof service and no storage
changes. Updated test type-checking passes in 17.44 seconds. VM1 unit
`burst-known-selection-refresh-integrated-20261016` runs the authority and
compact-persistence suites at
`/home/burst/known-selection-refresh-integrated.HVhp2Rnl`. The superseded build
log remains at `/home/burst/known-selection-refresh.HP8UkomC` and is not a pass.
The integrated run passes 12 authority tests, including the import/refresh
handoff, but the new receipt test fails while stopping its mock service. The
test retained a registry clone that kept the command channel open. That clone
is now replaced by a new, empty registry after service shutdown. Production
code is unchanged by this fixture correction. Test type-checking passes in
4.20 seconds. The failed log and input hashes are retained in
`target/validation-evidence/known-selection-refresh-20261016/`.
VM1 unit `burst-known-selection-refresh-fixture-20261016` passes the corrected
regression in 0.19 seconds and all eight compact-persistence tests in 1.37
seconds. These include late-beacon attachment, atomic import, first-certificate
preservation, sparse/legacy transfer and refresh scheduling. Evidence is in
`target/validation-evidence/known-selection-refresh-20261016/corrected/`.
The 12 unchanged passing authority tests were not repeated. Unit
`burst-known-refresh-candidate-build-20261016` now builds the tested source at
`/home/burst/known-selection-refresh-fixture.Nqvx2J3t`, after verifying the test
job succeeded and its source hashes still match. The build passes in 3m 23s;
daemon `3e25cb3d...` matches its VM2 copy. The unchanged older-ledger comparison
and orderly restart are running at VM2
`/home/burst/known-refresh-convergence.iGYSkjxy`, owned by units
`burst-known-refresh-convergence-observer-20261016` and
`burst-known-refresh-convergence-sources-20261016`. Driver comparison differs
only in the output directory; the consumer verifier hash is unchanged. Live
comparison has finished. Exact governance and consumer state pass after 342
seconds from RPC readiness, versus 441 seconds in the prior run. RPC startup
takes 40 seconds. After orderly restart, RPC takes 85 seconds and the same
state checks pass in one second. Both shutdowns are clean. The original
60-second catch-up target still fails; the restart check's one-second result
does not include startup. This single comparison is not a performance guarantee.
Evidence is in `target/validation-evidence/known-refresh-convergence-20261016/`.
Both temporary units are inactive. No capability or release gate became complete.

The remaining C03 delay needs attribution before another functional change.
Logging now records replay-cache fallback reasons and known-record import
durations. It changes no acceptance checks. Bounded local library checking passes
in 4.56 seconds. The diagnostic source is not the binary measured above.
VM1 unit `burst-replay-path-trace-build-20261016` is building that logging-only
candidate at `/home/burst/replay-path-trace.d5aAN0q1`, with one CPU, 6 GiB,
zero swap and a 20-minute limit. The build passes in 3m 24s. Daemon
`ae35ee7f...` matches its VM2 copy; the three-file diff contains logging only.
Build evidence is in `target/validation-evidence/replay-path-trace-20261016/`.
The unchanged ledger/state comparison now runs at VM2
`/home/burst/replay-path-convergence.uU2RmaZb`, owned by
`burst-replay-path-convergence-observer-20261016` and
`burst-replay-path-convergence-sources-20261016`. The driver differs only in
output directory and enabled diagnostic logs. No local build remains active.

The live trace reaches checkpoint 27 with cached height 26, rejects changed
record observations, then spends 116.5 seconds authenticating the installed
epoch. Total preparation takes 123 seconds. The next import also rejects its
height-26 cache. Earlier checkpoints reuse cached replay. This identifies an
expensive stale-read fallback, not a general cache failure. Partial evidence is
`replay-path-trace-20261016/catchup-in-progress.log`; the run is still active.
Activation persists a separate beacon copy even when the selection receipt
already contains that authenticated proof. The candidate now permits only this
absent-to-byte-identical copy during cache relocation. Existing proof changes,
new proofs and changed checkpoint bodies still invalidate reuse. The regression
checks identical-copy reuse without extra proof requests or writes, and refusal
of different and subsequently changed proofs. These edits are not yet checked
or included in the running binary. The trace does not name the changed key, so
the candidate's effect on this live delay remains unverified. Future stale-read
errors now include that metadata key. No requirement is signed off.

The diagnostic run finished with exact governance and consumer state passing
346 seconds after RPC readiness, 387 seconds including startup. Restart took
88 seconds to RPC readiness, followed by immediately passing state checks.
Both observer shutdowns were clean. The 60-second catch-up target still fails.
Final logs, driver and input hashes are in the same evidence directory; build
inputs are retained separately as `build-inputs.sha256`. The observer unit is
inactive. The source unit stopped at its runtime limit during the interruption.

On resumption, only the stalled owned file-transfer processes were terminated.
No compiler had started. The transfer succeeded with explicit timeouts. Bounded
local test type-checking passes in 11.11 seconds. VM1 unit
`burst-duplicate-beacon-reuse-20261017` now runs the authority recovery and
compact-storage tests at `/home/burst/duplicate-beacon-reuse.bfd2PdbL`, with one
CPU, 6 GiB, zero swap and a 20-minute limit. The desktop validation job has
finished. The VM build takes 6m 42s. All 14 authority recovery checks pass in
11.17 seconds, including identical-beacon reuse, changed-proof refusal, full
versus cached replay, late-beacon publication/reopen and installed-authority
handoff. Nine compact-storage checks pass in 5.51 seconds; the subprocess helper
is intentionally ignored as a standalone test and exercised by its crash parent.
Evidence and source hashes are in
`target/validation-evidence/duplicate-beacon-reuse-20261017/`.
Unit `burst-duplicate-beacon-candidate-build-20261017` now builds the same source
after checking those hashes and the completed test job. The next unchanged-ledger
comparison is prepared at VM2 `/home/burst/duplicate-beacon-convergence.rsU5YaVV`
on the original older ledger. The candidate build passes in 3m 23s; daemon
`a41e26c9...` matches its VM2 copy. Build evidence is in the same directory.
The first transfer was interrupted by the VMs' scheduled 02:00 UTC shutdown.
Azure reported both hosts deallocated and the activity log recorded deallocation
at 02:00:50 UTC. Only the two existing validation hosts were restarted; their
shutdown schedules were not changed and no resources were created. The partial
binary was not executed. After transfer and hash verification, units
`burst-duplicate-beacon-convergence-sources-20261017` and
`burst-duplicate-beacon-convergence-observer-20261017` started the comparison.
Live improvement and the broader C03 requirements remain unverified; no release
gate opens from the component checks. The 60-second comparison remains a
diagnostic target, not a new whitepaper requirement. Installed-app and recovery
acceptance remain the next integration boundary.

The comparison passes exact governance and consumer state in 248 seconds after
RPC readiness, 288 seconds including startup. The diagnostic baseline took
346 seconds after readiness. Checkpoint 27 preparation falls from 123 seconds
to 40 seconds, and the observed stale-read replay fallback is absent. Restart
takes 87 seconds; exact state checks then pass immediately. Both shutdowns are
clean and both temporary units are inactive. Evidence is in
`target/validation-evidence/duplicate-beacon-convergence-20261017/`.
The 60-second diagnostic target still fails. This is one measured improvement,
not arbitrary-scale recovery qualification or closure of C03/P4.

Next, reuse the installed-wallet lifecycle/history recovery journey with daemon
`a41e26c9...`. Source comparison confirms the GUI trees and protocol outside
`node/` match the existing integrated bundle, so unchanged GUI packages can be
reused. The existing onboarding verifier now also requires the saved challenge
and appeal hashes, when present, to remain confirmed. Its previous checks for
exact lifecycle status and receiver balances remain. This strengthens positive
C08/P3 recovery evidence without resubmitting operations. Assembly is prepared
at VM1 `/home/burst/recovery-qualified.sym31Fen`. The strengthened verifier
passes bounded local type-checking in 3.63 seconds. Unit
`burst-recovery-qualified-assembly-20261017` now assembles the checksummed bundle,
builds that verifier and runs artifact preflight. It does not rebuild the daemon
or GUI packages and is not installed-journey acceptance.

Assembly and preflight now pass on both hosts. The strengthened lifecycle
verifier builds in 22.49 seconds. VM2
`/home/burst/recovery-qualified-peer.jHtAGdk7` uses the same daemon and GUI
packages; preflight also compares the installed wallet executable to its package.
The retained snapshot journey passes saved challenge/appeal confirmation, exact
verification state, payment/receive confirmation and exact recipient balances.
The installed wallet's bounded Activity pages match archival history in both
directions without signing. After deliberate SIGKILL, the same lifecycle and
consumer checks pass again, followed by clean shutdown. Original ledgers are
unchanged. Both temporary units are inactive. Evidence, artifact/source hashes,
verifier hash and native test log are in
`target/validation-evidence/recovery-qualified-20261017/`.
This qualifies this independent-host recovery/history scenario on daemon
`a41e26c9...`; it does not repeat fresh onboarding, challenge/appeal submission
or governance in the installed apps. P2-P5 and full release remain open, including
public network identity/custody. The known-key fixture is not distributable.

Fresh installed-wallet qualification on the same daemon is now running in
`burst-fresh-candidate-sources-20261017` and
`burst-fresh-candidate-onboarding-20261017`. All three representatives and the
independent observer use `a41e26c9...`. VM1 disposable source ledgers are at
`/home/burst/fresh-candidate-network.oH2Oj635`; VM2 logs and isolated wallet
profiles are under `/home/burst/recovery-qualified-peer.jHtAGdk7/fresh-onboarding*`.
The flow checks paid endorsements, selected-verifier votes, verification,
restored signing, payments and receiving through the installed wallet. Each
host's job is limited to one CPU, 3 GiB RAM, zero swap and 20 minutes.
The historical-state preflight passed, but fresh onboarding failed on its first
payment. Signed block `b615882474e15fb75b603ccfa1c5a869466b4525b44b5f08eee88672add3418e`
used epoch `8b387111...`, installed at 02:24:16 UTC. The observer then installed
`e290fdea...` at 02:24:18 and `bfebfbbc...` at 02:24:20. The process response
rejected the payment because its epoch was no longer installed. This reproduces
the C03/C18 signing-versus-epoch-change case during cold catch-up. A historical
payment/status match is not proof that a node is ready for fresh signing.
The signed request and rejection are retained in
`target/validation-evidence/fresh-candidate-onboarding-20261017/`, together with
node/driver logs and input hashes. Encrypted backups and the wallet journal stay
in the isolated remote profile. No replacement was signed. The observer shut
down cleanly and the temporary source service was stopped. Fresh onboarding is
not qualified; original ledgers and public-release gates are unchanged.
Next work must cover sync readiness and recovery of this exact saved action,
without accepting closed epochs or automatically signing replacements.

The first saved-action recovery run reached the source epoch and reconciled the
payment through the installed UI. Its assertion expected epoch exclusion, but
the wallet archived the signed bytes as superseded by confirmed founder Burn
`d04b6527f3a325c0840400fa3c2d39f5a11e087ff57caa46a3e5edb7c07c2fc6`.
Source RPC confirms that transaction has the same account and predecessor as
the failed draft. This is a valid competing-successor resolution, not an
unresolved journal. The run failed before replacement signing; its log remains
`startup-recovery.log` on VM2. The onboarding driver now supports resuming from
the retained receipt, checks its exact hash/account/predecessor against the
confirmed successor, and requires a separate UI review for a new payment.
Bounded local syntax checking passes. Unit
`burst-fresh-startup-resume-20261017` runs that continuation on the unchanged
installed binaries and original isolated profile. No full requirement is closed
by this partial recovery evidence. Production sync readiness remains open.

The continuation verifies the retained competing-successor receipt, confirms a
separately reviewed startup payment, restores/receives, changes representative,
opts a verifier out and in, and confirms all three paid endorsements with exact
permanent burns. It then fails during verifier voting: saved vote
`b256de5e9936934014233fdf97afebd3dd5d2752a1518907d2f908064104a621`
is absent from the observer and all three representatives. Absence alone proves
neither exclusion nor safe resubmission. The original isolated profile and its
journal remain intact; no replacement vote was signed. The observer shut down
and the source service was stopped. Logs, driver and input hashes are retained
in `target/validation-evidence/fresh-candidate-onboarding-20261017/`.
This advances installed C18 recovery evidence but does not close C08/P2.
Next, inspect the saved vote's context and certified outcome before resuming;
production signing readiness and the full positive voting flow remain required.

The saved vote's epoch is now proven closed on all three representatives, with
its canonical predecessor unchanged. Its committee context remains
`de15bf906f174a247e99b224e097299fc32c121664f6cfb0e7ac15299298924d`.
The installed wallet resolves that exact saved vote without signing, then
requires fresh review. All seven votes confirm; the subject becomes verified,
restored signing works, BRN accrues, and payment plus TRST transfer/receive pass.
The flow checks provenance, exact recipient balances and rejection of unfunded,
disconnected and locked spending. It does not repeat funding or endorsements.
All three representatives match the final consumer report. After orderly restart
of all four nodes, the same state/provenance/balance checks pass without signing.
The observer exits cleanly; temporary source units are stopped. Evidence is in
`target/validation-evidence/fresh-candidate-onboarding-20261017/`, including the
failed runs and saved signed vote. Private keys and encrypted backups stay on
the disposable host profile.

This qualifies the recovered installed onboarding scenario on daemon
`a41e26c9...`, not uninterrupted first-attempt operation or all of P2/C18.
The panel-resume report omits earlier participation/endorsement hashes; their
UI assertions remain in the preceding run, not the final cross-node verifier.
No protocol rule was weakened. Pre-activation committee voting is intentional
(`verification_activation::validate_vote`); its unconfirmed tail may require
closed-epoch recovery. Production catch-up guidance and the remaining P2-P5
artifact/capability audit remain open.

C02/C03/C16 recovery beacon buffering: the disk spool passes ordered recovery at
the existing beacon size limit, corruption rejection, atomic persistence,
pruned-record recovery, and selection/deadline/roster agreement and recovery over
TCP. Evidence and matching input hashes are in
`target/validation-evidence/recovery-beacons-20261016/`. The VM unit completed
successfully. The desktop test build was stopped for soft-memory throttling and
is not a pass. Retained selections, replay indexes and full lifetime recovery
acceptance remain open. Gates are unchanged. Next work follows the pilot plan's
artifact qualification priority, not another unprompted storage optimization.

C18 retention reconciliation (2026-10-16): decision 31.4 requires bounded client
history. Source inspection found unbounded completed journal files and attempt
markers in the shared offline storage path, plus accumulated Activity pages in
memory. The implementation and acceptance boundaries are now recorded in
`COMPLETION_PLAN.md`; UI pagination alone cannot close this requirement. No draft,
receipt or marker was deleted, and no retention capability is claimed complete.

The shared archive path now bounds new completed receipts to 100 per account in
an explicit completed-receipt directory. Both online signed submission and native
offline resolution call this path. Bounded desktop checks pass over-limit
retention, independent accounts, unresolved drafts, duplicate paths sharing an
attempt marker, symlink refusal, complete ancestry/content preservation and
maintenance failure after durable resolution. Existing signed-client checks and
strict wallet-core Clippy also pass. Evidence is
`target/validation-evidence/receipt-retention-20261016/`. Only disposable test
receipts were pruned; no existing user profile or VM ledger was cleaned. Legacy
receipt migration and rebuilt installed-app acceptance are still required.
Activity now retains one page of at most 25 entries. The VM browser checks pass
forward/backward navigation through 110 entries, retry after a failed page, and
block-type labels. The narrow-screen screenshot was inspected at 390 pixels;
pagination fits without clipping. Evidence is in
`target/validation-evidence/activity-pages-20261016/`, including the desktop
process-limit failure and separate successful VM log. The bridge is mocked in
these browser checks, so they do not replace the installed-wallet history journey.
C18 is incomplete and release gates remain closed.

Receipt maintenance also retries markers left behind by an interrupted or failed
cleanup. It validates the existing completed receipt and checks current drafts
and their ancestry before removing a marker, then prunes receipts. The regression
leaves a marker through a deliberately unreadable record, removes that obstacle,
and completes a different transaction through the public storage API. Both old
and new markers are then removed; both completed receipts survive. A second
regression keeps a marker referenced by another active draft during later
maintenance. The bounded wallet-core library run passed on 2026-10-16, including
these cases and existing signed-client recovery checks (131 tests, 16.58 seconds).
This is component evidence, not installed-app or process-kill acceptance.
The combined wallet rebuilt and installed on VM1 in `wallet-retention.4XdDn6GT`.
Its actual WebKit history journey failed: the new UI expected decimal cursors,
while the RPC contract uses base64-encoded decimal offsets. The browser fixtures
had repeated that wrong assumption. The failure is retained in
`target/validation-evidence/wallet-retention-native-20261016/first-acceptance.log`.
The UI now encodes requests and decodes responses according to
`rpc/src/pagination.rs`; both pagination fixtures use that contract. Corrected
browser checks and the package build passed in `wallet-cursor.kj87aHYA` on VM1.
Installed Activity now passes against the retained archival ledger in both
directions, with exact entries and unchanged account state. Online payment and
offline existing-account receipt journeys also pass after native app restarts:
accepted responses are lost, duplicate signing remains blocked, reconciliation
does not resubmit, and the completed receipt exactly matches the saved journal.
Resolved drafts and attempt markers are absent. The first added receipt assertion
used the wrong filename; it was corrected to the implemented
`confirmed-by-<successor>-<hash>` path without changing product behavior. Both
failed and corrected runs are retained in
`target/validation-evidence/wallet-retention-native-20261016/`. These are installed
Linux checks on disposable Test/Dev fixtures, not public-network qualification.
Installed retention also passes after recovery followed by 105 additional
confirmed one-raw-unit payments. The check observes exactly 100 completed receipts,
no pending draft or orphan markers, and exact account block-count and burned-BRN
deltas. Every retained receipt belongs to one of the observed payment hashes and
the newest receipt is present after every payment. Evidence is
`retention-overflow.log` and `retention-overflow-inputs.sha256` in the same directory.
This closes the installed normal-path over-limit check for new-format receipts.
The remaining C18 contexts still prevent full sign-off.

Legacy migration now has an explicit account-named-profile API, called at desktop
startup only by the process holding the signing lease. Known receipt names are
checked against signed contents; publication preserves original bytes and mtime
and never overwrites an existing receipt. Active drafts, unknown names and
conflicting records remain untouched. Focused component checks pass migration
past 100 receipts, correct mtime ordering, unchanged active drafts and markers,
retry after destination publication, and conflict refusal. Reopen cleanup after
the last legacy name was removed also passes. The bounded full wallet-core library
check passed (134 tests, 20.41 seconds), as did strict library Clippy.
Installed Linux migration now passes in `wallet-migration.DzVkIrp5` using a fresh
profile seeded from 100 receipts produced by the installed wallet's prior live
Dev payment journey. Both first startup and a second native process preserve
every receipt byte and original mtime, remove legacy names and resolved markers,
and keep the saved account draft, its marker and an unknown file unchanged. Native
commands still report that draft as pending and refuse discard. No existing user
profile was migrated. Source/artifact hashes, driver and output are retained in
`target/validation-evidence/wallet-legacy-migration-20261016/`. This completes the
tested Linux legacy-migration/reopen path, not all C18 contexts or platform gates.

C21 updater stopped-service handling (2026-10-16): the macOS updater checked
whether a job was loaded, then restarted it even if it was stopped. A regression
reproduced the extra `kickstart` call. The updater now requires a successful
status query and a positive PID for the exact job label. This follows the
[documented launchctl list fields](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchctl.1).
Bounded script checks pass running/stopped/loaded-stopped jobs, failed status
queries, restart failure, unchanged binaries, retained backups, invalid downloads
and exclusive update locking. Logs are in
`target/validation-evidence/updater-stopped-job-20261016/`. These use mocked service
managers on Linux, not actual macOS upgrade acceptance. Real supported-platform
upgrade/rollback and signed release qualification remain open; no gate changed.

C21 Linux updater service acceptance passes on VM2 in
`linux-update.uM3JIAqa`. The actual updater replaces daemon `e2791110...` with
`76cd238f...` under a real systemd service, retains byte-identical previous
executables, restarts the running node, and leaves a stopped node stopped.
Certified lifecycle state, confirmed consumer payments and exact recipient
balances pass before replacement, after automatic restart, and after manually
starting the stopped updated service. This uses a separate copy of the retained
Test snapshot ledger and a local download fixture; no existing Dev service or
public release endpoint changed. Evidence and executable/driver hashes are in
`target/validation-evidence/linux-update-20261016/`. The temporary service unit
was removed and both temporary nodes stopped. This proves that same-schema Linux
replacement path, not cross-schema migration, rollback, public-download signing,
macOS/Windows upgrades or C21 as a whole.

Current continuation check, 2026-10-16: no capability completed. Source inspection
confirms the disk-backed authority reader is present; the next selection-history
boundary and its consumers are recorded in the plan. The local validation service
was absent. A focused recovery check was refused by `limited-run.sh` with exit 75
because available memory was below 4 GiB. No compiler started and no test passed.
No limits or release gates changed. Selection-history memory, the remaining
capability rows and P2-P5 still require implementation or qualifying evidence.
Archive import/export, installed-base reads and installation reconciliation now
validate historical selections through one shared visitor. Validation retains
one restored selection at a time instead of collecting the complete list and
discarding it. Single-selection checkpoint export, archived publication and
historical transfer now retain only the requested restored committee while still
validating the complete path. The bounded node library compile check passed.
Focused restoration, compact selection/deadline/roster TCP recovery and archived
committee/beacon reopen checks passed on VM1. Source hashes match the worktree;
logs and commands are in `target/validation-evidence/selection-lookup-20261016/`.
Missing and forged historical authority still reject both requested and absent
lookup heights. This is consumer-level evidence, not a lifetime-memory benchmark
or release-daemon qualification. Runtime recovery still collects selections and
replay rebuilds history-wide panel/index maps; its lifetime bound
is not solved. The visitor checks the complete authority stream, receipt coverage
and ordered beacon consumption. Existing recovery uses the same visitor to collect
its results. Focused restoration and integration checks passed on the existing
VM1 from `/home/burst/selection-validation.YKgf8Yin/protocol`. Evidence and source
hashes are in `target/validation-evidence/selection-stream-20261016/`.
`restoration.log` covers valid recovery after removal of the source ledger,
beacon tampering, missing historical parents and mismatched authority.
`integration.log` covers installation reconciliation, retained signing promises,
archived committee recovery after reopen and live two-epoch installation with
peer proofs and restarted payment admission. Both validation services are
terminal with exit 0. Checks used 1 CPU, 3 GiB and no swap; no desktop compiler
ran. These results verify the archive-validation change, not lifetime memory
bounds or a release capability. The runtime selection collection remains open.

C03/C08/C18 and P3/P4: the interrupted independent-host lifecycle now passes on
installed wallet `65b56753...` and daemon `106c36b9...`. The atomic recovery query
proved payment `5b1398a2...` excluded under installed epoch `2a9d6db0...`.
The resume driver checks the original profile, confirmed challenge, active
challenge, zero votes and unchanged recipient balance before UI reconciliation.
It verifies reconciliation does not sign a replacement, then reviews a new
payment. No challenge or earlier confirmed action was repeated.
The installed UI completed challenge voting, origin-specific revocation, appeal
voting, restored rights and a new payment/receipt. Final recipient balances are
3.001 active TRST and zero expired, revoked or pending. Both independent hosts
match the saved verification state and confirmed payment/receipt after an orderly
restart, with no resubmission. This is not a crash-recovery run.
Evidence and exact input hashes are in
`target/validation-evidence/native-release-20261015/independent-install/lifecycle-payment-resume/`;
host evidence is VM2 `lifecycle-payment-resume.fDMAdmIp`.
This closes the interrupted lifecycle scenario on those binaries. It does not
qualify newer worktree changes or sign off all P3/P4 behavior. Remaining pilot
acceptance and the full capability plan remain open.
Follow the pilot acceptance sequence before further storage optimization.

C10-C12 and P3/P4: the same installed wallet `65b56753...` and daemon
`106c36b9...` passed the independent-host native governance journey in VM2
`independent-governance.deFL3p6y`. The UI proposed and voted for verification
age 2, then observed certified activation. It published the multilingual
constitution amendment, registered a proposal-scoped delegate and submitted
the delegated ballot. Twenty direct native ballots completed the tally of 21.
The complete document matched after certified adoption. UI revocation of the
delegation after counting preserved that tally. Cancelled reviews signed nothing.
The independent source verified the saved report. After orderly restarts, both
hosts again matched the parameters, entire document, tally and revoked delegation;
both also passed the saved lifecycle verification/payment/balance checks.
Evidence is `target/validation-evidence/native-release-20261015/independent-install/governance/`.
The native job finished with exit 0. No new VM or compiler was started. The
two owned Test services `burst-governance-source-20261016` and
`burst-governance-peer-20261016` were stopped before mixed-ledger recovery.
This verifies these candidate journeys, not all governance fault cases, current
worktree artifacts or full pilot acceptance. Public gates remain closed.

C02/C03 and P4: daemon `106c36b9...` passed authenticated snapshot recovery of
the latest independent-host lifecycle and governance ledger. The operator
attested/exported epoch
`24edcd0d719b664fb71ba19677d413f4596b61f6654c29206b0da7452c60f881`;
snapshot SHA-256 is
`92de5a9fd0d6e7ac722b7ccf8894e686a05acbf87017ce5c45459ed38716c582`.
The scenario rejected an altered archive, installed into a fresh ledger,
SIGKILLed and restarted the receiver, disconnected/reconnected its source and
confirmed a new payment. Saved lifecycle balances and complete governance state
matched across installation, crash recovery and reconnect.
Evidence is `target/validation-evidence/native-release-20261015/independent-install/mixed-snapshot/`;
the retained archive is on VM1 under
`/home/burst/mixed-snapshot.rJeQjUHC/snapshot-runtime-KEa5Vg/`.
The bounded job exited 0 and its child nodes stopped. No source ledger, backup
or normal Dev service was removed. This used two loopback nodes on one host;
remaining pilot acceptance and current-source artifact qualification are not
implied. Independent-host recovery is recorded below. Public gates remain closed.

C02/C03 and P4: the same archive and daemon `106c36b9...` also passed recovery
on VM2 in a fresh `/home/burst/crosshost-snapshot.7oH61g1V/ledger`. The original
VM2 ledger and wallet profiles were untouched. The receiver authenticated the
archive's expected epoch, connected to VM1 over private TCP and matched the
saved lifecycle and governance reports. With the source stopped, peer count
reached zero and those state comparisons still passed. After receiver restart
and source reconnection, both reports passed again.
A fresh payment submitted through VM2 confirmed as
`1602a4db4ba7797bc5b7a6bf602802d42a234f770f7e1f6085b1ff76ee92c34c`.
VM1 independently reported that exact block confirmed and the same pending
amount, `1000000000000`, for its recipient. No receive was submitted.
Evidence, input hashes and node logs are in
`target/validation-evidence/native-release-20261015/independent-install/crosshost-snapshot/`.
The owned Test services were stopped after verification; no VM was allocated
and no ledger was deleted. This closes this candidate's independent-host snapshot
scenario, not current-source artifact qualification or all release requirements.
Next reconcile the tested binaries into one installable artifact set and finish
the remaining pilot handoff checks. Do not replace that work with another
unprompted storage optimization. Full capability requirements remain tracked.

C21/P5 packaging now distinguishes creator-key candidates from published-key
fixtures. Build and installer share the identity preflight; fixture builds need
an explicit mode, and fixture installation validation needs an explicit opt-in.
Network, source and toolchain metadata are included in the artifact checksums.
The installer rejects missing metadata coverage and changed metadata before
package installation. `target/validation-evidence/candidate-identity-20261016.log`
records shell syntax checks and focused positive/negative tests, including actual
build/installer preflight invocation. They ran through the unchanged desktop
wrapper without compiling or installing packages. No full artifact build or
release qualification is claimed. The creator-key choice was requested from the
owner; no identity or seed was created, and no private key was sent to Azure.
Whole-artifact preflight now checks both Debian packages before installation,
including checksum membership, package name, architecture, source version and
executable payload. `BURST_CANDIDATE_PHASE=preflight` performs no installation
or application launch. On VM1, `/home/burst/candidate-preflight.40zSl0fk/`
contains the checked scripts and tests. The six-test run passed with no skips
under `burst-candidate-preflight-check`, exit 0, after supplying Node on PATH.
Real dummy Debian packages prove a matching set passes, while an unlisted,
wrong-name or wrong-version second package fails before any installer call.
Desktop syntax and five identity tests passed under `limited-run.sh`; the
Debian-package test is explicitly skipped there because dpkg tools are absent.
This completes that packaging check, not C21/P5 or actual-artifact qualification.

C21/P5 current-source dashboard packaging: VM1 built the staged source archive
`ec1b04746ac42132b8461821ec938bbf580b7991de4d9c8b92942f4d6d9d7eff`
in `/home/burst/release-source.qyOIU2tF`. Debian package SHA-256 is
`0835f1bc34da8850b6126c5db4800e1ed2fe1f620c6a287234898d479fdbbae4`;
its executable is `c045ed0f8a84828f6d8fc27107382e3cee9ef224d36670e6f20976b607ed4965`.
The extracted packaged app passed native RPC contracts, all dashboard pages,
verification fields, explorer dialog/focus restoration, telemetry, constitution
and local-log scope against fixture daemon `106c36b9...` in a new Dev directory.
The first UI attempt failed to create a WebKit thread under the remote 64-task
cap. A fresh run passed with 128 tasks, still one CPU, 3 GiB and no swap.
Both runs are retained in `target/validation-evidence/current-node-package-20261016/`.
Build and journey services are terminal. No desktop limit changed, installed
package was replaced, or existing node data was used. This proves the packaged
dashboard journey on that daemon, not installation or a current-source daemon
and wallet release set. P2-P5 and the full release gates remain open work.

C21/P5 current-source wallet packaging: the same staged source produced package
`644154e749436b685bfca2fc47276fecf72168975c34e3dd6f62b95a76d93b81`
and executable `dea0258650dbefc1ff1541d435ba5ec3e1eac0a5b4dac7cd1de6ddb16c40044a`.
Its embedded packaged UI passed mnemonic and encrypted-backup restoration,
non-overwrite and wrong-password guards, onboarding, signer binding, payment
review/cancel, confirmed payments, returns without BRN refunds, scoped automation,
and group policies including outages and pending-page fairness. This used the
recorded `106c36b9...` daemon on fresh Dev data, not certified Test onboarding.
VM1 evidence is `release-source.qyOIU2tF/wallet-journey/burst-native-wallet-3PMybN`;
logs and input hashes are copied to
`target/validation-evidence/current-wallet-package-20261016/`.
Build and wallet journey services exited successfully; disposable processes
stopped and the existing Dev node remained untouched. No package was installed.
The same-source daemon fixture build completed under
`burst-current-daemon-build-20261016` with one CPU, 3 GiB, no swap and 20 minutes.
Its SHA-256 is `72c9505e31faa516e04555f9c130b6852584afe8713430227e2c49a373ebef62`.
The complete fixture set passed preflight and daemon smoke checks, then both
Debian packages were installed on VM1. Installed binaries matched their payloads.
The installed wallet passed through payment review but its return setup timed
out on Burn `57f623f8...`. Retained-state inspection after restart proved that
Burn excluded under epoch `03b8c6cb...`, with no confirmed successor. The voting
seal remained intact as required by `DECISION_RESOLUTIONS.md`.
The new explicit `--return-recovery` driver resumed the original app profile and
exact signed journal. UI reconciliation signed nothing; a separately reviewed
replacement confirmed, followed by return/reclaim without a BRN refund,
automation and group-policy checks. The failed run remains recorded, not relabelled
as a pass. The installed dashboard then passed against the same current daemon.
Evidence, package hashes and recovery driver hashes are in
`target/validation-evidence/current-installed-set-20261016/`; VM1 source and
retained profiles are under `release-source.qyOIU2tF`. All owned jobs are terminal
and disposable nodes stopped. The original Dev daemon remains running.
This completes current-fixture Linux installation and these Dev journeys.
Certified Test onboarding/lifecycle and cross-host recovery on this exact set,
creator-owned identity and the remaining P2-P5/full-release gates stay open.

Current certified onboarding acceptance completed on VM1 as
`burst-current-certified-20261016`, with evidence in
`release-source.qyOIU2tF/certified-evidence/burst-validation.dg4EfhKo`.
Both Test drivers were built from the frozen source; their hashes are in
`testnet-drivers/inputs.sha256`. The daemon is the exact `72c9505e...` fixture.
`run-validation.sh` now accepts `BURST_NATIVE_EMBEDDED=1` so certified UI
acceptance uses the installed wallet's embedded frontend, not a development
server. The runner hash is retained separately from the frozen build sources.
Three disposable nodes confirmed the parameter proposal and vote, then installed
the governed test-accrual rate. The normally endorsed subject received seven
selected-human votes, accrued BRN and completed a received payment. Those facts
survived a hard restart and fresh observer recovery. The installed embedded
wallet then created/restored encrypted backups, chose a representative, paid
three endorsements, reviewed seven selected-verifier votes, became verified,
restored signing, accrued BRN, paid, transferred and received TRST. Cancellation
did not sign; unfunded, disconnected and locked spending did not succeed.
All three nodes matched the consumer report, provenance and exact recipient
balance. Logs, public reports and driver hashes are retained in
`target/validation-evidence/current-certified-20261016/`.
This completes that same-host current-artifact journey, not cross-host P2/P4.
The same candidate files are staged on VM2 at `current-candidate.I9HAv4UJ` and
all manifest checksums match; no wallet profile or signing material was copied.
The retained ledger is now continuing challenge/appeal acceptance under
`burst-current-certified-lifecycle-20261016`. Original governance, onboarding
and payments are not resubmitted. Exact-artifact lifecycle and cross-host
recovery, creator identity, remaining P2-P5 and full-release requirements remain.

C02/C03 current-artifact restart blocker: the lifecycle's challenge and
`9d8fdf7a...` payment confirmed, but Receive `e6a06d98...` crossed a certified
epoch boundary. A read-only RPC check after restart proved exclusion under
`8b626625...` and preserved the original pending claim. The existing receipt
resume path was selected without repeating the challenge or payment. Its next
restart failed with `snapshot omits a locally observed checkpoint decision`.
Node0's ledger and all signer journals remain intact. Evidence is retained in
`target/validation-evidence/current-lifecycle-receipt-20261016/`.
Startup journal validation used the snapshot-import rule even for a validated
tip decision whose chain record was not yet persisted. The proposed correction
separates recovery from import: recovery retains that terminal decision for the
existing runtime replay path; import still rejects omissions, and both reject
conflicting or uncovered journals. No economics, signatures or fences are
rewritten by the check. Focused verification passed in VM1
`journal-recovery.bpjeesNL`: authenticated tip decisions survive startup
validation without journal changes; snapshot import still rejects the missing
decision; uncovered round keys and malformed authority paths still reject.
The initial 3 GiB remote job was OOM-killed while compiling and ran no tests.
The same check passed with a 6 GiB cap on the existing 8 GiB VM, one CPU and no
swap. No desktop limit changed. Both logs and exact changed-source hashes are
in `target/validation-evidence/journal-recovery-20261016/`.
The corrected daemon built under a 3 GiB cap as
`53f82698f6013004168a2a2db9cd1fe7dcd9f471c004791e6da5a27572ba7d4f`.
`burst-journal-retained-ledger-20261016` then exited successfully. It recovered
the actual failed ledger without clearing journals, reconciled the original
excluded receipt, completed the saved challenge and all seven votes, and
invalidated only subject-origin TRST. Seven appeal votes restored originated
TRST and wallet rights. A new payment and receipt confirmed. Three nodes matched
the final lifecycle state after a hard restart; a fresh observer recovered it
too. No original challenge or payment was resubmitted. The original failed
node0 ledger is preserved at VM1 `journal-recovery.bpjeesNL/failed-node0`.
The retained-ledger log, exact artifact/driver hashes and final public report
are in `target/validation-evidence/journal-recovery-20261016/`.
This verifies this restart defect and lifecycle scenario end to end, not all
C02/C03 requirements. The old `72c9505e...` candidate, including the copy staged
on VM2, remains unqualified. Assemble the corrected source/artifact set before
cross-host acceptance; remaining snapshot, release and scalability gates stay shut.

Corrected artifact assembly is complete at VM1 `corrected-candidate.Au5xVxrd`.
It combines the frozen source with the three tested journal-recovery changes,
daemon `53f82698...`, and the unchanged wallet/dashboard packages. Its source
manifest, artifact checksums and whole-set preflight pass. VM2 received the
bundle at `corrected-install.PKdDNGF8`, verified it and installed both packages.
The first dashboard attempt could not find WebDriver; the explicit existing
driver path fixed the job environment, and installed dashboard acceptance passed.
No rebuild or package reinstallation was needed for that retry.
P2/P4 cross-host consumer and orderly recovery scenarios now pass with the
corrected daemon. VM2 joined from `fresh-ledger` over Azure private TCP and
matched the saved lifecycle state and exact balances. Its installed wallet
completed encrypted backup restore, representative changes, verifier opt-out/in,
three paid endorsements, seven selected-verifier votes, verification, accrual,
payment and TRST transfer/receipt. Cancelled reviews signed nothing; unfunded,
disconnected and locked spending checks passed. VM1 independently matched the
consumer report, including certified context, confirmed actions and provenance.
After stopping the source, VM2 restarted with zero peers and again matched both
saved journeys. The source restarted on its retained ledger; VM2 reconnected
with one peer and both comparisons passed without transaction resubmission.
The consumer and restart jobs terminated with exit 0. Evidence is in
`target/validation-evidence/corrected-crosshost-20261016/` and VM2
`corrected-install.PKdDNGF8`. No wallet profiles or private signing material
were transferred. This verifies orderly recovery, not a crash during an action.
Current-candidate snapshot recovery, remaining P3/P5 acceptance and the full
capability plan remain open. Public release gates stay closed.

C02/C03 and P4 snapshot acceptance, 2026-10-16: corrected daemon `53f82698...`
passed operator attestation/export, expected-epoch authentication, altered-archive
rejection, fresh installation, receiver SIGKILL/restart, disconnect/reconnect,
exact lifecycle balances and a new confirmed payment. This run used two processes
on VM1 and exited 0. Archive SHA-256 is
`4a66af71122b595637c7027c78c516bca9f9c1a4e5c51f17a825d791770b4758`.
VM2 then authenticated that archive and recovered the lifecycle state, but its
consumer verifier failed at `block_info` for a saved payment. The independent-host
job exited 1; it is not passing acceptance. RPC source inspection shows
`handle_block_info` reads only the local block store and returns BlockNotFound
without peer retrieval. Decision 10.4 selects recovery of historical blocks from
peers. Determine the missing block's authenticated recovery path and restore
consumer history without weakening the assertion or treating recovered balances
as proof of available history. Evidence is in
`target/validation-evidence/corrected-snapshot-20261016/`; retained host data is
VM1 `corrected-snapshot.mwvpsmnS` and VM2 `corrected-snapshot-peer.lReMl2tP`.
Governance recovery was not exercised in this batch. No capability or release
gate is signed off; the source ledger and failed receiver are preserved.

The missing-body RPC path now delegates to a read-only node history reader.
It authenticates the retained snapshot from configured genesis, constructs its
account cursors and uses the existing bounded witness transport. Peer account
hints do not supply trusted roots. Only a membership-proven block is reported
confirmed; the lookup does not install blocks, change indexes or replay balances.
One lookup per reader is allowed at a time. This currently covers retained
snapshot prefixes, not every archival or account-history API.
Bounded `cargo check --locked -p burst-node -p burst-rpc` passed. The focused
`consumer_history_lookup_proves_membership` library test now passes on VM1 with
source hashes matching the worktree. It covers successful retrieval, unchanged
storage, corrupt/foreign-network snapshot refusal and a busy reader. The reader's
owned lock also stays with blocking authentication when its caller cancels.
Earlier stopped builds and a fixture export with insufficient signatures are
not passes. The fixture now signs with its actual verified-human roster.
Evidence is in `target/validation-evidence/history-lookup-20261016/`.
The subsequent default-network build was stopped before deployment because this
retained ledger requires the pinned private-Test identity. The corrected build
passed under 1 CPU, 3 GiB and zero swap. Daemon SHA-256 is
`d5bda0d736b884a6471282bbae75261211746672784340fcbccff15fac02a892`.
That binary ran on both hosts. The previously failing VM2 snapshot receiver,
without reset, passed the unchanged consumer verifier: confirmed payments,
certified verification context, representative/participation actions, endorsements,
TRST provenance and exact recipient balances. The saved challenge/appeal lifecycle
comparison also passed before and after the lookups. The receiver job exited 0;
temporary source/receiver processes stopped, with normal Dev nodes untouched.
`history-recovery.log`, input hashes and the build log are saved in the evidence
directory above. This closes the observed P4 missing-payment-body failure.
It does not qualify every history API, all P4 failure boundaries, remaining P3/P5
acceptance or full release. Public gates remain closed.

The candidate bundle now includes daemon `d5bda0d7...` and its matching source.
Wallet/dashboard packages are unchanged; their dependency lists do not include
the modified node/RPC crates. VM1 `history-candidate.zWNVVdIa` and VM2
`history-candidate-peer.W765tVhH` pass whole-bundle preflight. Transfer SHA-256 is
`baf11bbba2e632e8fe169709966b51e55cf03df12dddf704f2e610a53eb3959b`.
Assembly evidence is `target/validation-evidence/history-candidate-20261016/`.
VM2 also verified the installed wallet's exact binary hash. Its native governance
journey now passes on this candidate. The UI published and voted for the 180-second
Test exploration window and observed certified activation. It then published the
complete multilingual constitution amendment, registered a proposal-scoped
delegate, cast its delegated ballot and twenty direct ballots, and observed
certified adoption with 21 yea votes. The live open-phase counter showed 20
direct votes; closed counting included the delegated ballot. No counting rule
was changed to make the test pass. Post-count delegation revocation preserved
the certified tally, and cancelled reviews signed nothing.
VM1 independently matched the saved result. After orderly restarts, both hosts
again matched the complete document, activated parameters, tally and revoked
delegation. Both also passed the saved lifecycle and consumer checks, including
exact balances. Native and restart jobs exited 0; temporary nodes stopped.
Evidence is `target/validation-evidence/history-governance-20261016/` and VM2
`history-candidate-peer.W765tVhH`. This completes the current-candidate governance
journey, not all governance fault cases or P3/P5. Public gates stay closed.

C02/C03 and P4: the same daemon `d5bda0d7...` now passes snapshot recovery of the
combined latest governance/lifecycle ledger. Archive SHA-256 is
`cf5aaa6d813523a1fb05ab3211df073f4528e45bff9894bb3bf19a5a0fb21eab`, epoch
`addc521f67320823dcb1c7a1f2c1b7eb65c5a08ca983947aab4b395a3af151c8`.
The scenario verifies operator attestation/export, expected-epoch authentication,
altered-archive rejection, fresh installation, receiver SIGKILL/restart and source
disconnect/reconnect. Both nodes match the saved lifecycle balances and the full
governance report after each boundary. A new payment then confirms on both nodes
with identical pending amount. This was two loopback processes on VM1, not an
additional independent-host run. The bounded job exited 0 and stopped its child
nodes; normal Dev service remains running. Evidence is
`target/validation-evidence/history-mixed-snapshot-20261016/`; retained archive and
receiver are in VM1 `history-mixed-snapshot.4ESxnsQw/snapshot-runtime-P27FEG`.
The pilot table now reflects these results instead of calling completed journeys
absent. Final change-impact/artifact acceptance, remaining failure/topology checks,
creator-owned identity and handoff remain open, as does the full capability plan.

C21/P5 runtime dependency correction, 2026-10-16: measured GUI ELF requirements
include GLIBC_2.39. Both app configurations now declare libc6 >= 2.39; package
preflight enforces the frozen configuration's runtime dependencies before any
installation. The Debian-host test covers matching packages and a second package
with missing or too-low libc metadata, with no installation on rejection. All six
tests pass there; the desktop skips the real Debian test because tools are absent.
Tauri rebundled the previously qualified executables on VM1 in
`package-dependencies.9MurCElk`. Full extracted payload comparisons and updated
artifact/source preflight pass. The first attempt stopped on a different cached
wallet executable; it did not produce a qualified package. The retry used the
exact previous package payloads in a separate bundling directory. Both logs and
new checksums are retained in
`target/validation-evidence/package-dependencies-20261016/`.
Tauri warned that the existing binaries lack its bundle-type updater marker;
updater behavior is not qualified by this result. Apt's dependency simulation and
actual installation both pass on VM1, without removing packages or restarting
services. Installed dependency metadata and both executable byte comparisons
pass. `install.log` retains that evidence; this was replacement of installed
packages, not a clean-host installation. The corrected packages also install on
VM2 in `package-peer.hAC26IUe`. Whole-source/artifact checks and installed binary
comparisons pass there, followed by the native Dev wallet and dashboard journeys
on daemon `d5bda0d7...`. These cover backup/restore, signer binding, payment review
and cancellation, confirmed payments, returns, automation, group policies,
dashboard RPC/navigation and focus restoration. The successful job exited 0 in
59.7 seconds with a 458 MiB memory peak. It used one CPU, 3 GiB and zero swap.
The first UI attempt failed because the remote 64-task cgroup rejected a WebKit
fork; the retry allowed 256 tasks on that VM only. Desktop limits did not change.
An earlier checksum failure caught an incomplete transfer before extraction.
Both failures remain recorded, not counted as passes. Public logs and checksums
are in `target/validation-evidence/package-peer-20261016/`; profiles remain on the
host. This closes the package metadata omission and its second-host regression,
not P5. Creator identity, final artifact acceptance, remaining P2-P4 evidence and
the full capability plan remain open; all public release gates stay closed.

C03/C04/P4 current-artifact minority recovery, 2026-10-16: daemon `d5bda0d7...`
passes the existing `run-validation.sh --experimental-testnet` Dev journey with
three separate signer processes. The helper requires identical installed rosters
before isolation. The surviving signer has 10,000 of 30,000 human weight; two
signed payments remain unconfirmed during its four-second observation. After
SIGKILL and reconnecting all signers, the same hashes confirm on all three nodes
without resubmission. Frontiers, cumulative burns and both pending amounts match;
no recipient action is invented. Earlier phases check signed delegation without
creator bonus, merge/provenance, returns, governance withdrawal, economic state
after hard restart, a fresh payment and empty observer catch-up.
The bounded VM1 job exited 0 in 65.2 seconds. Reports, source/artifact hashes and
logs are in `target/validation-evidence/current-quorum-20261016/`; the ledger is
retained at `current-quorum.C23njbYA/evidence/burst-validation.cfrLf98p`.
The first run selected the helper's default-build mode, which incorrectly expected
this private-testnet artifact to disable certified selection on Dev. It failed
that assertion and remains retained separately. The passing run uses the existing
enabled-capability option; no application code or acceptance assertion changed.
This closes current-candidate qualification of that scenario, not C04, certified
human onboarding or independent-host conflicting-branch quorum resilience.
Creator identity, remaining P2-P5 acceptance and full capability requirements
remain open. Public gates stay closed.
The extended runner now reopens the minority alone before restoring quorum.
`verify-isolated` makes five observations over four seconds, checking zero peers,
minority weight, the two original unconfirmed hashes, exact account head/count,
unchanged confirmation height and cumulative burns. The same daemon passes this
boundary and subsequent confirmation on all three nodes without resubmission.
The VM1 job exited 0 in 65.6 seconds, including a focused validation-helper build.
Evidence is `target/validation-evidence/isolated-restart-20261016/`; retained
ledgers are in `isolated-restart.3szWVDFc/evidence/burst-validation.EaCR7VME`.
Only the runner and helper changed; no daemon rebuild or gate change was needed.
This extends the same C03/C04/P4 scenario, not full adversarial quorum sign-off.

C16/C18/P2/P4 account-history inspection, 2026-10-16: the retained snapshot
receiver has four confirmed blocks for the lifecycle recipient but returns one
history entry and no next cursor. The archival node returns all four. Daemon
`d5bda0d7...` is unchanged. `account_history` enumerates only the local height
index, whereas the repaired `block_info` path can recover a known hash. Missing
indexed bodies also become fabricated zero-valued entries. This is an unmet part
of decision 10.4 and the recorded consumer history requirement, not new
scope. The next implementation must join authenticated account ranges with local
suffixes and preserve pagination; replacing placeholder text alone is insufficient.
Evidence is `target/validation-evidence/history-enumeration-20261016/` and VM2
`history-inspection.FSC4I2AW`. Receiver startup initially waited for historical
dependencies; it became ready after its archival source connected. Both owned
diagnostic nodes stopped afterward. No capability became complete and no gate
changed. The concrete failure is now part of the pilot acceptance record.
The repair now uses account-chain heights and the existing snapshot-authenticated
reader, retaining only the requested page while walking bounded proof chunks.
Local and recovered entries must agree on account/hash/height; incomplete pages
and changed account heads fail explicitly. Ordering and cursor format are unchanged.
Bounded desktop RPC/node compile checks pass. The focused VM reader test passes
individual/full pages, invalid ranges, wrong-network and corrupt bases, busy
lookup and unchanged storage. Private daemon SHA-256 is
`49da5444670bd544c91627fd8a67da6121535286331cbbfecbe4f734405b463a`.
On the retained failed VM2 ledger, page sizes 1, 2 and 25 now match the archival
VM1 response exactly. Balances and authority remain unchanged; saved lifecycle
and consumer recovery assertions also pass. Logs, source/artifact hashes and the
archival reference are in `target/validation-evidence/history-page-20261016/`.
Both owned nodes stopped afterward. This closes that enumeration failure, not
large-history performance, every missing-history case or full artifact/P2-P5
qualification. No release gate changed.
Final history candidate: strict RPC/node Clippy passes after naming the callback
return type. VM1 `history-final.Upy4h742` contains matching frozen source,
unchanged qualified Debian packages and daemon
`e27911102c40a36594020ddb4a7834b426ba97664a99ad5522e3df462e4e4864`.
Whole-bundle preflight and the two-host retained-ledger history/lifecycle/consumer
check pass on that binary. Evidence is
`target/validation-evidence/history-final-20261016/`. Temporary nodes stopped;
normal Dev service remains active. This replaces the history candidate's artifact
identity, not the broader acceptance status. Creator identity and remaining
P2-P5/full-capability requirements remain open.

Installed Activity acceptance (2026-10-16) initially failed on that candidate. The retained
snapshot has 110 founder blocks; its exact head matches the archival chain's
110-entry prefix. The new native test imports the published disposable Test key
and opens Activity with automatic signing disabled. The first 25 entries do not
render: the UI reports `historical lookup is busy; retry later`. Dashboard also
requests history, while `historical_lookup::Reader` rejects concurrent requests.
Evidence is `target/validation-evidence/native-history-20261016/`, including the
earlier reference-height mismatch and both UI failures. The existing P2/P4 history
requirement needed bounded concurrency handling; RPC-only passes did not close it.
The reader now admits at most three requests, runs one at a time, and limits
waiting to ten seconds. Cancellation returns admission capacity, including when
blocking base preparation still owns the active guard. The focused authenticated
reader test passes concurrent page/hash lookup, cancelled waiting, overload
rejection and unchanged storage. On daemon
`76cd238f847b7ac91fa501a1070f4218d05109475be7bcd0eb7c57c37e4b7f17`,
the unchanged installed wallet passes all five pages through the exact 110-block
snapshot head. Entries match archival hash/account/operation/balance, no error
appears, pagination ends, and account state/balances remain unchanged. Evidence is
`target/validation-evidence/history-queue-20261016/`. This closes the reproduced
navigation failure, not large-history performance. The matching frozen bundle at
VM1 `history-qualified.82P0q140` now passes source/artifact/package preflight.
The same daemon also passes retained two-host history, lifecycle and consumer
recovery. These checks do not repeat onboarding, certify upgrades, or qualify
public network identity. Bundle manifests and recovery results are in
`target/validation-evidence/history-qualified-20261016/`. Temporary nodes stopped;
normal Dev services are unchanged. P2-P5 remain open.

Cold-join timing classification, 2026-10-16: the 180-second bound comes from
the observer validation scenario documented in `TESTNET_PILOT.md`, not an
explicit whitepaper or implementation-decision launch requirement. Keep the
failed timing results and the unchanged scenario threshold. Do not describe
them as failed protocol correctness or as the sole reason public release is
unsafe. Correct authenticated recovery, usable catch-up and the specified
throughput/resource targets still need their own evidence. After validating
the pending replay-cache refactor, prioritize remaining capability and safety
gaps rather than repeatedly optimizing only this scenario's deadline.

C03/C07/C09 and P2/P4 have a recovered independent-host onboarding journey,
but are not signed off. Clean installation and the Dev wallet/dashboard journeys
passed on VM2 (`installation.HwMto4aT`). Fresh Test recovery reached the source's
exact certified epoch, parameters, 559 blocks and 58 verified accounts, but took
about 246 seconds against the 180-second cold-join target (C02/C14).
The subsequent native certified onboarding stopped after five confirmed votes:
the joining daemon exited while installing a selection with
`selection certificate changed; reload the certified chain`.
Logs are in `target/validation-evidence/native-release-20261015/independent-install/`.
The stopped ledger and encrypted wallet profile remain on VM2. A byte-compared
ledger backup is retained in `pre-repair.M4OPZHNA`. Do not resubmit endorsements
or confirmed votes. Compact activation now persists its authenticated beacon in
the checkpoint transaction, including duplicate acceptance of the old layout.
The regression passed conflict rollback, reopen and duplicate-repair checks;
the existing TCP activation/recovery test also passed. Logs and exact input
hashes are in `activation-repair-test.log` and `activation-integration.log` in
the evidence directory above. The desktop compilation was stopped without a
test result; these passes came from the existing validation host. A new gated
daemon built separately in `activation-repair.1Z7OddSp`, SHA-256
`0e033c09056d369eff522731db60b4098f35f445d61a615ea9e5418ab0a38291`.
The saved-ledger retry confirmed a beacon mismatch at selection height 57
(`activation-joining-retry.log`). Source snapshot export and exact-epoch
authentication passed at height 58 (`activation-snapshot.log`), but installation
correctly refused to replace later locally confirmed votes with the older cut
(`activation-snapshot-recovery.log`, `snapshot conflicts with local finality`).
The subsequent proof-only repair preserves that safeguard and the same ledger.
Recovered activation proofs pass the existing authenticated attachment path
before selection publication. Matching durable proofs do not create another
transaction; conflicting proofs remain errors. Ordinary and snapshot-backed
regressions cover repair, reopen, unchanged economic state and idempotence.
`proof-recovery-recheck.log` retains the focused regression results and source
hashes. The gated daemon SHA-256 is
`102565e071afe9aa0bfefc29a42fca979ec1893954b9876a736e1a981f53472e`.

The native resume driver checked the saved selection context and five confirmed
votes before reusing them. It submitted the two remaining votes through the UI,
then completed verification, accrual, payment, TRST transfer and receipt.
Funding and endorsements were not repeated. `proof-recovery-native.log` and
`proof-recovery-onboarding.json` retain the journey results. The independent
source confirms the same verified subject, context, confirmed payments,
provenance and exact 1.001 active TRST recipient balance in
`proof-recovery-source-verification.log`. After the target daemon was killed,
the same ledger restarted without snapshot replacement. The same assertions
pass in `proof-recovery-restart-verification.log`; startup is retained in
`proof-recovery-restart.log`. This closes the missing-beacon failure for this
interrupted journey. It does not qualify all C03/C07/C09 behavior or transfer
older candidate results to the new daemon. P2-P5 and public release remain open;
the cold-join timing failure above is unchanged.

The repaired source and hard-restarted independent target also match the saved
native lifecycle and governance reports. Checks cover restored verification,
the post-appeal payment and recipient balances, certified parameter activation,
the complete constitution document, its vote tally and post-count delegation
revocation. `proof-recovery-mixed-source.log` and
`proof-recovery-mixed-target.log` retain the results and verifier hashes.
These are read-only recovery checks of previously signed operations, not new
native submissions or another complete lifecycle run.

C02/C03/P4 snapshot recovery passes on daemon `102565e0...` above using the
mixed ledger after recovered onboarding. The run authenticates an operator
export at epoch `6a1710073caa9698c9b6c4998d735142d33619003e42b2a495f9306bc4245090`,
rejects an altered archive, installs into an empty ledger, hard-restarts the
receiver, disconnects/reconnects it and confirms a new payment on both nodes.
Saved appeal balances, constitution, governed parameters and delegation state
match throughout. Snapshot SHA-256 is
`2b36b1209a7fd0aca84138255e63df247a1a11ef7333c00f709182f3a5635285`.
Evidence is `proof-recovery-snapshot-fixed.log` and `proof-snapshot/` under the
independent-install directory. Host evidence is
`proof-snapshot.qSMf6qpi/snapshot-runtime-PeXlIx`. The run took 65 seconds,
peaked at 174.5 MiB and used no swap. Both nodes ran on one validation host;
this does not prove distributed quorum or independent cold-join performance.

The first snapshot attempt failed because the service's working directory was
not writable. `tools/validate-snapshot-runtime.mjs` now starts its children in
the private evidence directory, where standalone verification can stage files.
The failed `proof-recovery-snapshot.log` remains retained. No authentication,
snapshot policy, resource ceiling or release gate changed.

C02/C14 cold-join work remains open. The new baseline on daemon `102565e0...`
started from an empty VM2 ledger at 01:56:17 UTC. At 188.5 seconds it had 491
of 579 source blocks, so it missed the unchanged 180-second target. Both VMs
were deallocated at 02:00 UTC, matching their enabled daily shutdown schedule;
the run did not finish. `cold-profile-observation.log`, `cold-profile-target.log`
and `cold-profile-source.log` retain the evidence. The shutdown schedule was
not disabled. Only VM1 was requested to restart for focused compilation.

Selection refresh currently includes selections whose beacon is already stored,
which can send known records through historical replay during catch-up. The
pending change in `checkpoint_store/refresh.rs` skips those scheduling hints
for inline and separate proofs, including archived and snapshot-backed records.
It leaves proof authentication and conflict rejection in replay/publication.
The focused tests pass for missing proofs, inline/separate stored proofs,
refresh after proof removal and sparse archived traversal. The snapshot-backed
late-proof test also passes valid recovery and invalid-proof rejection. Output
and exact input hashes are retained in `refresh-recovery-build.log`; the test
binary compiled in 6m43s on the validation host. These tests do not establish a
cold-join improvement. The gated daemon build passed in
`refresh-recovery.t1qPIudS`; SHA-256 is
`760131aca51b1ea6ab06df971df0a5af905f89c9536a1c95eee2a43b5053f04c`.
The serial test/build job took 10m02s, peaked at 3.7 GiB and used no swap.

An independent empty-ledger join on that binary completed in 261.9 seconds,
failing the unchanged 180-second target. It recovered all 579 blocks, 67
accounts, 59 verified wallets and the exact certified epoch. The saved native
wallet balance/provenance, post-appeal state, governed parameters, constitution
and delegation checks passed. This is correct recovery but failed timing, not
completion of C02/C14. `refresh-cold-result.log` retains the failing overall
result; `refresh-cold/` holds the input hashes, result and detailed node log.
Host evidence is VM2 `refresh-cold.cqRw8e`. Later checkpoint preparation took
roughly 1-2 seconds per step; the refresh change has not resolved that work.
Review replay/cache reuse and repeated preparation before another candidate
build. No deadline, authentication requirement or release gate changed.

C02/C03/C14 follow-up removes a second replay of each installed successor.
Previously `commit_with_recovery` returned the parent in its cache, so the next
installation replayed the checkpoint just committed. Preparation now uses the
shared `RecoveredSnapshot` replay path and retains its successor, selection
receipts, authority history and record observations. The separate continuation
installation replay implementation is removed. Commit-time economic, journal,
cut and observation checks remain in place. A regression checks that the cache
matches installed state and rejects a changed successor record.
The desktop's unchanged resource-limited `cargo check --locked -p burst-node
--lib` passed in 19.23 seconds with incremental compilation disabled. The
validation-host tests now pass successor-cache equivalence and changed-history
rejection, preparation without writes, writer rechecks, live engine publication,
atomic activation proof persistence and activation recovery over TCP. Four live
record-only catch-up scenarios also pass, covering genesis and snapshot-backed
receivers, a source without snapshots, full proposal content and restart.
The test binary compiled in 6m32s and the gated daemon built in 3m17s. The
serial host job peaked at 2.9 GiB with no swap. Candidate SHA-256 is
`a569edfdc36fc817b0fe4ebdca8ad18a393302b18edca7ed99ecba9cb87d9756`.
Source and build evidence is in `successor-cache-build.log`, host stage
`successor-cache.mSqqIsiI`.

That exact candidate recovered the saved onboarding, appeal, constitution,
governance and delegation state on the source and an independent empty-ledger
receiver. Both match the saved reports, including confirmed payments and the
exact recipient balance. The receiver reached 579 blocks, 67 accounts, 59
verified wallets and epoch
`6a1710073caa9698c9b6c4998d735142d33619003e42b2a495f9306bc4245090`.
It took 271.5 seconds and failed the unchanged 180-second timing assertion.
The receiver job peaked at 126.6 MiB with no swap under a one-CPU/3-GiB limit.
This proves this history's recovery, not a speed improvement, full C02/C14
qualification or distribution readiness. Evidence is
`successor-source-verification.log`, `successor-cold-result.log` and
`successor-cold/` in the independent-install directory. The preserved receiver
ledger is VM2 `refresh-cold.4zU1Hx`. No transactions were resubmitted.

The same candidate also passes authenticated snapshot recovery and independent
ledger restart. `successor-snapshot-result.log` and `successor-snapshot/` retain
operator export, exact-epoch verification, altered-archive rejection, fresh
installation, SIGKILL/restart, reconnect, saved lifecycle/governance comparisons
and a new payment confirmed by both nodes. Snapshot SHA-256 is
`2b36b1209a7fd0aca84138255e63df247a1a11ef7333c00f709182f3a5635285`.
The loopback-host journey took 71.8 seconds and peaked at 205.5 MiB with no swap;
it is not distributed-quorum evidence. Host evidence is
`successor-snapshot.CWFArUkc/snapshot-runtime-anu4Qt`.
The independently joined VM2 ledger then passed saved-report comparisons after
startup and again after SIGKILL, without resubmission or snapshot replacement.
Evidence is `successor-restart-result.log` and `successor-restart/`, host stage
`successor-restart.9PM5H4`. These close the recorded candidate's recovery checks
for this mixed history. Exact application artifacts and remaining P2-P5/full-plan
requirements still need qualification; no public gate changed.

Installed-app pairing, 2026-10-16: the existing wallet executable
`094c32254c21cef6a6c84794166618b655901aa726d8be3b1ea2833dbde50338`
and dashboard executable
`c045ed0f8a84828f6d8fc27107382e3cee9ef224d36670e6f20976b607ed4965`
match their original installation hashes. Neither app embeds the daemon.
Both passed their native Dev journeys against daemon `a569edfd...` above in
isolated profiles on VM2. Wallet checks cover mnemonic/encrypted backup restore,
signer binding, payment review and receipt, returns, automation cancellation,
pending pagination and origin-based group policies. Dashboard checks cover real
RPC data, verification fields, all tested routes and restored dialog focus.
Evidence is `successor-apps-result.log` and `successor-apps/`; complete native
profiles/logs remain in VM2 `successor-apps.vJ0OT2zM`. The serial job took
60.9 seconds. No app rebuild or production gate change was needed.
This is Dev pairing evidence. Certified Test app operations on this daemon,
final source/artifact provenance and distribution instructions remain open.
The candidate still uses the published seed-42 fixture identity described in
`TESTNET_PILOT.md`; it is not the secret shared-pilot identity and must not be
distributed as that network's release artifact.

Certified Test application qualification on that pairing now has a resumed
cross-host journey. VM2 `successor-certified.NxpPYIel` passed wallet creation,
backup restore, pre-verification delegation, participation and three paid UI
endorsements, then exited with status 1 before recording a vote. Its SSH observer
stalled and did not retain the final error; the service's terminal failure was
verified separately. Do not call this an uninterrupted pass or infer a protocol
fault from the missing error. The ledger retained all three endorsements and a
certified seven-person panel with zero votes. The saved wallets were resumed at
that exact context without repeating funding or endorsements.
VM2 `successor-certified.8utB4rB1` then passed seven reviewed UI votes,
verification, accrual, reviewed payment, TRST transfer and receipt. Source and
receiver independently match the saved report and exact recipient balance.
Evidence is `successor-certified-result.log`, `successor-certified/` and
`successor-certified-source-verification.log` in the independent-install
directory. The resumed run has a durable host `run.log`; the first failure's
cause remains unresolved. This supports C07/C18 recovery and P2, not complete
P2-P5 sign-off. No daemon or application changed between attempts.

The next uninterrupted run passed on the same daemon and installed wallet.
VM2 `successor-certified.tKfCQmUH` completed creation/backup restore, paid
endorsements, participation, certified assignments, seven reviewed UI votes,
verification, accrual, payment, transfer and receipt without a resume. Both
hosts match the resulting certified context, confirmed payments and exact
recipient balance. Evidence is `certified-clean/` and
`certified-clean-source.log` in the independent-install directory.
`wallet/tests/native-session.mjs` now retains each child test's stdout/stderr in
a private per-step file before relaying it to the terminal. Its bounded syntax
check and this native run passed. Only test logging changed, not the app or
daemon. The earlier intermittent failure remains unexplained; a passing repeat
does not erase it. This completes this uninterrupted candidate onboarding
scenario, not the remaining certified lifecycle/governance and release audit.

C18/P3 receive recovery gap reproduced in VM2 `successor-lifecycle.h4viyKBC`.
The UI submitted challenge `c90010c29165f484b2208d29d346f867984de003247480b42b3bf503f84b27c5`
and confirmed a payment while challenged, as decision 17.3 requires. Receiving
that payment timed out waiting for the UI's `Received` state. The durable
wallet journal retains receive hash
`2d1f2ca1a7c0632535c79ac20d867c7c3ef52f5ce625f6a58ce4fafb09963ae7`.
The node's atomic `payment_exclusion` query confirms its epoch
`4799c607f38e242c6fcf8a04dcbd747521676718b13ac2be9a9fe8a77b181c88`
is excluded under installed epoch
`31a8b8cf080660206240fbf63e2b8ea138c3d630097395180469f0b6ab861f16`.
The receiver still has its confirmed predecessor and the original 1-TRST claim
is pending. No missing block alone was treated as exclusion proof.
`ReceivePage` now links saved transactions to the existing reconciliation flow,
including after reopening Receive. This route does not offer resubmission or
an unrelated offline burn. Browser/IPC checks pass for confirmed, excluded,
superseded and unresolved receipts, keyboard navigation, and return to Receive.
Unresolved receipts remain saved and can be checked again. Evidence is
`receive-recovery-browser.log` in independent-install; the host stage is
`/home/burst/receive-recovery.ePgzr0X0`. TypeScript also passes locally.
The desktop browser could not start within its thread ceiling; no limit was
raised. The browser results came from the existing VM1 with 3 GiB RAM, zero
swap and one CPU equivalent. These mocked IPC checks do not prove native
reconciliation. The subsequent native recovery is recorded below. Do not delete
the saved drafts or repeat the challenge/payment. Original native evidence is
under `successor-lifecycle/` in independent-install. C18 and P2/P4 remain open.
The wallet profile is `burst-native-wallet-2JW5YL` within the host stage; the
progress report records the original encrypted-backup paths. This failed run
does not qualify revocation or appeal behavior on the candidate.

Native Receive recovery now passes with wallet executable
`24781ef61deee0a60ece8b3efb73925f8d91efad5c364e5b4ec93c458dd1d2aa`
and daemon `a569edfd...`. On VM2, `receive-lifecycle.i7PI0INO` used the saved
profile, followed the Receive recovery link, retained the exact signed block
and ancestry in an exclusion archive, and verified unchanged account head and
pending claim. Its next step failed because the driver looked for the dashboard
Receive button while already on Receive. The driver now handles either route.
`receive-lifecycle.k0G7oymx` resumed from that archive without repeating the
challenge, payment or reconciliation. Fresh receive review and all seven native
challenge votes passed. Certified revocation invalidated both subject-origin
payments while preserving unrelated founder-origin TRST.

The appeal was confirmed as
`0638fa85b0965a00e041195ccafa67f77b1a25a2b9a13c0694b8d1ef8884f990`,
but the native driver timed out observing the first appeal vote. Its saved hash
is `c90ff095686d1865e286039851a78f61ab9a83f38d443ab0590878e94942a21e`.
An atomic exclusion query proved epoch `75e03f6f...` excluded under installed
epoch `05aa03d2...`; no appeal votes were recorded at that interruption.
The resume below recovered this saved vote. C18 recovery and C07/C09 full
lifecycle qualification remain open. Logs, progress and input hashes
are under `receive-recovery/` in independent-install. Build evidence is from
VM1 `receive-recovery.ePgzr0X0`; no optimized desktop build ran. Public gates
remain closed, and this interrupted journey is not an uninterrupted pass.

VM2 `receive-lifecycle.pz8lISV9` completed the existing appeal through the native
UI. It checked the exact panel and confirmed appeal, reconciled the excluded
vote without resubmitting it, and recorded seven fresh Legitimate votes. The
certified reversal restored verification and both subject-origin TRST payments.
It also reconciled the saved revoked-state spending attempt, then completed a
new payment and receipt. The recipient ended with 3.001 active TRST and no
revoked TRST. The independent source passed the same final verification-state,
confirmed payment/receive and portfolio assertions. Evidence is
`receive-recovery/appeal/`; the source report is VM1
`receive-recovery.ePgzr0X0/native-lifecycle.json`, checked with
`qualified-debug.nzieknOy/validate_testnet_onboarding verify` against port 28077.
The first appeal resume wrapper stopped before app launch because it expected
61 verified wallets, ignoring the tested revocation; its retained stage is
`receive-lifecycle.SI6szKxe`. The corrected precondition requires 60 at that
resume point. Neither the appeal nor challenge was repeated.

This closes the interrupted journey, including recovery, for the recorded
candidate. It does not explain the earlier vote UI observation timeout or prove
an uninterrupted lifecycle, all recovery cases, final artifact installation, or
public-network readiness. Those gates remain open work; no public gate changed.

C18/C20 vote feedback has a reproducible polling-failure gap. The new cases in
`wallet/tests/verifier-participation.test.cjs` hold a vote submission, fail the
verification-status poll, then resolve the vote as confirmed or uncertain.
Neither result remains accessible in the baseline UI. The existing successful
vote, cancellation, exact stake/context and eligibility tests pass. Baseline
evidence is `receive-recovery/vote-status-before.log`. Moving the receipt to the
page, lifting action state, and a separately subscribed receipt did not satisfy
the regression. Those production edits were backed out; failed evidence remains
in `vote-status-after.log` and `vote-persistent-receipt.log`. The tests remain
regressions, not skipped checks. DOM inspection subsequently found duplicate
vote panels and an orphaned dialog: the vote and appeal siblings shared the same
React key. Their keys now include the panel kind. The vote panel also remains
mounted during eligibility polling failures, retains its transaction receipt,
and disables unsigned actions until eligibility is known again.

The final browser checks pass confirmed and uncertain outcomes across failed
polls, status restoration without duplicate submission, and a successful new
poll while submission is pending. Existing exact stake/context, cancellation
and eligibility checks still pass. Evidence is `vote-distinct-keys.log`,
`vote-polling-final.log` and `vote-successful-poll.log` under
`receive-recovery/`; `vote-dom-inspect.log` retains the duplicate-panel diagnosis.
This closes the reproduced browser/IPC defect, not native release qualification
or the unexplained historical timeout. Component guidance helped keep receipt
ownership in the existing panel rather than adding another transaction owner.
The native build passed on VM1 in `vote-native.YnP1EiRl`. Executable SHA-256 is
`a1f3bfad2bd0592d0f45a3ee82d4e77ad8b9e2f72dccec7eeed509b5bf3d65db`;
Debian package SHA-256 is
`7bf3e7fc59a000fc1689cef232c330cc9ec34d5029f78f085933304e43c9d7a4`.
The rebuilt app then completed an uninterrupted native onboarding journey on
VM2 in `vote-native-onboarding.DKpmp4sr`: encrypted backup/restore, delegation,
participation, three paid endorsements, seven reviewed votes, verification,
accrual, payment, transfer and receipt. Unfunded, disconnected and locked signing
were rejected. The wrapper exited nonzero afterward because it called the
lifecycle verifier on an onboarding report (`missing verification account`).
The existing `verify-consumer.mjs` accepts the correct schema and passes against
both independent nodes without repeating signed actions. It checks the exact
certified context, confirmed payments, transfer provenance and 1.001 active TRST
with no pending, expired or revoked balance. Evidence is
`receive-recovery/vote-native-onboarding/`; package build inputs and hashes are
in `receive-recovery/vote-native-build/`. This qualifies that native onboarding
scenario on the rebuilt executable. Installer acceptance, remaining lifecycle
and governance qualification, recovery coverage and P2-P5 sign-off remain open.
No release gate changed.

C18/C21 installed-wallet qualification uses Debian package `7bf3e7fc...` above.
VM2 upgraded `burst-wallet` with `dpkg -i`; `dpkg --verify burst-wallet` passed.
The installed executable SHA-256 is
`65b56753f2326c7f5c0aeac652c859555350eab4e410c4e2ac90a75df02ca050`.
It differs from the pre-packaging executable and is tracked separately.
`installed-wallet/package-inspection.log` retains its package, launcher and
hash evidence. Optional ELF inspection failed because `readelf` was unavailable;
that inspection is not claimed as a pass.

The installed app's challenge journey in VM2 `installed-lifecycle.rThVNOXA`
confirmed the challenge, allowed the required challenged-state payment, and
received it. Its first challenge vote correctly displayed confirmation pending;
the driver stopped without resubmission. The saved vote is
`30e4d9e9570cebd97e1079d0939c2ffd6cfaebe654ae94f9e2cf2b74510e1306`.
The node's atomic exclusion query proved its old epoch excluded while the
account retained the signed predecessor. Evidence is
`installed-wallet/excluded-vote.json`. The lifecycle resume path now supports
a partial challenge panel, checks the exact saved context, confirmed challenge
and payment, recipient holdings, and reconciles the exact saved vote through
the existing UI before fresh review. Confirmed votes are reused, never repeated.
The bounded syntax check passed. VM2 `installed-lifecycle.zLXh7ZG7` completed
all challenge votes, certified revocation and origin-specific invalidation.
The first appeal vote also became uncertain with atomic epoch exclusion.
Its hash is `03bc69068ef296b66fc176cef4a3c81dad562e85e21a6d1151fb2e46eb35af9a`.
The app retained the vote and displayed its uncertain outcome; it did not
report success or resubmit. The existing appeal resume in
`installed-lifecycle.MYC6NbTi` reconciled it, completed seven reviewed votes,
restored verification and originated TRST, reconciled the rejected revoked-state
spend, and confirmed a fresh payment and receipt. The wrapper exited successfully.
Both independent nodes match final verification, confirmed payment/receipt and
the exact 3.001 active TRST recipient balance. Evidence is under
`installed-wallet/{first,challenge,appeal}/` in independent-install.

This completes the installed candidate's recovered lifecycle scenario, not an
uninterrupted pass. C03/C18 reliability remains open: both panels' first votes
were retired when a newer certified epoch installed. Diagnose why new panels
repeatedly admit a vote that cannot confirm before rollover. Recovery safety is
verified here; repeated manual recovery is not sufficient consumer acceptance.
Installed governance, broader recovery and the remaining P2-P5 requirements are
still release blockers. No protocol rule or release gate changed.

C03/C18 transport diagnosis found that `peer_read_loop` records block gossip
before admission and suppresses identical bytes until capacity eviction. A block
rejected while its committee or dependency is unavailable therefore cannot
benefit from later rebroadcast on a quiet network. This is a concrete retry
defect; retained native logs do not yet prove it is the whole first-vote failure.
Block gossip now uses a one-second suppression window. Duplicate arrivals do
not extend it, retries do not grow the cache, and signed vote relay retains its
existing deduplication. Authentication, epoch and committee validation remain
unchanged. Votes before activation remain allowed, as intended by
`verification_activation::validate_vote`.

The bounded local `burst-network` dedup tests pass, including timed retry,
capacity, permanent vote suppression and zero-capacity behavior. The socket
regression now sends identical block frames before and after the retry window
and checks the actual admission queue. The local `burst-node` compilation was
stopped through its owned validation service after sustained memory pressure
near 2.5 GiB; it produced no test result. No socket-test pass is claimed yet.
Exact-baseline remote source is staged in VM1
`gossip-retry.vNN04EHv`, with only `network/src/dedup.rs` and
`node/src/connection_registry.rs` replaced. The serial check/build job
`burst-gossip-retry-20261016` passed under 6 GiB RAM, zero swap and one CPU
equivalent. All 15 connection-registry checks passed, including the socket retry
regression and existing vote-relay/recovery isolation checks. The gated daemon
SHA-256 is `106c36b9b3b6294a42c883d360900ba075ea47a939a93155adbad97f3fc0285b`.
Build output and source hashes are in `independent-install/gossip-retry/`.
Both validation hosts have that exact executable. Native qualification must
pass before this reliability requirement can be closed.

The new daemon's native run in VM2 `gossip-native.6BMckagB` reached a pending
first vote. Detailed source logs now prove repeated admission of the same hash
`b4c8749e0a725d6ec60ba10495923bc3f6cf07f8ca1d7d8338844de57e56cf40`.
The source initially rejected it because the target was not accepting votes,
then accepted and confirmed it at 05:23:40 UTC. VM2 confirmed it too. No new
signature was made. This validates retry recovery in the live race; confirmation
took about 32 seconds and exceeded the wallet's initial wait.

The native driver now supports the existing saved-vote confirmation UI rather
than treating every pending result as permanent failure. It still requires the
exact hash and kind, actual confirmation, journal reconciliation and an unchanged
account head. Unresolved or excluded votes do not pass. VM2
`gossip-native.vwdm9xar` reconciled that confirmed vote, reused it, and completed
the other six votes, verification, accrual, payment and receipt. It then stopped
in the fresh lifecycle after a confirmed challenge: payment
`5b1398a241571680bc7358ad883e77bd3282629368a53cecdfbc7ae13ffbaf30`
was admitted locally but rejected on the source as `block payment epoch is not
installed`. Its signed journal and profile remain retained; do not repeat the
challenge or infer exclusion solely from the now-missing block. C03/C18
checkpoint-transition reliability and lifecycle acceptance remain open.
Reports and input hashes are in `gossip-retry/native/`. The desktop low-memory
guard refused the syntax check; the resumed VM job passed its syntax checks.
No public gate changed.

C21/P5 creator setup now has explicit file output in `genesis_keygen`. Unix
file mode creates a new 0600 seed file, refuses overwrites and symlinks, syncs
the file and prints public identity information without the seed. Network and
output mode are required; terminal disclosure requires `--show-secret`.
The parser and private-file tests pass, as does a real executable check of
public output, seed format/permissions, non-disclosure and overwrite refusal.
Evidence is `keygen-file-validation.log` in the independent-install directory;
`tools/validate-genesis-keygen.mjs` retains the executable check. No network
identity was created or replaced; validation generated disposable test keys
only. File mode is not implemented on Windows. Creator-key custody, final
private-network artifacts and the rest of P5 remain open.

C21/P5 packaging is in progress, 2026-10-16. The Linux candidate build now uses
`tools/build-linux-candidate.sh` with locked dependencies, one Cargo job, source
fingerprints and artifact checksums. It only accepts the gated private-Test
configuration and does not publish. Source archives were checked on the existing
validation host before extraction into `release-source.H6qK6F75`; output is
`linux-candidate.bNPvewTd` in its retained native-validation workspace.
The optimized daemon and both app builds passed. The wallet package is
`Burst Wallet_0.1.0_amd64.deb`, package
name `burst-wallet`, SHA-256
`56fc338be5e2bbea94a17a39b0faa23cc9180ca9cf274915329422428e1ddfd1`.
The node package SHA-256 is
`d48e0b8d23eac0fb1c50ed51dae2bc7804a3ca159129daff551fc46a70932e99`;
daemon SHA-256 is
`a13a73b5efd8809c94ecb674430787ee0f23da19a784953169eb814858fe133e`.
The candidate's `build.log` records the build and bundling results.
`tools/validate-linux-candidate.sh` checks artifact and frozen-source hashes
before installation, compares installed executables with package contents and
runs the existing native app journeys in isolated profiles. Both packages installed
and passed those Dev-network journeys. Wallet coverage includes backup/restore,
onboarding, reviewed payments, returns, automation and live group-policy checks.
Dashboard coverage includes native RPC, page navigation, verification details
and restored keyboard focus. The first dashboard attempts exposed test-driver
assumptions: permissive key-file creation, a development-server URL and selection
always paused. The corrected drivers retain the original frozen-source manifest,
record their own hashes and check the candidate's enabled Dev/Test capability.
No app binary changed to pass those checks.

Build records and all installation-attempt logs are retained under
`target/validation-evidence/native-release-20261015/linux-candidate/`.
Host wallet evidence is `installation.jJxxSDpU`; passing dashboard evidence is
`installation.ayQUlXlo`. The installed wallet's certified onboarding journey
passed on the retained three-node Test ledger (`consumer-ui.GuOTSZMD`). It covered
creation/backup/restore, paid endorsement reviews, participation, seven assigned
UI votes, verification, accrued-BRN payment and TRST transfer/receipt. All three
nodes agreed on the certified context, confirmed payments and exact balance.
The read-only restart check (`consumer-ui.tYN4m860`) passed without resubmission.
The local evidence directory includes `native-onboarding.json` and the certified
onboarding/restart logs. Candidate challenge/appeal validation also passed
(`consumer-ui.xuX4vg6h`): spending before judgment, seven challenge votes,
origin-specific revocation, seven appeal votes, restoration, safe reconciliation
of a rejected payment and a fresh reviewed payment/receipt. Three-node restart
verification passed in `consumer-ui.6WVxd3Em`, without resubmission. The same
local evidence directory retains `native-lifecycle.json` and both lifecycle logs.
Candidate governance UI completed parameter activation, exact multilingual
amendment adoption with 21 votes and post-count delegation revocation in
`consumer-ui.ZpZq1JFE`. Its immediate cross-node check failed because a peer still
reported the revocation as unconfirmed; the validator's normal mode does not
wait at that comparison. Preserve that failure rather than calling the whole run
a pass. The saved report passed on all three restarted nodes in
`consumer-ui.0FZPRWPl`, without another proposal or vote. Both logs and
`native-governance.json` are in the local candidate evidence directory.
Authenticated snapshot qualification passed against the same daemon in
`snapshot-runtime-K3Vyvp`: attestation/export, exact-epoch verification, altered
archive rejection, fresh-node installation, SIGKILL/restart, disconnection and
reconnection, exact lifecycle/governance state and a new confirmed payment.
Snapshot SHA-256 is
`6c77a1be20e8401c5b09e437a408196ebb7287287a31444793caf7fa7da8d82e`.
Its input hashes, result and logs are retained in the local candidate evidence
directory. This was a two-node loopback topology, not independent-host acceptance.
Candidate NAT qualification passed in `burst-nat.t8tOq8sN` with the exact daemon
hash above: inbound isolation, reconnect/payments, multiple default/distinct-port
clients, double NAT/CGNAT, manual forwarding and UPnP lifecycle/automatic discovery.
CGNAT gateways are reported as non-routable, not falsely described as traversed.
The lab used a compatible Trixie base because this candidate requires glibc 2.38.
Its daemon hash, lab image identity, driver hashes and logs are retained in
`burst-candidate-nat-evidence.tar.gz` beside `burst-linux-candidate-nat.log`.
The owned lab container was removed. This is controlled namespace/emulator
evidence, not physical-router breadth or distributed-quorum qualification.
The governance verifier now allows the existing 60-second normal convergence
window for the exact post-count delegation comparison, matching its activation
check. Its separate helper build and saved-report validation passed on three
restarted candidate nodes (`consumer-ui.2AnIRjeg`), recorded in
`burst-linux-candidate-governance-driver.log`. This check did not inject delayed
peer confirmation. No candidate app or daemon was rebuilt for the correction.
Remaining certified journeys, independent installations and other supported
platforms remain open.
C21/P5 is not complete.

Independent installation, 2026-10-16: the same candidate packages installed on
the existing second Ubuntu 24.04 validation VM, which had no BURST desktop
packages. Archive/source fingerprints and installed executable comparisons
passed, followed by the complete Dev wallet and dashboard journeys in fresh
profiles. Evidence is under
`target/validation-evidence/native-release-20261015/independent-install/`.
The initial attempt stopped before package installation because the validator
hashed a relative script path after changing directories. It now resolves that
path first; the resumed run passes. Host evidence is
`independent-candidate.YckU2594/candidate/installation.HwMto4aT`.
The second host's fresh Test ledger (`join.XW3NwfFV`) is now recovering over the
private network from the first candidate node. Cross-host certified wallet
actions and state comparisons have not yet run. RPC remains loopback-only.
Before building, five tested executables were copied and byte-compared into
`qualified-debug.nzieknOy`; only the workspace's rebuildable debug cache was
cleared, freeing 12 GiB. Ledgers, backups and evidence were preserved. No local
optimized build ran, and no release gate changed.

C03/C08/P3 lifecycle evidence, 2026-10-16: the rendered challenge/appeal
sequence now reaches revocation, origin-specific TRST invalidation, seven UI
appeal votes, restored verification, restored balances and a new UI payment and
receipt. Three nodes agree before and after restart: 3.001 active TRST, zero
pending, expired or revoked, with the original appeal context. Runs resumed
saved actions rather than repeating challenges, votes or payments.

The idle signer had retried stale checkpoint observations indefinitely. It now
re-authenticates installed authority on `StaleRead`, refreshes only the same
anchor and preserves signing journals. The live appeal exercised this path at
record 46. The late-beacon worker regression, authority handoff checks, TCP
agreement and bounded library check pass. The regression's earlier timeout was
a fixture clock before beacon availability; its corrected version first proves
a signable candidate exists. A deliberate payment while revoked left a saved
attempt. The UI proved its epoch exclusion without signing or deleting it by
hand, then allowed the fresh reviewed payment. The excluded archive is retained.

Evidence is under `target/validation-evidence/native-release-20261015/`:
`lifecycle-ui-completed/`, `lifecycle-ui-restart/`,
`burst-lifecycle-restored-payment.log`, `burst-lifecycle-ui-restart.log`,
`burst-lifecycle-reconcile-host.log` and `burst-lifecycle-reconcile-resumed.log`.
Earlier stale-read, fixture-clock, directory-setup and navigation failures remain
recorded there. Daemon SHA-256 is
`9066d018d4617676bb958937957f36393fb6d6d3be0b230a299b67ee2b9116e9`;
wallet SHA-256 is
`626febacdfd5facd4e97817168b8716518f75f92a44e691e5cfac69456fb86d6`.
This verifies these lifecycle steps on debug artifacts, not all of P3 or a
release candidate. P2-P5 and the remaining C01-C23 evidence stay open. Public
gates remain closed.

Execution reconciliation, 2026-10-15: source inspection of compact acceptance,
record retirement, incremental export and file-backed authority history agrees
with the retained no-snapshot retirement test and its passing historical log.
The completion plan's older instruction to implement that retirement was stale
and is corrected. No new runtime check ran and no capability became complete.
Resume P2's rendered consumer journey, then P3-P5 application, recovery and exact
artifact qualification. Those still block pilot distribution; the full C01-C23
requirements and public release gates remain unchanged.

P2 rendered journey implementation: the existing certified onboarding test now
drives creation, encrypted backup/restore, BRN payment and manual receipt forms.
Review/cancel and exact ledger assertions pass on the existing native binaries.
`burst-consumer-ui-host-selector-fixed.log` records the successful native journey;
its subsequent Rust report check failed because it expects a different schema.
`burst-consumer-ui-recovery.log` passes the corrected native-report verifier after
restarting all three nodes, without repeating onboarding or payments. The subject
remains verified with the same selection context; both payments are confirmed;
the recipient holds exactly 1.001 active TRST with zero expired, revoked or pending.
Logs and `consumer-ui-onboarding.json` are retained under
`target/validation-evidence/native-release-20261015/`. The first attempt's
accessible-name selector failure remains in `burst-consumer-ui-host.log`.
Wallet hash is `28ca6045a5c0cb71fbfdf4d7ec4bbea5f9b3f8da60ac78922109829d3d94a04e`;
daemon hash is `fc18d0e0cdae16114e3f7302b0ad4c3cdd01c477b5db2cd0b3fca5c9b90f0ba1`.
This completes those rendered steps on these debug artifacts, not P2 as a whole.
Panel voting and other command-only steps, independent installs, P3-P5 acceptance
and the full release blockers remain open. Shared UI timeout diagnostics exclude
page text so recovery words cannot enter failure logs.

Current P2 blocker: the next native voting run stopped on its startup payment
before exercising any panel votes. The native signer reported confirmation pending
for `c28d2e119c70dcf192f418d43338b111dc35fb13d5a2fc7b0802d5b3b7ff734e`.
After restart, node 0 returned block not found for that exact hash. No replacement
payment was sent. The run and attempted same-wallet recovery are retained as
`burst-consumer-ui-voting-started.log` and `burst-consumer-ui-voting-resumed.log`
in the evidence directory above, with `consumer-voting-node0.log`. The resume
check fails on the missing block; it does not establish permanent exclusion or
safe retry. Root cause remains undetermined. Resolve this C03/C18 admission and
recovery failure before treating P2 as reliable. Encrypted disposable backups
are preserved on the validation host under `consumer-ui.plDgol3y/` in the existing
workspace. Earlier successful journeys remain evidence for their own runs,
not proof against this failure. No public gate changed.

P2 assigned-voter UI now passes on the same debug binaries recorded above.
`burst-consumer-ui-voting-diagnostic.log` covers seven actual selected voters,
assignment discovery, exact subject/decision/stake review, cancellation without
an account-head change, and confirmed submission. The subject becomes verified,
restores its backup, pays through the UI, and the recipient receives exactly
1.001 TRST. Three nodes agree. `burst-consumer-ui-voting-recovery.log` verifies
the same selection context, confirmed payments and balances after another
three-node restart. Reports and logs are in the evidence directory above.
The test resumed the existing panel without funding or endorsing it again.
The preceding failures remain recorded: an assertion expected `500 BRN` instead
of the rendered `500.000000000000000000 BRN`; the next run timed out waiting for
the receipt. Read-only restart inspection found no votes on any node. Scoped
error/uncertain reporting and the existing 60-second native confirmation helper
replace that UI-only wait; protocol deadlines are unchanged. No earlier vote
timeout is counted as a pass.

The separate startup-payment failure above remains unresolved. Source inspection
shows RPC `accepted` can mean queued after its two-second wait, while the wallet
reports success only after confirmation. A certified epoch installation occurred
during the failed confirmation window; causation is not established. The new
diagnostic run did not reproduce that failure. It retains exact signed startup
request/response bytes in `consumer-startup-payment.jsonl`; the bounded proxy's
local test verifies lossless u128 forwarding and rejects oversized requests.
There is no retroactive capture of the original missing transaction. Endorsement,
participation, delegation and TRST transfer still use native commands rather than
their forms. Independent installation, other P3-P5 acceptance and full-release
requirements remain open.

C18 online BRN payment recovery is now connected to the existing payment journal.
The native `pay` command configures `NodeClient` to save the signed block and
bounded unconfirmed ancestry, then sync the attempted marker before `process`.
Its required stake-release batches use the same path. Confirmed blocks publish
their archive before releasing the reservation. Ambiguous responses leave the
draft available to the existing restart, canonical-slot and epoch-exclusion
recovery commands. Archive publication moved from the native layer to shared
wallet-core storage; there is no second receipt format.
`burst-online-payment-journal-local.log` verifies the draft/marker before the
mock server sees submission, malformed-response retention, confirmed archival,
and refusal to send when an existing reservation prevents publication.
`burst-online-payment-journal-regressions.log` passes the wallet-core library
suite, including existing ancestry and offline recovery checks.
`burst-online-payment-native-check-fixed.log` compiles native library/test code.
The earlier native check lacked an explicit toolchain and did not compile; its
failed log is retained. A new fixture's initial signing-helper argument-order
error was corrected before the passing core runs.
`--online-payment-recovery-only` now extends the existing native fault journey
to ordinary UI payment, lost accepted response, app restart and reconciliation
without resubmission. `online-journal-host.log` now records that native journey
passing on rebuilt wallet `9487be6db28a1681a4546e1705eb565302fe29da2c289cdc5171c351b0512f18`
and the unchanged daemon above. Cancellation leaves the account head unchanged;
one accepted submission loses its response; the durable draft blocks duplicate
signing and discard. After app restart, reconciliation finds the exact confirmed
hash and one-raw-unit pending receipt, releases the reservation and sends no
process request. The existing offline preparation/response-loss/restart journey
and interrupted archive publication test also pass. The log includes source
hashes and is retained in the evidence directory above. Disposable native data
is preserved on the host as `burst-native-wallet-oNQAof/` and
`burst-native-wallet-GRTYTm/` in its validation workspace.
This completes the online BRN accepted-response-loss journey on these debug
artifacts, not C18 or P2. Other online actions are not yet wired to this journal;
recovery labels still call online drafts offline payments. The original node-side
failure, independent installations and candidate qualification remain unresolved.

C18 online epoch-exclusion validation exposed a separate exact-number bug.
`block_info(include_signed_block=true)` put the signed `StateBlock` into a JSON
Value; balances above u64 caused `serialization should not fail: number out of
range` and a closed RPC connection. The native payment failed before saving or
submitting its draft. The first and diagnostic logs are `online-exclusion-host-first.log`
and `online-exclusion-host-diagnostic.log`; the latter identifies `block_info`.
The optional signed block is now exact JSON text, parsed directly by wallet-core
without passing u128 fields through JSON numbers. Nodes and wallets must update
together for this recovery field. The ancestry regression now uses balances
above u64 and passes preservation, restart and confirmed-conflict resolution.
`lossless-recovery-host.log` records that check and the rebuilt native run.
The serialization fix lets the online draft reach uncertainty, refuse premature
exclusion and duplicate signing, and submit the governance proposal. The run then
fails on governance vote `40e94dc9c98a8fb997f1482c40052d2306f8e5abe266946de95bedfae9172a72`.
`online-exclusion-node0.log` shows RPC queued at 16:57:30 UTC and admission rejected
at 16:57:34 with `block payment epoch is not installed`. This is direct evidence
of the queued-then-stale-epoch race, not proof about the older missing payment.
No vote was retried. The signed BRN draft and isolated native profile are retained
on the host in `burst-native-wallet-Zfhqss/`; proposal and draft identifiers are
in `consumer-ui.jjHl5fEL/native-online-exclusion.json.progress`.
Wallet hash is `35676732cd9fc27c47e76f3f34b189f075224119e3087eee15cf3e9f43541f5f`;
daemon hash is `0cf9aba63823aa2080d97ffe7f404b7f4b5b600519c32e1d60a5b9a29ee374b5`.
The full online exclusion journey remains failed. C03/C18 next work must cover
durable recovery of governance and other signed actions across epoch changes,
including content dependencies and explicit review before any replacement.
Do not solve this by accepting closed epochs or automatically signing retries.

C18 native action journal now covers all native signing clients, including
governance, verification, scoped delegation, appeals and automated receipt/merge.
Version 4 retains the signed block, bounded ancestry and any full proposal bytes.
Content must match the signed manifest before publication and when reopening.
Archival preserves the complete record; an interrupted or conflicting publication
cannot release the reservation. Versions 1-3 remain readable. Offline payment
signing/review stays restricted to its supported contexts; a generic recovery
record does not authorize resubmitting a governance action through that form.
Canonical-slot and certified exclusion checks can resolve signed wallet actions.
Epoch and governance-activation blocks cannot become wallet action drafts.
`burst-action-journal-core.log` verifies pre-submission publication for BRN,
governance votes and full proposals, uncertain-response retention, content
commitment checks and exact archival. `burst-action-journal-native.log` covers
native signer/review checks, archive interruption and process lease behavior.
The first core run failed because the future-version fixture still used version
4; it now tests version 5. The bounded size test now exceeds the content-aware
file limit. The native envelope test also now recognizes version 4 without
rounding u128 tokens; its first native failure remains in `action-journal-host.log`.
`action-journal-native.log` records an uncertain full proposal
`577d8771466b0f324bd5eedf7ccc7ab8a03a0b9bae013e1cde2b920df6aaa7ce`.
Unlike the earlier vote's stale-epoch admission rejection, this proposal was
stored but ordinary voting was fenced by a governance counting boundary.
`action-journal-recovery.log` passes recovery of its actual journal in a new app
session after all three nodes restart. Duplicate signing is refused; the node
installs certified epoch `29e92671a4d24f40ef898901f8bf292eaf8060c3ba209a4c03955a6b24d848ab`
and retires the unconfirmed proposal. Native exclusion then releases the
reservation and archives byte-identical content without any process request or
replacement signature. The original journal is retained on the host under
`burst-native-wallet-Fpp7AL/`; recovery data is `burst-native-wallet-l8usq6/`.
Wallet hash is `28e411f001207b8271d05eac712730882d172f2f31d2f854709c5ae82c4a8e51`;
daemon hash remains `0cf9aba63823aa2080d97ffe7f404b7f4b5b600519c32e1d60a5b9a29ee374b5`.
This completes that native proposal-recovery case, not C18. Generic recovery
labels and per-action qualification remain open, alongside P2-P5 and the existing
full-release requirements. `action-journal-complete.log` now passes the positive
online BRN journey on these binaries. After the dropped submission, normal
proposal/voting/activation closes the epoch. The original draft is rejected;
native exclusion archives it; a newly reviewed one-raw-unit payment confirms.
The verifier checks the exact canonical successor and one-unit spend delta on
all three nodes. This run's governance vote confirmed normally, so it does not
exercise the conditional vote-replacement branch. The actual uncertain proposal
recovery above is separate evidence. The report is
`consumer-ui.XyvZ0MDS/native-online-exclusion.json` on the host. Earlier failures
are not counted as passes. `action-journal-restart.log` passes the same canonical
successor/spend checks on all three nodes after restart, without another native
submission. Logs and the report are copied to the evidence directory above.

C18 recovery UI now distinguishes generic actions from offline payments. Native
status includes the block kind and whether payment review is supported. Generic
actions show confirmation checking without payment resubmission; exclusion does
not display success or open a new payment form. `burst-action-ui-remote.log`
passes seven mocked-IPC browser cases, including keyboard use and 320px layout.
The light/dark recovery screenshots were inspected. The shared route header
still says "Offline payment"; correcting that label remains open.
`burst-action-status-native.log`
compiles the native status changes and passes archive interruption/conflict
recovery. The missing ledger dependency was added. The desktop browser attempt
in `burst-action-ui-affinity.log` failed before assertions at the task ceiling;
the successful browser run used the existing validation host. No desktop limit
was raised. These checks do not complete C18 or P2-P5. Evidence is retained in
the same directory above; the earlier native artifact hashes do not cover this UI.

`burst-rendered-action-host.log` now passes the rendered native recovery of the
retained proposal after three-node restart. The test imports the disposable
identity through the wallet form, displays the exact pending hash, checks that
payment resubmission/discard controls are absent, and presses Check confirmation.
The current ledger resolves it as superseded, unlike the earlier exclusion run.
The UI reports the distinct outcome without showing confirmation or preparing a
new payment. The proxy observes zero submission requests; the archive matches
the original signed record byte-for-byte. The report and inspected screenshot
are `native-rendered-action-recovery.json` and `.png` in the evidence directory.
Wallet hash is `73552b24e09bf5f41ba440e6c7ca47a85cd50f8b537e4ca8bba45026bedd5e6b`;
daemon hash remains `0cf9aba63823aa2080d97ffe7f404b7f4b5b600519c32e1d60a5b9a29ee374b5`.
This completes that rendered supersession case, not all action recovery or P2-P5.

P2's native consumer journey now includes the TRST transfer form.
`burst-consumer-transfer-ui.log` passes wallet creation, encrypted restore,
initial receipt, seven assigned UI votes, verified accrual, BRN payment,
reviewed TRST transfer and receipt. Transfer review/cancellation leave the head
unchanged; an unfunded amount cannot open confirmation. The confirmed transfer
preserves its origin and original burn timestamp. All three nodes agree on the
selection context, confirmed transactions and exactly 1.001 active TRST at the
recipient, with no pending, expired or revoked balance.
`burst-consumer-transfer-restart.log` repeats those read-only checks after all
three nodes restart, without repeating transactions. `consumer-transfer-ui.json`
retains the report and provenance. Both logs are in the evidence directory above.
The wallet and daemon hashes match the rendered recovery artifacts above.
Encrypted test backups remain under `burst-native-wallet-XLDzcx/` on the host.
Endorsements, participation and delegation in this journey still use native
commands; independent installations and the other P2-P5 gates remain open.

The next P2 run replaces participation and endorsements with their rendered UI.
`burst-consumer-endorsement-ui.log` passes opt-out/opt-in and three reviewed
endorsements, followed by seven assigned UI votes, verification, restored-wallet
payment, TRST transfer and receipt. Each endorsement review shows the target,
336 BRN amount and permanent-burn warning. Cancellation leaves the account head
unchanged; confirmation increases total burned BRN by exactly 336 BRN.
`burst-consumer-endorsement-restart.log` passes the same three-node read-only
verification after restart, including both participation blocks, all endorsement
blocks, selected context, exact balances and transfer provenance. The report is
`consumer-endorsement-ui.json`; logs are retained alongside it. Artifact hashes
are unchanged from the rendered recovery run. Backups remain in the host's
`burst-native-wallet-P2gNFm/` directory. Pre-verification representative selection
and startup funding still use native commands. Independent installation and the
other P2-P5 requirements remain open; no release gate changed.

P3 numeric proposal publication and direct voting now pass through the native UI,
including cancellation without a signed block. `burst-governance-ui-settled.log`
records certified activation of `min_verification_age_secs = 2`, then native-command
publication of the full multilingual amendment, 21 direct/delegated ballots,
certified adoption and post-count delegation revocation without changing the tally.
`burst-governance-ui-restart.log` verifies the same parameter values, complete
constitution and revoked delegation on three restarted nodes. The report is
`governance-ui-settled.json`; artifact hashes match the rendered recovery run.
The verifier now reads expected governed values from the report, preserving legacy
report defaults. The test derives its voter count from the unchanged quorum and
current population instead of requiring exactly 52 verified accounts.

The preceding failures are retained, not counted as passes. `burst-governance-ui-host.log`
stopped after confirmed publication because an assertion expected snake_case
instead of the RPC's enum-style parameter name. `burst-governance-ui-resume.log`
refused to vote on that exact proposal after its window ended. The new reviewed
proposal in `burst-governance-ui-corrected.log` remained uncertain at a promised
governance deadline. Its hash is
`3b260723580eaa09a150cea11dcea6e95f721d860bb8403c2bf68f7d0db09a8c`.
`burst-governance-ui-recovery.log` passes rendered certified exclusion after
restart, exact archival and zero resubmission. Its report is
`governance-ui-excluded-recovery.json`; node evidence is
`governance-ui-pending-node0.log`. Only after this recovery did the settled run
publish its new proposal. No deadline lock was bypassed. These files are in the
evidence directory above. Constitution/delegation form qualification, remaining
P2-P5 acceptance and the full release requirements remain open.

P3 constitution-editor qualification remains unverified. The certified journey
now enters and reviews the complete Unicode article through the editor and checks
that cancellation does not sign. Its first run stopped earlier, on numeric proposal
`e26db6952ce3de1a8b216a2023173636635d9b3fe795e6f53bdd0a98d6b7649b`,
which returned confirmation pending. Node 0 logged RPC queuing at 18:25:58.007
and acceptance at 18:26:28.474 UTC. Acceptance is not finality. After restart,
rendered recovery confirmed that exact action, archived its original bytes and
made zero resubmissions. This completes that recovery scenario only. Logs
`burst-constitution-editor-ui.log`, `constitution-editor-pending-node0.log`,
`burst-constitution-editor-recovery.log` and report
`constitution-editor-confirmed-recovery.json` are in the evidence directory above.
Wallet and daemon hashes are unchanged. Diagnose the C03/C18 startup/admission
delay before another happy-path run; the delay's cause is not established.
The editor, delegation forms, remaining P2-P5 acceptance and full release gates
remain open. Desktop syntax checks passed under the resource wrapper.

C03/C18 startup handoff inspection found duplicate preparation: compact startup
publishes recovered state, then constructs the checkpoint installer with
`last_complete = None`. The installer now receives the recovered height/epoch
only after startup commit, force-sync and runtime publication. Its existing
tip/epoch equality check still forces preparation when authority advances.
Debug timing now separates checkpoint preparation, authentication, admission
locks, durable commit and publication. `cargo check --locked -p burst-node --lib`
passed through `tools/limited-run.sh`.

The handoff change now ran on the retained three-node ledger. Daemon hash is
`fe42f935d776cae5a8751ab56aa33885d858bb4bf51baa1bbc2d482a59c3ccfe`;
the wallet hash is unchanged. `burst-startup-handoff-ui.log` records a pending
proposal despite 755 ms admission. `startup-handoff-pending-node0.log` identifies
the remaining blocker as a durable governance-deadline promise, not admission
latency. After restart, checkpoint 31 installed and rendered recovery proved
exclusion, exact archival and zero resubmission. Its preparation took 54,032 ms.
Evidence is in `burst-startup-handoff-recovery.log`,
`startup-handoff-recovery-node0.log` and `startup-handoff-excluded-recovery.json`.

After that resolution, `burst-startup-handoff-settled.log` passes numeric UI
publication, voting and certified parameter activation. Checkpoints 32 and 33
advance normally; their timing is in `startup-handoff-settled-node0.log`.
The run then fails safely before signing an amendment because the editor's
polled rules predate that activation. The editor now fetches fresh context for
each review, preserves the exact revision check at publication, and blocks
duplicate review requests while loading. Bounded TypeScript checking passes;
the updated editor has not yet run natively. Progress is retained in
`startup-handoff-settled-progress.json`. Next finish editor acceptance and address
the measured checkpoint preparation delay. These files are in the evidence
directory above. P2-P5 and full release gates remain open.

C03/C18 replay-cache handoff has focused and live debug-artifact evidence. Startup
now returns the existing authenticated history with its committed runtime and
moves that cache into the checkpoint installer after force-sync. Cache reuse still
rechecks the base commitment and stored record observations; successor preparation
does not publish authority. The bounded library check passes. The focused
`startup_cache_reuses_verified_history_and_advances_without_publishing` test
was stopped during compilation under `burst-local-validation.service` (session
28220). Its memory-high counter reached 394,182 and memory-pressure stalls were
about 24% over the preceding minute. Only the owned service was stopped; no limit
was raised. Its exit status is not a test pass. The same focused test passed on
the existing validation host: no proof refetch for the recovered epoch, replay of
an accepted successor, and unchanged installed authority. Daemon/wallet builds
also passed. Live checkpoints 34 and 35 used the cache and installed, with
preparation still taking about 36 seconds each. Performance remains open.

The native run passed numeric UI proposal/vote/activation, then published the
full Unicode amendment through the refreshed editor. It checked exact review
text, cancellation without signing, confirmed publication and exact operations.
Twenty direct/delegated ballots confirmed before the next ballot was rejected
because the governed Test voting window had closed. This is not a full adoption
or restart pass, and the valid deadline rejection must not be removed. The
subsequent outcome check below resolves this amendment without another proposal.
Evidence is `burst-cache-editor-host.log`,
`cache-editor-node0.log` and `cache-editor-progress.json` in the evidence directory.
Wallet hash: `626febacdfd5facd4e97817168b8716518f75f92a44e691e5cfac69456fb86d6`.
Daemon hash: `d374c6abbf5c8622585974fde703f42211e73d24ffdb50f2fc2dec482255bcab`.
Amendment: `870870ec1d6a2bdc8885fa7d09c7886ba97a30ab69edbd08712585ba042d7fd8`.
P2-P5 and full release gates remain open.

P3 amendment outcome recovery: all three nodes report this exact amendment as
Activated with `phase_projected=false`, 20 yes votes and zero no votes. The first
180-second observation stopped while checkpoint installation was still running;
it is not a failed quorum or a passing recovery check. The next observation
completed installation at height 37 and found the certified outcome on all nodes.
Evidence is `burst-amendment-outcome.log`, `burst-amendment-outcome-catchup.log`
and `amendment-certified-outcome/` in the evidence directory above.

The native governance journey can now resume an adopted amendment from its
retained progress file. It checks the proposal IDs, proposer, exact Unicode
operations, certified tally and current governed parameter before revoking the
existing delegation. It does not repeat proposal publication or ballots, and
skips revocation if the confirmed entry is already revoked. Activation waits
require `phase_projected=false`. Scheduled entry into voting can remain projected
until an admitted transaction advances durable state. Bounded JavaScript syntax checking passes.
The native resume run passes exact article text at constitution version 3,
confirmed delegation revocation at generation 2, unchanged tally and agreement
across three nodes. Its report and executed driver are in
`governance-adopted-resume/`; the earlier 21-voter attempt remains partial.
This completes adoption and post-count revocation for this amendment on the
recorded debug binaries. Restarting all three nodes also preserves the exact
document, parameters, tally and revoked entry. Evidence is
`burst-governance-adopted-restart.log` and `governance-adopted-restart/`.
No proposal or ballot was resubmitted during either recovery run. P2-P5, installation
qualification and the full release gates remain open; delegation here still
uses native commands rather than its rendered forms.

P3 proposal-scoped delegation forms now have live debug-artifact evidence across
the original run and its resumed completion. Grant, secondary ballot and revocation use the existing
forms, check review content, cancel without changing the account head, and require
one confirmed block for submission. The setup proposal chooses a 180-second Test
voting window when the current window is shorter, through normal governance.
Public defaults and the planned direct-voter count are unchanged.
The first run exposed an over-strict test condition introduced in the previous
phase-wait change: requiring a nonprojected Exploration phase prevented the test
from casting its first vote. Scheduled voting is intentionally projected by the
RPC; only final activation must be certified for this assertion. That condition
is corrected. The stopped proposal
`59a9efc9f8c16ead83c5190f7d83dc6dd1725b1767707cc222adf9f64192dccf`
is certified Rejected with zero votes on all three nodes. Evidence is
`burst-governance-delegation-ui.log`, `delegation-ui-projected-phase/` and
`governance-window-rejected/`. No protocol rule was changed.
The corrected run certifies the 180-second Test window through proposal
`f61eb8e938a6ccc0077b601e6a5e223fa86f119a858b339e8e849022edc3887d`.
It passes UI grant and secondary-ballot review/cancellation/confirmation, then
confirms 20 direct votes. The native session reaches its 600-second limit before
adoption and revocation finish. The unchanged final-tally assertion is 21.
Read-only restart inspection subsequently finds the exact amendment
`2cdb03de607b6fc6fc7b0a652529e3ead3ddebdb118b1928b0dcdefb74dfb5fd`
certified Activated with 21 yes votes and zero no votes on all three nodes.
The earlier projected count of 20 was not the certified outcome. Evidence is
`burst-governance-delegation-ui-corrected.log`, `delegation-ui-corrected/`,
`burst-delegation-amendment-outcome.log` and `delegation-ui-certified-outcome/`.
The adopted-amendment resume passes revocation through the UI, including review,
cancellation without signing, one confirmed revocation, and the unchanged tally
of 21. All three nodes agree on the exact document, parameter and revoked entry
before and after another restart. Evidence is `burst-delegation-ui-resume.log`,
`delegation-ui-resumed/`, `burst-delegation-ui-restart.log` and
`delegation-ui-restart/`. Executed drivers are retained with the resume report.
This completes this proposal-scoped form journey on the recorded binaries,
not P3 or installer acceptance. The interrupted full run remains a timeout;
resumption does not turn it into an uninterrupted pass. The P3 pending-result
display gap is addressed below. Checkpoint
preparation still takes 42-49 seconds at heights 39-41 on these debug artifacts.
That performance gap and P2-P5 release gates remain open.

P3 provisional-result display: the wallet now carries `phase_projected` through
its proposal type and uses one phase label in lists, details and vote reviews.
Projected final outcomes say "Result pending"; details identify phase and counts
as provisional. Scheduled voting remains available, and certified outcomes keep
their final labels. TypeScript checking passes. The desktop browser check could
not start Chromium within the task limit (`pthread_create` failed); it is not a
test pass and no limit was raised. The same focused checks pass on the existing
validation host, covering projected and certified outcomes, voting review and
cancellation, delegation actions, wallet changes and mobile overflow. The captured
pending-result screen was inspected. Evidence is `burst-governance-projection-check.log`,
`burst-governance-projection-host.log` and `governance-projection/` in the evidence
directory above. These browser checks use mocked IPC; the native wallet binary
has not been rebuilt or qualified with this display change. P2-P5 remain open.

C03/C18 caught-up recovery no longer replays the same authority twice. Stage
timings on the retained ledger identified journal-authority replay as 23,777 ms
of a 43,328 ms recovery on node 0. When the accepted tip equals the authenticated
installed height, journal validation now uses that installed authority history.
Record observations still pin the head, bodies and beacons through publication;
the journal guard still validates signer promises. Accepted-ahead recovery still
authenticates its later decisions. No timeout, quorum or release gate changed.
The bounded library check passes. Focused host tests pass valid caught-up recovery,
changed-head refusal, corrupt-journal refusal without repair, later accepted
decision authentication and preservation of existing promises. The same three-node
restart verifier passes exact governance parameters, constitution, tally and
revoked delegation. Measured node-0 recovery is 18,868 ms after the change;
installed-epoch authentication remains about 17.6 seconds. This is one before/after
comparison on debug artifacts, not a general performance bound.
Evidence is `burst-recovery-stage-host.log`, `recovery-stage-baseline/`,
`burst-recovery-reuse-host.log`, `recovery-installed-tip-reuse/` and the matching
local check logs in the evidence directory above. Baseline daemon SHA-256 is
`e23d525634a540213756664b506fbced8556108aabff0aff64aae38199a6a91b`;
updated daemon SHA-256 is
`05f742d6b0ce10d5252e952ddfd9e71e1e4e25ddfcec7b4d8fd9eaa4b1b899a1`.
Repeated replay while installing a newer accepted checkpoint remains open.
This closes the measured identical-tip duplicate, not C03/C18 or P2-P5.

C03/C18 accepted-ahead recovery now starts at the authenticated installed state
and replays only its newer economic suffix for signer-journal validation. The
installed view keeps its original balances and epoch. Independent temporary
authority and observation files prevent either branch from changing the other;
certificate checks and publication-time input checks remain in place. The old
second recovery from genesis/archive was removed. Authority-file iteration still
authenticates the prefix; this is not a claim of constant-time recovery.
Focused host checks pass installed-state equivalence with a later local decision,
promise preservation, changed-head/journal/record refusal, independent authority
branches, damaged observation rejection and input changes during proof retrieval.
The rebuilt debug daemon also passes the retained three-node governance restart
verifier. Evidence is `burst-recovery-accepted-suffix-host.log` and
`recovery-accepted-suffix/` in the evidence directory above. Daemon SHA-256 is
`53e74d13ab83c187cadaddc28e890eed76cb1452ea67b24aee4db7482d84edc6`.
The desktop library check passed before the final source-path simplification;
the host compiled and tested the final revision. The desktop test compilation
was stopped at sustained soft-memory throttling and is not a passing test.
Live accepted-ahead installation timing, native pending-result display and exact
release-artifact qualification remain unverified. P2-P5 remain incomplete;
public release gates remain closed.

C06/P2 expired-selection recovery: the live worker now calls the existing compact
refresh path on timer ticks. Previously only driver tests called it. The native
Settings journey exposed repeated pre-beacon refusals and timed out before panel
selection. That run remains failed in `burst-consumer-representative-ui.log` and
`representative-ui-expired-selection/`. The randomness rule was not relaxed.
The new worker regression starts an expired unsigned candidate in a later round
and proves that the timer proposes a newer cut without installing economics.
It passes in `burst-compact-refresh-worker-fixed.log`; the earlier test compilation
error is retained in `burst-compact-refresh-worker.log`. Existing journal/late-prepare
priority and normal TCP agreement checks pass in `burst-compact-refresh-native.log`.

P2 representative selection now runs through Settings: invalid input signs
nothing, and valid input produces exactly one confirmed change while the wallet
remains unverified. The retained wallet resumed without funding or endorsing again.
Seven UI votes, verification, restored signing, payment and TRST transfer/receipt
passed on three nodes, then passed another restart with the representative and
exact balances intact. Evidence is `representative-ui-resumed/`,
`representative-ui-restart/` and `burst-consumer-representative-restart.log`.
The resumed native run did not log a timer refresh; the worker regression proves
that branch separately. Wallet SHA-256 is
`626febacdfd5facd4e97817168b8716518f75f92a44e691e5cfac69456fb86d6`;
daemon SHA-256 is
`472d343b4de6e8197057b3858b7091547978f8e44158fd8d826f11ed9c15b4b0`.
This closes the disconnected refresh path and rendered representative step,
not C06 or P2 as a whole. Independent installations, remaining P3-P5 acceptance
and full-release requirements remain incomplete; public gates stay closed.

Validation-host state, 2026-10-15: evidence is retained under
`target/validation-evidence/native-release-20261015/`. The task's SSH key and
`BurstOnboardingValidationTemporary` rule were removed; the original developer
rule remains. The host was subsequently restarted for profile-lease validation
when desktop available memory fell below 4 GiB. The ambiguous-submission recovery
batch finished and its temporary access was removed. The subsequent
epoch-exclusion native/restart/cold-observer batch also finished. Its temporary
`BurstOfflineValidationTemporary` rule and `burst-exclusion-validation-20261015`
authorized key were removed; Azure confirmed the VM is deallocated. The original
developer key remains. Retained Test data now includes the certified
verifier-balance change in `native-exclusion.json`; use the exclusion-mode
verification for that ledger rather than asserting the old parameter value.
The original developer access is retained. The local validation service is
absent; available desktop RAM remains below its required floor. Project data
and validation evidence are retained.

| Capability | Status | Verified evidence / remaining completion boundary |
| --- | --- | --- |
| C01 requirements and release evidence | In progress | Source hashes match all 388 indexed sections; assignments and acceptance scope are consolidated. Reproduction and behavioral sign-off remain. |
| C02 authenticated state/sync longevity | In progress | Component recovery covers 32,000 added accounts. Live-node fresh/resumed catch-up reaches height 130 with burns and delayed receives across record retirement, matching economics and restart. The latest independent-process cold catch-up passes in 78.87 seconds under the 90-second deadline. Timing reliability, full mixed-lifecycle longevity, maximum-budget combinations and remaining state/inventory limits are not qualified. |
| C03 deterministic atomic transitions | Partial | Startup migrates holdings and graph state to versioned rows after authenticated recovery; each migration converts its provisional journals atomically. Certified recovery reconstructs row journals; abort/reopen and selective graph rollback pass. Live nodes verify graph activation, restart and admit payments. Aggregate export, whole-state preparation and broader acceptance remain open. |
| C04 human finality and all forks | Partial | Gated identity/governance replacement exists. Signed-network journeys cover endorsement, proposal, delegation and ballot branches. The remaining block-kind, overlapping dependency and adversarial transition matrix is not qualified. |
| C05 BRN | Partial | Segmented accrual and stake/credit code; complete positive integration matrix. Launch rate resolved to 1/hour by owner. |
| C06 TRST and restoration | Partial | Lifecycle and provenance code; finish combined expiry/revocation/restoration/fork/pruning journeys. |
| C07 humanity onboarding | Gated | Controlled-time journeys exist; complete real-clock/application and concurrent lifecycle acceptance. |
| C08 challenges and appeals | Gated | Repeated appeal fixtures exist; full app/recovery/adversarial matrix remains. |
| C09 randomness/deadline recovery | Gated | Exact-round and delayed-beacon recovery exercised; production timing and adversarial scheduling remain. |
| C10 numeric governance | Gated/partial | Governed verification activation now passes live snapshot bootstrap, parameter-hash publication and restarted payment admission without resampling its selected panel. Full rule/activation/finality matrix remains. |
| C11 constitution | Gated | Content/diff paths exist; full native/content/recovery acceptance remains. |
| C12 delegation | Gated | Three-human journeys verify elected ballot replacement, opposite-arrival revocation and post-cut old-epoch revocation followed by successful retry, through certified counting and recovery. The remaining deadline/fault matrix and full native acceptance are not qualified. |
| C13 crypto/wire/work | Partial | Some choices historically signed off; current-revision independent positive vectors and integration evidence remain. |
| C14 concurrency/performance | Incomplete | Candidate admission/expiry encode changed holdings and graph rows and check their stored before-values. Detached maps still need comparisons; extending a parent scans its edge rows. Global engine locks remain. Full replacement retains an old-key set. Aggregate export, account-scoped preparation, lifetime bounds and selected hardware targets remain unproven. |
| C15 networking/self-hosting | Partial | Wire-33 signed transactions pass isolated Linux NAT/firewall/UPnP scenarios on the Azure validation host. Physical-router interoperability, hostile-network and distributed-quorum recovery qualification remain. |
| C16 recoverable pruning | Incomplete | Installed snapshots archive covered active checkpoint records without erasing their original bytes or delayed beacon proofs. This is active-set retirement, not disk pruning. Dependency-preserving policy and restoration after pruning remain. |
| C17 APIs/notifications | Partial | Implemented endpoints; full app-facing positive/reconnect/access-control contracts remain. |
| C18 consumer wallet | Partial | Real native payment evidence is narrower than all required wallet behavior; signer/offline/full-lifecycle acceptance remains. |
| C19 group trust | Partial | Any/all/graph checks exist; directory interoperability and complete real-ledger flows remain. Weighted scoring not a selected blocker. |
| C20 UI | Unverified as a whole | Existing design/UI work is not all-screen evidence. Required usability remains; decorative additions are optional. |
| C21 operations/platform release | Incomplete | Database open enforces schema/index compatibility and atomic initialization. Unsupported schemas are not falsely marked migrated. Full upgrades, fault qualification and exact current release artifacts remain. |
| C22 economic/security validation | Unproven | Protocol simulation is not equilibrium/incentive proof; analysis and bounded-community evidence remain. |
| C23 explicit future features | Future, not complete | Source-authorized later scope; no unsafe substitutes enabled. |

## Storage evidence

Logs and revision-specific source hashes are retained under
`target/validation-evidence/native-release-20261015/`. Holdings and graph rows
are active after authenticated startup recovery, following decision 34.14.
Each migration converts its provisional journals atomically. No C02/C03/C14/C16
capability or public release gate is signed off by the checks below.

| Requirement boundary | Verified behavior | Evidence logs |
| --- | --- | --- |
| Holdings migration and recovery | Historical expiry caches survive conversion; independent-holder rollback, abort/reopen and both authenticated recovery modes pass. Format markers cannot be undone by a block. | `trst-journal-migration-live-host.log` |
| Changed holdings writes | Reordered holdings, merge splitting, revocation/restoration, expiry extensions and independent appeal causes match full reconstruction. Unchanged portfolios are not read or rewritten; stale changed rows reject. Send preflight no longer changes live expiry caches. | `trst-row-delta-host-final.log` |
| Graph reconstruction and selective undo | Repeated edges, remerge exclusions and revision history survive reconstruction. Independent consumers survive successive rollback; orphaned downstream merges reject. | `trst-graph-records-host.log`, `trst-graph-records-sequential-undo-host.log` |
| Persisted graph ordering | Consumer/occurrence keys and separate ordering slots preserve unrelated journals through non-tail removal, reopen and later insertion. Mixed state and reordered surviving edges reject. | `trst-graph-slots-host.log`, `trst-graph-slots-expiry-host.log` |
| Graph migration and certified recovery | Historical edge positions are translated to installed slots. Replay graphs and provisional journals install together; abort, reopen and rollback pass. Missing journals leave no migrated rows or marker. Live nodes explicitly verify row activation, restart and admit payments. | `trst-graph-migration-host-final.log` |
| Changed graph writes | Candidate admission and expiry emit changed records. Repeated inputs, remerge exclusions, revocation and restoration match full reconstruction. An unrelated-row sentinel stays unread and unchanged; stale changed rows reject. Migration/rollback and live restart/payment regressions pass. | `trst-graph-delta-host.log` |
| Replay-derived graph journals | Recovery retains a shared graph view and emits changed graph records instead of serializing the full graph after each block. It stages fresh graph rows and matching journals in one transaction, including legacy databases. Storage rollback, both payment-recovery formats, live two-epoch restart/payment and strict node lint pass. A signed merge fixture verifies row-journal derivation, installation and rollback to the original graph in both database formats. | `trst-replay-graph-host.log`, `trst-replay-graph-merge-host.log` |

The broader `trst-replay-graph-snapshot-host.log` run found bootstrap assertions
still reading the removed aggregate holdings key. It was deliberately stopped
after these failures; it is not passing whole-suite evidence. The bootstrap
crash test completed before interruption. Later download/install results from
the interrupted process are not qualification evidence. Corrected assertions
compare reconstructed holdings and graphs rather than obsolete storage keys.
The new merge fixture initially attempted to merge as an unverified wallet,
then used a receive balance inconsistent with its predecessor. Both rejected
as required; the corrected fixture uses a verified wallet and signed cumulative
balances. Production checks were not weakened. C02/C03 remain open.

The corrected bootstrap batch passes with matching source hashes in
`trst-replay-graph-merge-host.log`: active panel/deadline preservation, settled
verification/rewards, archive download, base replacement, peer-proof installation,
restart/payment, late-confirmation rechecks and forced-sync-failure recovery.
Recovery now uses shared engine views and changed holdings records instead of
whole-wallet serialization. Graph and holdings records share journal-budget
checks, and both formats install with their journals in one transaction.
`trst-replay-holdings-host.log` passes with matching source hashes. It covers
both recovery formats, storage rollback, exact signed-merge holdings/graph
installation and rollback, live two-epoch restart/payment, bootstrap and live
snapshot installation, including late confirmation and sync-failure recovery.
Strict node lint passes. Full-state preparation and export remain unresolved.
The local attempt was deliberately stopped with the limits unchanged after
rustc read about 294 GB from disk while near MemoryHigh. It did not reach test
execution and is not passing evidence. Measurements are retained in
`trst-replay-holdings-local-resource.txt`, alongside the local build log.
Bounded aggregate replay maps and full mixed-lifecycle large-state acceptance
remain open. The signed merge fixture is not evidence for appeal/restoration
recovery at scale.

Large graph storage qualification (`trst-graph-large-host.log`): 70,000 merges
with eight inputs each encode to 73,920,064 legacy bytes, exceeding the old
64 MiB aggregate bound. Row persistence, two reopens and selective rollback
match streamed canonical hashes. One added merge journals four graph rows
(1,635 bytes); delta commit takes 644 ms and candidate preparation plus commit
812 ms on the validation host. The fixture takes 84.19 seconds and peaks at
1,125,624 KiB RSS, including multiple test-owned graphs. Source hashes match.
This verifies storage beyond the old bound, not signed-node lifecycle throughput,
bounded production memory or the Pi hardware targets.

Snapshot record integration (`snapshot-records-host.log`) passes with matching
source hashes and strict node/consensus lint. New exports bind manifest format 2
to body version 6. Holdings and graphs are sequences of key-bound records;
import feeds the existing validating builders without retaining aggregate
economic byte strings. Signed format-1/version-5 snapshots remain readable and
re-export to the same new-format state as independent replay. Tests cover large
integer amounts, duplicate records, governed-expiry mismatch, authority binding,
signed merge recovery/rollback, live two-epoch restart/payment, bootstrap,
peer-proof installation, late confirmation and sync-failure recovery.
The 64 MiB total-body ceiling remains. Record envelopes add wire overhead;
this is not increased-capacity or bounded-whole-state-memory qualification.
Other snapshot fields and transfer limits must be addressed together before
removing that ceiling. C02/C14 stay open.

Canonical map sorting (`snapshot-sort-host.log`) now spills maps above 4,096
entries into sorted temporary runs with 1 MiB raw-key batches and one run per
merge level. Redundant whole-map and sequence preflights are removed. Canonical
byte equivalence, cross-run duplicate rejection, truncated-run rejection and
writer failures pass, as do authenticated economic restore, live two-epoch
restart/payment and strict node lint. Source hashes match the tested files.
The 32,000 signed-account fixture exports 20,160,852 state bytes, resumes a
simulated interrupted transfer, installs authenticated history proofs and
reopens the database with startup metadata checks. It finishes in 97.02 seconds
with 516,912 KiB peak RSS for the test process. This is not a distributed
32,000-account network or production throughput measurement. Temporary storage
may be tmpfs; batches bound retained keys, not whole-process memory. Scratch
storage budgeting, full-state decoding, mixed economic lifecycle qualification
and the 64 MiB ceiling remain C02/C14 work.

Record-count preparation now uses checked structural counts instead of sorting,
cloning and encoding every row before the writing pass. The focused TRST check
(`snapshot-record-count-local.log`) compares counts with emitted rows through
remerge, repeated edges, revocation/restoration, independent appeal causes and
expiry changes. The node uses these counts for canonical and serde array headers.
Bounded node compilation passes in `snapshot-record-count-node-check.log`.
The combined counting/scratch-directory batch passes in `snapshot-scratch-host.log`
with matching source hashes and strict node lint. It covers canonical equivalence,
duplicate/error rejection, economic restore, live two-epoch restart/payment and
the 32,000 signed-account export/transfer/install/reopen journey. That fixture
takes 98.63 seconds and reports 539,592 KiB peak RSS; it does not establish a
whole-node performance improvement.

Disk-backed export now carries its staging directory through sort runs and
archive revalidation. Staged imports reuse their directory during canonical
comparison. Missing scratch directories reject before output; success and
writer-failure checks leave no temporary files. Byte-backed APIs and manifest-only
calculations still use the default temporary directory. This closes the selected
directory propagation gap, not scratch-space budgeting, full-state memory bounds
or the 64 MiB snapshot ceiling. C02/C14 remain incomplete.

The relevant TRST/node lint checks pass in those logs. Live journeys use
controlled authority and clocks; they do not qualify production quorum timing.
Changed-row checks are not a complete concurrent read-dependency protocol.
Extended-parent scans, detached-map comparisons, full replacement, aggregate
export and full mixed-lifecycle large-state qualification remain open, along with the other C01-C23
requirements.

Failed attempts remain evidence, not passes. The first row-delta run found an
old expiry test call missing its before-snapshot argument. The first graph
migration run deleted a printable hash key instead of the raw journal key;
the corrected fixture proves absence before testing migration failure. Earlier
codec logs retain the corrected large-enum Clippy failure.

Temporary access for the journal-conversion, row-delta, graph-records,
graph-slots, graph-migration, graph-delta, large-graph, replay-graph and
replay-holdings, snapshot-record, snapshot-sort and snapshot-scratch batches was removed. Their terminal
Azure checks confirmed deallocation; the original developer access was retained.

## C18 opening-payment recovery

Opening drafts can now resolve a confirmed first account slot through
`account_info.confirmed_open`, followed by a matching confirmed `block_info`.
The saved draft remains reserved if the node is old, the opening is unconfirmed,
or its account/previous position differs. The exclusion endpoint accepts an
unopened account only after authenticated installed epoch closure, with both
account absence and an empty canonical opening index read in that transaction.
RPC confirmation now checks the canonical account position as well as height;
a stale reverse-height entry alone cannot mark a fork block confirmed.

`offline-opening-wallet-local.log` verifies signed opening recovery and response
binding through scripted RPC responses. `offline-opening-rpc-local.log` verifies
canonical-position confirmation. `offline-opening-node-check.log` compiles the
node and its tests. Existing offline signing, file-corruption rejection,
exact-byte submission and successor recovery pass in
`offline-opening-wallet-regressions.log`.

`offline-opening-host-final.log` executes both certified exclusion fixtures,
including unopened-account absence, canonical-index disagreement and database
reopen. The real-node RPC journey signs and saves opening drafts, confirms either
the exact opening or a competing one, restarts the node and resolves the saved
files to the confirmed first slot. The competing-opening case then receives the
original pending payment and verifies the combined balance. Original draft bytes
and submission markers survive. Existing wrong-epoch admission/restart checks
and strict node/RPC/wallet-core lint also pass; source hashes match.
This first batch used a local Dev representative and wallet-core, not native UI
or a distributed quorum. The first host launch exited before its script upload completed;
`offline-opening-host.log` retains that failure and is not passing evidence.
The temporary SSH key and rule were removed, and the VM was confirmed deallocated.

The later `offline-native-receive-host.log` verifies the native receipt flow.
The wallet prepares a confirmed pending receipt online, then reviews, cancels,
signs and saves it with RPC disconnected. After an app restart, the daemon
accepts the saved opening but the proxy drops its response. The wallet retains
the uncertain draft and refuses to discard it. A second app restart resolves
the original confirmed opening without signing or resubmitting. The pending
source is consumed and the account has exactly one raw unit of TRST. The same
batch reruns the original offline burn journey and the real-node receipt/restart
test. Both use the existing durable draft and reconciliation paths.

`offline-receive-wallet-tests.log` covers wallet-core regressions.
`offline-receive-browser-final.log` verifies exact 18-decimal receipt amounts,
receipt-specific review, cancellation, duplicate-sign suppression and saved
receipt review. It also checks horizontal overflow at 390px and dark-mode 320px
and keyboard movement from Cancel to Submit. The 320px screenshot was inspected;
the address and payment ID wrap inside the dialog. These browser checks mock
native commands; the separate native run supplies the actual signing and node
evidence. Native binary hashes and matching source hashes are in the host log.
These are Linux debug validation binaries, not signed release artifacts.

The follow-up `offline-existing-receipt-native-final.log` passes the same native
journey for an already-open account. The fixture first confirms an opening with
two raw units, then signs a separate one-unit receipt offline. After acceptance,
response loss and two app restarts, recovery preserves the original opening,
confirms a `Receive` block extending it, consumes the intended pending source,
and leaves exactly three raw units. It does not re-sign or resubmit. The run
uses the same wallet and daemon binaries as the opening test; the updated native
test source hashes match the host. `offline-existing-receipt-native.log` contains
an SSH-access failure before execution and is not passing evidence.

C18 native opening exclusion now passes (2026-10-16) on installed wallet
`dea02586...` and daemon `76cd238f...`. The new branch of the existing certified
exclusion driver keeps an uncertain offline opening for an unopened wallet,
refuses duplicate signing, and submits ordinary governance to close the epoch.
The old opening is rejected; native resolution reports exclusion and archives
its journal. A separately authorized receipt creates one confirmed `Open` block,
consumes the incoming payment, and leaves one raw TRST unit with zero BRN burned.
The saved receipt, exact balance and activated governance state survive SIGKILL
and node restart. Evidence and input hashes are in
`target/validation-evidence/opening-exclusion-20261016/`; VM1 retained directory
is `opening-exclusion.D5nYtxpz`. This is installed WebKit/native-command acceptance
on a copied Test ledger, not full rendered recovery UI, multi-host quorum or app
restart coverage for this branch. The temporary node stopped; Dev is unchanged.

C18 remains incomplete. Other offline contexts, cross-account dependency loss,
bounded local history and cross-device signer coordination remain open in the
current plan. These receipt journeys cover unopened and existing
accounts, not every receipt lifecycle or platform. Release gates stay closed. The receipt
batch's temporary SSH key and rule were removed and its VM was confirmed
deallocated after validation.

Orphan recovery needs positive finality evidence. The current successor lookup
requires a confirmed predecessor. Rollback can remove the losing predecessor
and its height index, so a missing `block_info` or reverse index cannot prove
that a saved draft is permanently excluded. A recovery extension must bind the
saved draft to retained predecessor ancestry and a confirmed competing account
position, or to a certified closed-epoch proof that excludes that ancestry.
It must work after restart and must not release the reservation on missing or
unconfirmed history. This is C18 recovery work, not a reason to weaken the
current refusal or silently discard an uncertain payment.
The node's fork-candidate cache cannot supply that guarantee: finalized-position
cleanup deletes candidate payloads and content. A wallet-side recovery witness
must survive that cleanup and bind each predecessor hash to its signed account
and parent before comparing it with canonical confirmed history. Cache presence
alone is never finality evidence.

The wallet now captures signed unconfirmed predecessors during online
preparation and saves them with the signed draft in format 3. Formats 1 and 2
remain readable. `block_info` supplies the full signed block only when requested;
ordinary responses are unchanged. Capture stops at a confirmed predecessor or
the opening. Signature, hash, account and parent checks bind the saved ancestry
to the draft. Native reconciliation checks it for a confirmed competing account
position when the direct successor lookup cannot resolve the draft. A matching
ancestor, missing history or an unconfirmed competitor does not release it.
Archiving preserves the ancestry before releasing the account reservation.

`offline-ancestry-local-final.log` verifies preparation, disk round-trip, conflict
resolution and refusal of unrelated or unconfirmed responses. In
`offline-ancestry-host.log`, the live worker fork test prepares a draft on a
provisional branch, elects its competitor, removes the losing predecessor and
restarts. Wallet-core resolves the saved draft to the confirmed competitor while
the old predecessor body is absent. Election votes are injected at the worker's
authenticated-election boundary; this does not verify wire voting or an
independent quorum. The batch then stopped at a strict-lint style error.

After that correction, `offline-ancestry-host-final.log` passes strict wallet-core
and RPC lint, the archive interruption/evidence-preservation test, native builds,
and both native burn and existing-account receipt restart/lost-response journeys.
Source hashes match the host. Those native journeys use confirmed predecessors;
native UI recovery of an actual forked draft was not covered by that batch.

`offline-ancestry-wallet-regressions.log` passes the wallet-core suite for the
draft-format change. A final traversal change starts at the oldest saved slot
and stops on missing evidence instead of repeating a network timeout for each
ancestor. `offline-ancestry-ordered-host.log` verifies two saved predecessors,
unchanged ancestry that must remain unresolved, invalid replies, the live
fork/restart test, strict wallet-core lint and the rebuilt native burn journey.
Its source hashes match the final code. The earlier whole-suite run predates
this traversal change; the final focused run covers the changed behavior.
The ancestry batch's temporary SSH key and NSG rule were removed, and the
validation VM was confirmed deallocated after all processes finished.

Recovery ancestry is bounded to 256 blocks and 256 KiB per prepared context.
An oversized provisional chain refuses preparation and asks the user to wait
for frontier confirmation. This is an explicit wallet context limit, not a
ledger-history ceiling. Legacy drafts without saved ancestry may remain
unresolved. Cross-account dependency loss and sparse-history recovery still need
acceptance evidence; this does not complete C18.

The native fork subcase now passes in `offline-fork-native-uncertain.log`.
The real UI imports the disposable account, prepares and signs a draft on an
unconfirmed predecessor, and saves its signed ancestry. A native submission
command sends through a proxy that drops the process request. The wallet records
the attempt, retains the reservation and refuses discard. The live worker then
elects a competing predecessor and removes the losing branch. After both node
and app restart, the UI's Check confirmation action resolves the conflict,
archives the original draft and ancestry byte-for-byte, and releases signing
without resubmitting. A fresh preparation succeeds. The draft itself never
appears in the ledger. UI signing and recovery are exercised; the deliberately
dropped submission is invoked through native IPC by the fixture.

`offline-fork-native-host.log` and `offline-fork-native-host-final.log` cover the
unsent form of that journey, the shared native UI helpers and startup
synchronization. The final uncertain run has matching source hashes and retains
its public receipt under `native-fork-receipts/`. These Linux embedded-app runs
still use explicit worker-boundary election votes on one Dev node, not signed
network quorum or cross-platform qualification. No release gate is opened.

The certified-exclusion fixture's hard-coded format-2 extraction was updated to
read format 3 without rounding raw u128 values through JavaScript. Its isolated
envelope test covers formats 1-3 and maximum u128; it shares the dropped-request
proxy exercised by the native fork run. The full certified governance/exclusion
journey was not rerun in this batch.
Temporary SSH access was removed and the validation VM was confirmed deallocated.

The C18/C16 recovery lookup now has an atomic, body-independent RPC path.
`confirmed_account_slot` accepts `account` and `previous` and echoes both with
an optional `successor`. A zero predecessor selects the opening slot. The node
reads account finality, the current head and both canonical position indexes in
one LMDB transaction. It returns only a successor at or below the confirmation height;
unknown or unconfirmed positions remain unresolved, and inconsistent indexes
fail. The wallet binds the response to its saved account and predecessor and
rejects malformed or zero successor hashes. Only an explicitly unsupported
endpoint falls back to the earlier body-based lookup. This answer comes from
the configured trusted node; it is not a transferable finality proof.

`offline-indexed-slot-wallet.log` covers response binding, saved ancestry and
older-node fallback. The first host storage fixture tried to lower a confirmed
height, which the store correctly preserved. The fixture now starts with an
unconfirmed successor and advances finality. `offline-indexed-recovery-host.log`
retains the failed fixture assertion; it is not passing evidence. The corrected
fixture uses deliberately undecodable bodies to verify index-only lookup,
including opening/non-opening slots, another account's indexes, an unconfirmed
successor, reopen and head/index disagreement. This does not implement or verify
physical ledger pruning.

`offline-indexed-recovery-host-final.log` passes the corrected index-only fixture,
strict node/RPC/wallet-core lint, current native builds and the uncertain native
fork/restart journey through the new endpoint. A separate native receipt journey
accepts a payment, loses its response, restarts the app, then deliberately makes
every historical `block_info` request unavailable. The fixture first probes that
fault, then the UI resolves the original receipt through a confirmed-index query.
Its original opening and exact balance survive, the intended pending payment
is consumed, and recovery does not resubmit. Source hashes match the host.
This verifies unavailable-body RPC recovery with intact canonical indexes. It
does not prove that an actual pruning transition preserves those indexes or all
other lifecycle dependencies; C16 remains incomplete.
`offline-indexed-opening-native.log` also passes the opening receipt's
acceptance/lost-response/two-restart journey on the same binaries. The saved
receipt manifests for both native receipt modes are retained beside the logs.
The indexed-recovery batch's temporary SSH access was removed and its VM was
confirmed deallocated. No optimized desktop build was run.

## App release sweep: not yet

The production-prep sweep fixed defects, not the visual design. It does not
qualify all screens, supported devices or distributable artifacts.

A. Accessibility

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Should fix | Parameter load errors lacked announcement (`wallet/src/components/parameter-editor.tsx:61`) | `role="alert"` | Nonvisual users need the failure state | Fixed in code; screen-reader check open |

B. Performance

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Should fix | Every portfolio batch rendered (`wallet/src/pages/portfolio.tsx:119`) | 25-row pages | Avoid excessive DOM work without hiding holdings | Fixed rendering; full-response bounds remain open |

C. Mobile

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Should fix | Onboarding/toasts ignored device insets (`wallet/src/pages/onboarding.tsx:50`, `wallet/src/App.tsx:57`, `node-gui/src/App.tsx:37`) | Safe-area offsets | Keep controls clear of system UI | Fixed in code; hardware check open |

D. Forms

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Should fix | Representative change lacked Enter submission (`wallet/src/pages/settings.tsx:181`) | Form submit plus pending guard | Keyboard submission must work without duplicate actions | Browser contract passes |

E. Stability and states

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Blocker | Proposal summaries supplied incomplete details (`node-gui/src/pages/governance.tsx:146`) | Full-detail request, loading/retry and cursor navigation | Do not present absent detail fields as real values | Browser contracts and native cursor forwarding pass |
| Blocker | Route render failure could blank the dashboard (`node-gui/src/App.tsx:22`) | Reload boundary keeps navigation | Give users a recovery action | Browser failure/recovery contract passes |
| Should fix | Log polling bypassed request guards (`node-gui/src/pages/logs.tsx:45`) | Shared guarded hook | Prevent overlapping/stale page requests | Fixed; build passes |

F. Motion: no additional source-level defect confirmed in this sweep; real-device
reduced-motion and frame pacing remain unverified.

G. Theming

| Severity | Before | After | Why | Status |
| --- | --- | --- | --- | --- |
| Should fix | Stored appearance depended on app mounting (`wallet/index.html:10`, `node-gui/index.html:10`) | Local pre-mount appearance script | Prevent the wrong appearance before React loads | Browser pre-module check and embedded loading pass |

H. Content: no additional confirmed content/leftover finding. I. Marketing/SEO:
not applicable to this product-app sweep; website and deck are not covered.

Needs devices: check VoiceOver/NVDA reading and focus order, phone keyboard and
safe-area overlap, both themes on every route, and reduced-motion behavior.
No new product decision is required by these fixes. Larger unresolved selected
requirements remain C18-C21; they are not classified as optional UI polish.

Verification: frontend `npm run build` passes in both apps. Focused browser
contracts pass on Azure; the desktop browser attempt hit its unchanged task cap
and is not a pass. `validate-desktop-security.mjs` passes for both embedded Linux
apps. Wallet `tests/native-session.mjs --embedded` passes its Dev journeys.
Exact logs, hashes and limits are recorded below. This is not installer sign-off.

## Latest reconciliation

- C18 offline consumer flow now exposes prepare, offline review/sign/save,
  reconnect review/submit, confirmation reconciliation and unsent-draft discard.
  Review binds the saved hash, provider and full-precision raw amount. Read-only
  profiles cannot mutate drafts. A durable attempt marker is published before
  sending a saved block; preflight rejection leaves an unsubmitted draft
  discardable. Version-1 drafts have unknown submission history and cannot be
  discarded. Ambiguous responses keep the draft and account reservation.
  `offline-ui-integration-host.log` passes focused core/native checks, TypeScript,
  frontend build and browser contracts for cancellation, duplicate clicks, exact
  amount/hash binding, lost responses and read-only mobile layout. Desktop/mobile
  captures in the same evidence directory were visually inspected without
  clipping. `offline-ui-native-build-host.log` retains the initial JSX build
  failure, corrected before the final run. These browser contracts mock IPC;
  real embedded-app/daemon acceptance is being checked separately. No capability
  is complete or release gate opened by these checks. Cross-device authority,
  offline identity/governance contexts, opening-account recovery, bounded receipt
  retention and platform durability remain explicit C18 completion boundaries.
  The real embedded Linux app now passes the disconnected-payment journey against
  a disposable Dev daemon: review/cancel without signing, signing with the RPC
  proxy stopped, unchanged ledger head until submission, pending-draft exclusion,
  complete native-process close/relaunch, signer restoration while offline,
  full-precision review and confirmed one-raw-unit receiver receipt. The restart
  test also proves that keys were absent in the new process before restoration.
  `offline-native-process-recovery-host.log` records the pass; retained host data
  is `/tmp/burst-native-wallet-hu49qf`. Reproduce with
  `WALLET_BINARY=<embedded-debug-wallet> BURST_DAEMON=<dev-daemon> node
  tests/native-session.mjs --embedded --offline-only` from `wallet`, on a host
  with Tauri/WebKitWebDriver/Xvfb and existing native-test dependencies.
  Wallet SHA-256: `e41f23e7042e7157012ad8e02c4ff83b5273e65835833e9ea388b56bef39487f`;
  daemon: `0197a3bfd077e234f18a0411e20a6d94eced01b5a91df1b3f99ef00823da0225`.
  `offline-native-recovery-host.log` records the build, exact-amount core review
  check and earlier frontend-reload journey. The first harness incorrectly
  expected reload to remain unlocked (`offline-native-journey-host.log`). Its
  correction exposed a real dead end: the offline dashboard hid payment entry
  (`offline-native-journey-final-host.log`). A saved-payment recovery action now
  remains available in that error state. Process restart here is orderly, not
  power-loss/SIGKILL qualification; this is Linux Dev evidence, not a public
  multi-human quorum, installer or other-platform sign-off.
  The ambiguous-submission variant now passes against the real node: the proxy
  consumes an accepted process receipt, drops the response and observes exactly
  one submission. The native wallet retains the signed hash and durable attempt
  marker and refuses discard. After a second complete app-process restart, the
  UI reconciles the confirmed successor, finds the exact one-raw-unit receiver
  receipt and clears the reservation without signing or submitting another block.
  Evidence: `offline-native-ambiguous-recovery-host.log`; retained host fixture
  `/tmp/burst-native-wallet-LEmdEt`. Wallet hash is unchanged; updated daemon hash
  is `5ebca6001041b84915db3cff14e968205e364b54065cdec93dc2896453f7752f`.
  This verifies the lost-response/restart subcase, not all stale-draft recovery.
  Explicit remaining gap: an attempted draft excluded by a later certified epoch
  can have no confirmed successor, leaving its reservation unresolved. Recovery
  needs evidence of exclusion and permanent epoch closure, not merely a missing
  block or a different reported head. Include that positive path and admission
  refusal of the excluded old block before claiming complete stale recovery.
  Epoch-exclusion recovery is now implemented through a read-only node callback.
  One LMDB read snapshot checks installed authority, the permanently closed old
  epoch, and the unchanged canonical fully confirmed account frontier. Missing
  history, unknown closure and provisional frontiers cannot release a draft.
  The wallet binds the response to its signed account/previous/epoch, archives
  the draft before releasing the reservation, and distinguishes exclusion from
  confirmation in the UI. Sparse predecessor bodies may fall back to this index
  check; an RPC error alone never releases the reservation. This remains the
  existing trusted-node RPC model, not a transferable cryptographic proof.
  `payment-exclusion-core.log` passes exact response binding/refusal under the
  desktop wrapper. In `payment-exclusion-host-final.log`, the accepted-checkpoint
  installation fixture, reopen/refusal cases, native library check, TypeScript
  and frontend build pass. The aggregate command then failed because its
  temporary browser launcher was absent after host reboot; browser checks are
  separate, not inferred from that build. `payment-exclusion-host.log` retains
  the initial fixture failure: account storage correctly preserved a confirmed
  height instead of lowering it. The fixture now adds an unconfirmed next slot.
  Desktop node-test attempts were stopped for sustained soft-memory throttling
  and are explicitly NOT passes (`payment-exclusion-desktop-first.log` and
  `payment-exclusion-desktop-focused-status.log`). No limits were raised.
  `payment-exclusion-browser.log` separately passes the offline contracts,
  including exclusion displayed as not applied with no replacement signature.
  The native app/normal-node epoch-change journey and refusal to admit the
  excluded signed block remain the next acceptance boundary. No C18 or release
  sign-off follows from the component checks.
  That normal-node boundary now passes for an unchanged confirmed predecessor.
  The embedded native wallet saved a burn and retained its reservation after a
  dropped process request. A different verified account proposed and voted for
  a real parameter change. Certified activation closed the old epoch without
  changing the reserved account head. Ordinary RPC admission rejected the exact
  old signed bytes for their epoch. Native recovery archived the draft as
  excluded, released the reservation, and confirmed an explicitly requested new
  one-raw-unit payment. All three nodes agreed on the canonical successor and
  exact odometer delta. Hard restart and a fresh observer also recovered the
  receipt, proposal activation and governed value.
  Evidence: `payment-exclusion-native-host.log`,
  `payment-exclusion-native-recovery.log`, and `native-exclusion.json` in the
  evidence directory. The first aggregate run stopped after successful native
  recovery because the older harness expected the verifier-balance parameter
  to remain at its pre-test value. The exclusion-mode harness now verifies the
  new certified proposal/value and unchanged test accrual instead. The resumed
  run reused the original report/ledger without another proposal or payment;
  its fresh observer is `observer.gGCxYWk5` under the retained
  `burst-validation.native-VfdiIwnZ` directory. Other harness modes are unchanged.
  Wallet SHA-256: `048bdeb695a7298e9db22c425045092540926ada1a1896d93b9f9c9dd5d1bcab`;
  daemon: `ef3a85e650cb452dbf7f45899d435a0dbcb95d4b52b2b9451fcd97bed09c1f9f`.
  This closes this C18 recovery subcase, not the entire capability. It uses a
  disposable private-genesis Test ledger, three loopback nodes on one Linux host
  and delegated founder consensus authority; it is not independent-quorum,
  installation, power-loss or cross-device qualification. Opening/orphaned
  predecessor recovery, other offline operations, bounded receipt retention
  and the cross-device decision remain open. Public gates remain closed.

- C18/31.9 implementation increment: `OfflinePaymentContext` now captures public
  account state and precomputed work without signing or submitting stake-release
  acknowledgements. The existing transaction builder creates payments after
  disconnection. Reconnection validates the signed block, current frontier,
  epoch, parameters and work before submitting the exact bytes. Already-confirmed
  hashes reconcile without resubmission; ambiguous process receipts retain the
  signed hash. Neither stale-state handling nor retry signs a replacement.
  The RPC contract test actually closes its preparation listener before choosing
  and signing a payment, then checks exact submission, confirmed retry, stale
  frontier/epoch/parameters/work and lost receipt. Pure tests cover burn amounts,
  account opening, send provenance, signer mismatch and tampering. The wallet-core
  library suite passes under the unchanged desktop wrapper; evidence:
  `target/validation-evidence/native-release-20261015/offline-wallet-core.log`.
  Strict wallet-core library Clippy also passes (`offline-wallet-clippy.log`
  in the same evidence directory). The existing app receipt map in
  `wallet/src/hooks/use-transaction.ts` survives navigation only, not process
  restart; durable signed-draft integration must precede exposing offline signing.
  These are core/HTTP-contract checks, not daemon or consumer-app acceptance.
  No C18 completion claim: app review and durable pending-draft integration, actual
  daemon offline acceptance, identity/governance offline contexts and cross-device
  coordination remain. Context exports are node-provided state, not authenticated
  balance proofs; JSON must remain opaque to JavaScript to preserve raw integers.
  No new Tauri signing endpoint is exposed and no release gate changes.

  Durable core storage now writes a versioned signed-payment file through a
  synced temporary file and no-overwrite publication. Unix parent-directory sync
  completes publication; other platforms still need durability qualification.
  Loads are bounded and reject unsupported versions, corrupt signatures and
  non-regular paths. `submit_saved_offline_payment` binds the expected wallet and
  needs no keys. Its HTTP-contract journey drops the signing context and keys,
  reloads the saved draft, submits exact bytes, reconciles a confirmed retry and
  retains the draft after an ambiguous receipt. Storage tests preserve maximum
  merge/release lists and u128 amounts, verify no overwrite/private Unix file
  permissions, and reject truncated, tampered and oversized data. Final focused
  tests and strict library Clippy pass under the unchanged wrapper:
  `target/validation-evidence/native-release-20261015/offline-draft-recovery-final.log`.
  This is file reload/contract evidence, not a killed-app or power-loss test.
  No app endpoint is exposed yet; it must reserve one pending account position,
  block conflicting automation/signing and provide review/recovery before this
  core facility can satisfy the consumer workflow. Full C18 remains partial.

  Native integration now registers preparation, offline signing/save, pending-hash
  lookup and exact saved-payment submission commands. Startup initializes a
  private app-data draft directory. All existing manual/automation transaction
  paths obtain keys through `signing_keys`; that shared helper now refuses keys
  while the account's pending file exists, including corrupt draft files. The
  offline commands hold the same signing mutex and stop automation. Successful
  confirmed submission archives the signed record before removing its pending
  reservation; failed/uncertain submission leaves the draft in place. Native
  library `cargo check` passes, and focused native tests verify reservation
  persistence across wallet-state reconstruction, unrelated-account isolation and
  fail-closed storage errors. Evidence: `native-offline-reservations.log` in the
  same evidence directory. Resource wrapper stayed at 3 GiB/64 tasks/one CPU.
  This is not native IPC or rendered-flow acceptance. UI wiring, safe retirement
  of superseded drafts, independent-process profile exclusion and real daemon
  acceptance remain before the offline workflow is usable. The mutex plus disk
  check does not coordinate separate processes/profiles or devices; do not label
  it the decision-31.7 cross-device solution. No public release gate is opened.

  Same-profile process coordination now uses an OS file lease held for the app
  lifetime. A second process retains read access but cannot obtain signing keys
  or mutate pending-payment recovery. The lock file is never unlinked; process
  exit releases the OS lease. Focused Azure tests verify separate-process
  exclusion, read-only wallet access, normal release and acquisition after forcibly
  killing a test-owned holder. Evidence: `native-profile-access-recovery-host.log`
  in the same evidence directory. The desktop wrapper refused validation below
  its 4 GiB availability floor; nothing bypassed that refusal. This verifies Linux
  lease behavior/native guards, not two rendered wallet instances, other platforms
  or coordination across different profiles/devices. UI and stale-draft recovery
  remain; retirement must use confirmation evidence anchored to the signed draft,
  not simply clear a reservation when an arbitrary node reports a different head.

  Stale-draft recovery now follows the saved predecessor's confirmed successor.
  `block_info`/`blocks_info` expose an optional successor from canonical account
  indexes only when the next position is confirmed. Lookup is bounded and does
  not scan/load the entire account history. An account not yet present returns no
  successor rather than making its unconfirmed block unqueryable. Wallet recovery
  verifies the anchor's account/confirmation and the successor's exact predecessor,
  account and confirmation. It reports confirmed versus superseded distinctly;
  missing/old-node history or inconsistent responses do not release the draft.
  The native command archives the signed record before releasing its reservation,
  with retries preserving an already-published identical receipt and rejecting
  conflicting receipts. Focused RPC, HTTP-contract and native archive tests pass
  on Azure: `offline-successor-recovery-final-host.log` and
  `offline-successor-rpc-final.log`. The initial RPC test failed on an invalid
  fixture address; `offline-successor-recovery-host.log` retains that failure.
  This is not yet normal-daemon/rendered-UI acceptance. Opening-account drafts
  lack a predecessor anchor and remain explicitly unresolved by this recovery
  method; their network-bound recovery still needs implementation. No C18 sign-off
  or public gate change follows from these focused checks.

- C18 signing architecture reconciliation (2026-10-15): decisions 31.7 and
  31.9 require one active signing device and fully offline transaction creation.
  `WalletState::signing` is a process-local mutex, not cross-device exclusion.
  `PreparedWork` contains only a root and nonce;
  `submit_signed_with_prepared_work` still fetches account state, release batches,
  parameters and (for endorsements) the target frontier before signing. It is
  not an offline-signing implementation. Pure block builders exist and should
  be reused, rather than introducing a second transaction implementation.
  No requirement is complete from this inspection; no runtime behavior changed.
  Before implementing device coordination, resolve whether normal handoff must
  require the old signing device to relinquish control, or whether a restored
  wallet must remotely supersede an unavailable/offline signer. The latter
  cannot prevent the disconnected device from creating signatures with its
  existing key; rejecting its later submissions requires network-visible
  authority/fencing rules, not a local flag or an independent node-local lease.
  Governance delegation is not spending authority and must not be reused for
  this purpose. Acceptance must include two actual app instances, offline
  signing on the active device, watch-only operation on the other, handoff,
  lost-device recovery, stale-frontier/epoch handling and ambiguous submission
  recovery without silently signing a replacement. This is an explicit C18
  product/protocol decision boundary, not optional polish or a new release gate.

- C10-C12/P3 native governance now passes numeric activation, adoption of a large
  multilingual constitutional article, scoped delegated voting and subsequent
  delegation revocation without rewriting the certified tally. Exact content,
  parameter and delegation state survive three-node hard restart and fresh
  observer recovery (`observer.div4PE0W`, `/tmp/burst-native-full-recovery.log`).
  Cold recovery waits for the proposal and the later revocation separately;
  neither arrival is evidence that the other has arrived. Evidence and limits are in
  [TESTNET_PILOT.md](TESTNET_PILOT.md). Full UI, installation and broader fault
  qualification remain open.

- C02/C12/P4 mixed snapshot recovery exposed a missing dependency path. The
  adopted constitution recovered, but a later revocation remained unapplied:
  its proposal body was absent from the sparse snapshot. Historical finality
  authorized the age exception, then ordinary dependency validation parked the
  block. `snapshot-runtime-5BX1Ef` and its debug logs retain the failure.
  A bounded worker now connects this gap to existing authenticated ancestry
  proofs and restores covered bodies/indexes without replaying their economics.
  Authority, snapshot commitment, confirmed position and retained-index conflicts
  are checked before commit. Index/cache publication is serialized with normal
  commits and checkpoint replacement. The final revision passes the focused
  foreign-proof/missing-proof/index-conflict/idempotence/economics test and the
  mixed lifecycle/governance snapshot journey: exact state after installation,
  hard restart, reconnect and a new confirmed payment. Evidence:
  `/tmp/burst-snapshot-dependencies-publication.log`, `snapshot-runtime-305Yr2`.
  Daemon SHA-256:
  `0197a3bfd077e234f18a0411e20a6d94eced01b5a91df1b3f99ef00823da0225`.
  This closes the reproduced sparse-snapshot dependency failure, not all C02
  longevity or distributed-quorum fault qualification. Public gates stay closed.
  The broader snapshot-history selector also included the expensive live crash
  matrix. It passed bootstrap/download/record/replacement crash checks, then hit
  its ten-minute batch limit during cold retired-history catch-up. Cargo exited
  with 124; its remaining test process was explicitly stopped. This aggregate
  run is incomplete, not a pass (`/tmp/burst-snapshot-dependencies-regression.log`).
  A second component batch reached its three-minute cap during record rollover
  and exposed one invalid pre-bootstrap selection fixture. Its failure is
  reproduced in `/tmp/burst-bootstrap-arrival-fixture-before.log`. That fixture
  now starts from a certified roster snapshot instead of a forbidden committee;
  the threshold, three identity arrival orders, recovery and rollback assertions
  remain. The corrected focused test passes in
  `/tmp/burst-bootstrap-arrival-fixture-fixed.log`, followed by strict node-library
  Clippy. Neither incomplete aggregate
  is counted as a pass; no production guard or deadline was relaxed.

- C18/C20 app defects fixed: node governance fetches complete details with
  retry and cursor navigation; render failures have reload recovery; logs use
  the shared guarded request hook. Wallet portfolios render 25 entries per page,
  representative forms submit with Enter, and parameter errors are announced.
  Both apps restore appearance before mounting and include toast safe-area offsets.
  Frontend builds pass. Synthetic-IPC browser contracts pass on Azure in
  `/tmp/burst-node-release-states-host-fixed.log` and
  `/tmp/burst-wallet-release-ui-host.log`. The desktop browser attempt hit the
  unchanged process cap and is not a pass. The strengthened 51-entry paging
  check also verifies that the full balance stays visible on the last page
  (`/tmp/burst-wallet-portfolio-total.log`, pass). Native cursor forwarding now passes
  through Tauri/WebKit and a local HTTP contract fixture. Both embedded apps
  enforce CSP: bundled assets, component positioning and native IPC work, while
  injected inline scripts and browser network requests are blocked. Development
  hot-reload allowances are separate from the embedded policy. Evidence:
  `/tmp/burst-embedded-desktop-security.log` and the saved JSON/hash reports.
  The embedded wallet also passes its existing native Dev journeys: backup and
  restore, onboarding, signer binding, review/cancel/payment/receipt, return,
  automation cancellation, pending pagination and group-policy outage behavior.
  Evidence: `/tmp/burst-embedded-wallet-journey.log`, `burst-native-wallet-6R6bGW`.
  Wallet SHA-256:
  `58e4bb964cf33cb1375a9b35217ef493f51026c89d0e566a1c26a1fd45125e04`;
  node-app SHA-256:
  `65cee5d1b261d459ce6ad54395d1039c7b81399de00ffdf10327d929d44e515e`.
  These are embedded debug builds on Linux, not signed installers. Whole-app
  screen-reader/device checks, portfolio response bounds, independent installs
  and the remaining selected wallet functionality are still open.

- C08/C06/P3 native challenge and appeal commands passed on the retained Test
  ledger. The challenged wallet could still pay before judgment. Revocation
  invalidated its originated TRST without touching unrelated funds; an admitted
  appeal kept the restrictions until certified reversal. Reversal restored the
  recipient's funds and the wallet could pay again. Exact status and balances
  survived a three-node hard restart. Fresh-observer confirmation initially
  exceeded the cold-join deadline. With signature arithmetic optimized in the
  development profile, the same ledger passed empty-observer recovery in about
  75 seconds, including exact balances and final confirmations. Evidence is
  `/tmp/burst-native-certified-lifecycle.log` and
  `/tmp/burst-native-certified-lifecycle-recovery.log` and
  `/tmp/burst-native-lifecycle-optimized-recovery.log`. Recovery resumes the
  already signed appeal, without repeating votes or staking again. The native
  constitution/delegation journey is recorded above; full UI acceptance is open.
  The snapshot runtime scenario also passes after its first check exposed different
  genesis defaults between node and snapshot commands. Shared configuration
  resolution now applies the same network defaults to both. The normal binary
  attests/exports/verifies the archive, rejects altered bytes, installs it in an
  empty node, survives a hard restart and reconnects after source loss. Exact
  lifecycle state/balances and a new payment pass on both nodes. Evidence is
  `/tmp/burst-native-snapshot-runtime-final.log` and `snapshot-runtime-rAy7Qr`.
  This is a loopback, single-representative Test topology on the validation host,
  not distributed-quorum or release-artifact qualification.

- C07/C18/P2 native consumer signing now passes against normal Test daemons.
  A new wallet is created and backed up, receives paid endorsements, discovers
  its panel, receives seven native votes, becomes verified, restores its key
  and spends accrued BRN. A second wallet receives both the payment and a TRST
  transfer. Unfunded, disconnected and locked spending attempts fail. This
  closes the native-command journey, not full click-through UI or platform
  distribution acceptance. Exact artifacts and evidence are recorded in
  [TESTNET_PILOT.md](TESTNET_PILOT.md). Challenge/appeal and governance recovery
  results are recorded above; public gates remain closed.

- C03/C06/P1 normal onboarding exposed two protocol integration faults, not
  additional scope. Endorsement start times depended on arrival order, so
  installed state could differ from authenticated replay. The shared transition
  now derives the start from the earliest accepted timestamp; the reversed-order
  reproducer and endorsement regressions pass. Selection discovery also admitted
  panels before the required bootstrap exit, while installation rejected them.
  Replay and discovery now enforce that requirement before certification.
  The normal daemon now passes the specified 50-wallet bootstrap, paid
  endorsements, certified beacon selection, seven signed votes, verified
  accrual/payment, hard restart and empty-observer catch-up. The validator
  compares unordered endorsement/vote lists without discarding their fields,
  and waits for cold-join payment recovery before comparing verification state.
  This closes P1's debug-artifact network scenario, not native-app acceptance
  or qualified public artifacts.
  Failed runs, artifact hashes and acceptance boundaries are recorded in
  [TESTNET_PILOT.md](TESTNET_PILOT.md). No release gate has opened.

- C10/P3 normal Test governance activation now survives a hard restart and
  fresh-observer recovery. The signed proposal/vote and activated eligibility
  parameter agree across nodes; initial accrual remains 1 BRN/hour. Exact
  evidence and the corrected validator's earlier failure are recorded in
  [TESTNET_PILOT.md](TESTNET_PILOT.md). This completes that scenario, not all
  governance or public-release acceptance.
- C06/C17/C18/P1/P2 had a concrete application gap: verifiers could not opt in,
  discover their assignments or cast panel votes from the wallet. Those paths
  are now wired through existing signed transactions and current panel state.
  Assignment-page, browser interaction and native review checks pass, with
  evidence and boundaries recorded in the pilot ledger. They do not prove a
  native-to-live-node verifier journey. Full real-clock certified onboarding,
  lifecycle recovery and qualified installable artifacts remain blockers.

- P5 private bootstrap ownership now has an explicit build mode. The Test
  public key is pinned at compilation; runtime signing requires its matching
  creator seed. Normal-daemon restart checks reject absent and wrong seeds,
  accept the matching fixture seed, and keep Live gates closed. Ledger identity
  checks still pass. This completes the ownership wiring, not pilot sign-off;
  a real creator-owned key and qualified artifacts remain open. Evidence and
  fixture artifact hash are in [TESTNET_PILOT.md](TESTNET_PILOT.md).
- Pilot P1 now has opt-in, network-aware checkpoint activation through normal
  signing, persistence, installation, recovery and RPC paths. Normal-library
  policy checks and the actual debug daemon's isolated Dev/Test/Live restart
  checks pass; Live stays disabled. Commands and evidence boundaries are in
  [TESTNET_PILOT.md](TESTNET_PILOT.md). No capability or pilot row is complete.
  P3 lifecycle activation is also wired for experimental non-Live builds.
  Normal-daemon payment, delegation, governance submission/withdrawal,
  hard-restart and fresh-observer journeys pass. After waiting for authority
  convergence, the minority pending-chain recovery also passes without
  resubmission or automatic receive. Exact artifacts and evidence are recorded
  in the pilot ledger. This completes that scenario, not the full recovery gate.
  Real-clock certified onboarding, full P3 lifecycle/application journeys,
  recovery acceptance and qualified artifacts/bootstrap still block distribution.
- The pending C02 frozen-receipt change passed six restoration checks in
  `/tmp/burst-shared-frozen-receipts.log`, including shared candidate storage,
  legacy encoding equivalence and rejection of altered frozen inputs. This is
  component evidence only. Further memory optimization is paused for the pilot.

- C02/C03 recovery observations and successor references now share digest-checked
  temporary-file storage instead of lifetime vectors. Export, cache refresh,
  installation and journal guards
  stream the same checks. Cache refresh retains retired bodies and active-key
  absences without a historical key set. Successor target selection checks the
  entire stream before retaining the requested prefix, and replay reloads one
  exact observed body at a time. These two storage gaps are closed;
  retained selections, active inventories and whole-state
  collections still prevent a general bounded-memory claim. No pruning or
  authority rule changed, and all four release gates remain disabled.
  Evidence: `/tmp/burst-streamed-observations-component-final.log` covers a
  131,073-record stream, filtering, damaged/truncated/appended files and existing
  inventory/atomic-retirement checks. The `cache`, `export`, `stored` and `live`
  logs with the same `/tmp/burst-streamed-observations-` prefix cover historical
  corruption rejection, late beacons, recovery without a source prefix and live
  two-epoch restarted payments. `retired-live.log` passes height-130 live catch-up,
  archived-byte preservation, balances and restarted payments in 139.95 seconds.
  `journals.log` passes installed-target, signer-journal and changed-proof-delivery
  checks; `path.log` passes later authenticated-height handling.
  `lint-final.log` passes strict node lint. These are development checks, not
  full longevity, crash-matrix or release qualification.
  `/tmp/burst-streamed-successors-component.log` adds 131,073 successor references,
  exact-prefix selection, missing-target rejection and ordered cursor replay.
  The `stored`, `cache`, `journals`, `live` and `lint` logs with the same
  `/tmp/burst-streamed-successors-` prefix pass exact-body replacement/removal
  checks, late-beacon and historical recovery, installed-target selection, live
  two-epoch restarted payments and strict node lint on the shared implementation.
  `cold-live.log` passes independent-process genesis catch-up across retired
  history to height 130 in 78.87 seconds under the unchanged 90-second readiness
  deadline, then checks balances, graceful shutdown and restarted payments.
  The full journey takes 220.70 seconds. `export.log` and `path.log` pass historical
  export and later authenticated-height handling. The cold run survived a chat
  interruption; its existing process was observed, not replaced. This is one
  passing cold run, not timing-reliability or distributed-network qualification.

- C02/C03 compact signing compares retained receipts against replay inputs by
  reference. It no longer clones a receipt and serializes both sides for each
  comparison. Every frozen field, receipt count and receipt order remain checked;
  the exhaustive field pattern requires updates when the receipt type changes.
  `/tmp/burst-borrowed-receipt-comparison.log` checks identical input acceptance,
  a mutation of every stored field, and snapshot recovery with the source gone.
  `/tmp/burst-borrowed-receipt-signing.log` passes agreement/resume, durable
  fence/journal reopen, minority-promise preservation and missing-receipt rejection.
  `/tmp/burst-borrowed-receipt-live.log` passes live two-epoch proof fetching and
  restarted payment admission; strict node lint passes in
  `/tmp/burst-borrowed-receipt-lint.log`. This removes comparison allocations,
  not the retained historical list. C02/C03 and all four release gates remain open
  and disabled respectively. No production memory or pruning claim follows.

- C02/C03 selection replay and authenticated receipt restoration now share an
  unchanged electorate with its historical parent. Changed delegations keep
  separate weights, and receipt restoration checks the next-roster commitment.
  `/tmp/burst-shared-selection-roster-consensus.log` verifies allocation reuse
  and unchanged historical voting weights. The restoration log
  `/tmp/burst-shared-selection-roster-restoration.log` verifies sharing before
  export and after source-ledger removal, the same selected panel, and rejection
  of inconsistent receipts and wrong historical authority.
  `/tmp/burst-shared-selection-roster-live.log` passes two live installed epochs,
  proof fetching and restarted payment admission; strict consensus/node lint
  passes in `/tmp/burst-shared-selection-roster-lint.log`. This closes a duplicate
  allocation gap, not C02/C03: retained receipt heads, requests, observations and
  whole-state collections still need bounded recovery and mixed-lifecycle
  qualification. No receipt pruning or production memory bound is claimed.
  All four release gates remain false.

- C02/C03 compact snapshot and successor projection now borrow the replay state's
  account map instead of cloning every account into a vector and constructing a
  second lookup index. The slice API and indexed API share the same validation
  and atomic projection implementation. Indexed input checks key/account identity;
  successor publication consumes the original map for frontiers. Account, witness
  and retirement budgets remain unchanged.
  `/tmp/burst-borrowed-account-projection.log` passes database-content equivalence,
  abort/reopen, misbound-account rejection and late-source-error rollback, plus
  the existing projection suite. `/tmp/burst-borrowed-account-transfer.log` passes
  authenticated transfer with 4,000 replayed accounts. Live two-epoch installation
  and restarted payments pass in `/tmp/burst-borrowed-account-successor.log`;
  snapshot replacement/restart passes in `/tmp/burst-borrowed-account-replacement.log`.
  Strict storage/node library and test lint passes in `/tmp/burst-borrowed-account-lint.log`.
  The 16,000-account export/transfer/install/reopen probe passes in 29.77 seconds
  with a 301,768 KiB process high-water mark
  (`/tmp/burst-borrowed-account-16000-probe.log`). This includes fixtures and
  archives; it does not establish a production memory bound or an overall peak
  reduction. The first measurement launch did not execute because `/usr/bin/time`
  was absent; the successful run used the probe's built-in measurements.
  Whole-state replay, operation-limit migration, mixed-lifecycle longevity and
  the earlier intermittent timeout remain open. No release gate changed.
- C15 handshake bookkeeping now bounds both pending challenges and tracked IP
  windows. Previously, consuming cookies freed pending capacity while each new
  IP could leave another accounting entry behind. Unknown IPs now wait when the
  accounting table is full; completed 60-second windows release their slots.
  Existing IPs retain the same rate limit. Expiry and rate accounting use
  monotonic elapsed time rather than wall-clock subtraction. Zero configured
  capacity/rate rejects without allocating accounting entries. Node diagnostics
  now report an unavailable handshake challenge at debug level.
  `/tmp/burst-bounded-handshake-final.log` passes bounded accounting under 10,000
  refused new keys, valid reconnect after window expiry, independent NAT
  challenges, single-use signatures and invalid-identity rejection. The node's
  context-bound mutual-proof test passes in `/tmp/burst-bounded-mutual-handshake.log`.
  `/tmp/burst-bounded-handshake-cold-recovery.log` passes all four cold-start
  interruption cases with content preservation and restarted payments in 173.86
  seconds. Strict network/node library and test lint passes in
  `/tmp/burst-bounded-handshake-lint.log`. No per-IP rate, pending-cookie allowance,
  crash timeout, desktop limit or release gate was raised.
  This closes the unbounded accounting defect, not C15. Shared-NAT availability,
  hostile-network qualification and the earlier intermittent timeout remain open.
- C02/C03 closed-epoch recovery no longer collects every closure or destination
  in memory. It streams closure records and resolves their hashes to authenticated
  installed heights in an anonymous disk index. Sorting uses 4,096-row runs and
  bounded merge readers. Each merge checks its input digest; final checks bind
  lookups to the exact derived bytes and reject duplicate hashes or changed scratch.
  Forward links across skipped epochs and overlapping links remain valid; backward,
  self, unknown and not-yet-installed destinations reject without rewriting records.
  `/tmp/burst-external-epoch-index-final.log` passes these cases and the existing
  signer journal checks. Its 131,073-entry index fixture crosses the old inventory
  count and checks lookup, mutation and truncation. It is synthetic scratch evidence,
  not a 131,073-checkpoint signed-network longevity run.
  `/tmp/burst-external-epoch-recovery.log` passes the 11 targeted installed-recovery
  cases, including promise preservation and damaged/foreign journal rejection.
  Live sync-failure recovery passes in `/tmp/burst-external-epoch-live.log`; strict
  node library/test lint passes in `/tmp/burst-external-epoch-lint.log`.
  This completes the closed-epoch inventory replacement, not C02/C03. Whole-state
  and installation budgets, mixed-lifecycle longevity and the earlier intermittent
  cold-start timeout remain unresolved. Durable formats and release gates did not
  change. All validation used the unchanged desktop wrapper.
- C02/C03 cold-start interruption coverage now includes verified proposal content
  already stored locally. The old `checkpoint_content_received` hook ran only
  after a network fetch; the test could not stop the local-content path at that
  boundary. The hook now runs after either path supplies content and before
  replay/publication. This is test-only; production behavior is unchanged.
  `/tmp/burst-cached-content-crash.log` passes suffix, downloaded-content,
  cached-content and accepted-record interruptions, with snapshot-free restart,
  content preservation and payment admission after each. It took 170.88 seconds
  within the unchanged wrapper. Timeout reports now retain the last 32 recovery
  probes, with bounded probe text.
  Strict node library/test lint passes in `/tmp/burst-cached-content-crash-lint.log`.
  The validation service is inactive; all four public gates remain false.
  The preceding unchanged traced test also passed (`/tmp/burst-cold-start-trace.log`).
  Neither run establishes the cause of the earlier broad-run timeout. That
  unresolved result remains recorded below; no release gate is opened.
- C02/C03 round recovery no longer builds a lifetime journal-key map. It uses
  distinct representatives from each authenticated parent roster and rejects
  uncovered keys. Decision preservation, unfinished rounds, shared delegation,
  malformed heights and unknown signers pass in
  `/tmp/burst-streamed-round-authority.log`.
  Finality promises and fences now share streaming validation with installation
  callers. Recovery retains a coverage count; callers needing a sorted promise
  list still enforce the existing per-namespace operation budgets.
  `/tmp/burst-streamed-finality-recovery.log` passes the installed-component suite,
  including 131,073 synthetic finality promises, fences and counters through full
  journal validation and reopen. Oversized list collection still rejects.
  The broader `/tmp/burst-streamed-finality-integration.log` run finished with
  62 passes, one failure and four subprocess helpers ignored. Bootstrap/download,
  live installation/replacement crash matrices, sync failure, sparse catch-up,
  retirement and journal rejection checks passed. Cold-start interruption timed
  out waiting 120 seconds for `checkpoint_content_received`, after its preceding
  interruption/restart/payment case passed. The unchanged isolated test passes
  all three stages in 94.67 seconds (`/tmp/burst-streamed-finality-cold-start.log`).
  That rerun does not explain the timeout. Scheduling/order diagnosis remains
  required; this integration boundary is not signed off. No timeout was raised.
  Strict consensus/node library and test lint passes in
  `/tmp/burst-streamed-finality-lint.log`; the distinct-representative/equal-human
  roster test passes in `/tmp/burst-streamed-roster.log`. All checks ran within
  the unchanged desktop wrapper; the validation service is stopped.
  This completes those inventory replacements, not C02/C03 or public release.
  Closed-epoch maps, bounded installation inventories, whole-state limits and
  mixed-lifecycle longevity remain. All four public gates remain false.
- C02/C03 signer recovery now streams sequence/counter/snapshot-promise records
  and uses indexed counter lookups. Every counter epoch must still be installed
  and authenticated; attestations retain historical electorate checks. Duplicate
  or unordered authority entries reject. The local change guard now keeps a
  length-framed digest instead of all journal keys and hashes. It does not grant
  authority. A 131,073-signer counter fixture passes full journal validation and
  reopen, detects changed counters and ignores unrelated preferences
  (`/tmp/burst-streamed-signer-journals.log`). Integrated recovery preserves
  promises and still rejects damaged or foreign journals without repair
  (`/tmp/burst-streamed-signer-recovery.log`); strict storage/node/test lint passes
  in `/tmp/burst-streamed-signer-lint.log`. The running-node sync-failure recovery
  journey passes in 3.21 seconds (`/tmp/burst-streamed-signer-live.log`). All
  checks used the unchanged desktop wrapper. This is not qualification of that
  many live signers. Round, finality and closed-epoch inventories, whole-state
  scaling and physical pruning remain open. Public gates are unchanged.
- C02/C03/C16 compact projection now streams old height indexes and block keys,
  preserving cemented bodies/indexes in place rather than rebuilding a complete
  retained-history map. The input witness and provisional-retirement budgets
  remain; they no longer cap total cemented history. A storage fixture with
  131,073 retained bodies totaling more than 64 MiB passes replacement, abort,
  commit and reopen. Old confirmation work is acknowledged without requiring
  every retained body to be resupplied. Missing bodies, conflicting positions,
  orphaned reverse indexes and late archive conflicts still reject and poison
  the writer. Evidence is `/tmp/burst-streamed-compact-indexes.log`; strict
  storage/test Clippy passes in `/tmp/burst-streamed-compact-indexes-lint.log`.
  Live snapshot installation/replacement, peer-proof recovery, SIGKILL at five
  installation stages, late confirmation and sync-failure recovery pass in
  `/tmp/burst-streamed-compact-live.log` (178.42 seconds). These checks used
  the unchanged desktop wrapper; no release gate changed.
  The large fixture uses opaque storage bodies. It does not establish large
  signed-network acceptance, bounded whole-state memory or safe physical pruning.
- C02/C03/C16 checkpoint replacement now clears old owned tables without
  copying all rows, and clears owned metadata namespaces in 256-key batches.
  The complete replacement still uses one LMDB transaction. Storage checks cover
  a 131,073-row lifecycle namespace, abort/reopen, poisoned writers, sparse
  projection, and preservation of unrelated settings and signing journals
  (`/tmp/burst-bounded-projection-clear.log`). Node recovery checks preserve
  economic rights, identity arrival order, provisional chains and signing
  promises, while still rejecting changed or corrupt state
  (`/tmp/burst-bounded-projection-recovery.log`). Live snapshot replacement with
  peer proofs passes in 10.63 seconds (`/tmp/burst-bounded-projection-live.log`);
  strict storage/test Clippy passes in `/tmp/burst-bounded-projection-lint.log`.
  All validation used the unchanged desktop wrapper. These remove the old-table
  clearing limit. The retained-block scanning change is recorded above.
  Physical pruning and full lifetime qualification remain incomplete.
- C06/C16 the node no longer deletes origin/reverse token indexes merely because
  an origin was revoked or a token expired. The removed background task did not
  compact ledger bodies, used a hard-coded policy, scanned unbounded collections
  and reported candidate counts while ignoring deletion failures. Normal expiry
  processing and bounded expiry-lookup cleanup remain. The retained path passes
  atomic pending returns, revocation reversal, expiry extension back to spendable
  funds and database reopen with origin lookup intact
  (`/tmp/burst-expiry-retained-index-retry.log`). The existing live-node journey
  also passes exactly-once pending expiry return, restart and fresh-node catch-up
  in 8.50 seconds (`/tmp/burst-expiry-retention-live.log`). Both checks used the
  unchanged desktop resource wrapper. The first compile failed on a
  missing mutable test binding; its log is retained separately. No stored data
  was deleted by this change. This removes unsafe maintenance, not the pruning
  requirement: configurable dependency-preserving compaction and recovery from
  archival peers remain C16 release blockers. No capability is signed off here.
- C10/C11/C18 native parameter and constitution authoring journeys both pass
  on the same host (142.91 seconds combined). The wallet reviews and submits
  exact changes, supports cancellation, rejects stale/wrong-wallet reviews,
  burns the reviewed endorsement amount and signs ordinary and secondary-key
  votes. Parameter withdrawal preserves the spent endorsement. Certified
  activation, all-source restart and a fresh observer preserve the expected
  tally, parameter value and multilingual constitution. Evidence is
  `target/validation-evidence/native-20261014/authoring-recovery.log`, with
  `parameter-review.png`, `withdrawal-review.png` and `constitution-review.png`.
  These selected Linux journeys are complete. The full parameter catalog,
  concurrent amendments, cross-platform artifacts and other release blockers
  in the capability table remain open. The source archive hash and test-only
  clock correction hash are recorded in that directory's `host-state.json`.
- C12/C18 native scoped delegation now passes on the bounded Azure validation
  host. The actual wallet creates parameter and proposal grants, discovers and
  decrypts incoming grants, signs two delegated ballots, revokes a grant and
  rejects stale or wrong-wallet signing. Three nodes certify the expected tally
  and activation; all three restart, and a fresh observer recovers the same
  results and scoped records. The selected journey passed in 40.76 seconds.
  Evidence is `target/validation-evidence/native-20261014/delegation-recovery.log`
  and `scoped-ballots.png`. The first run passed the wallet interactions but
  failed recovery because startup used wall time after the fixture accelerated
  its certified cutoff. Startup now uses the fixture clock in test builds only;
  production clock checks are unchanged. That failure remains in `first-run.log`.
  This completes this native delegation journey, not the remaining deadline,
  cross-device, platform or full wallet matrix. All public gates remain closed.
- C12/C18 earlier desktop attempts did not verify native delegation. The wallet
  debug binary was rebuilt from the current tree with the protocol's pinned
  nightly toolchain (`/tmp/burst-current-native-wallet-build-retry.log`, 29.76
  seconds). The native runner now builds frontend assets before starting WebKit
  and serves them from its existing process, with owned-server cleanup. It no
  longer requires a separately running Vite server. Frontend builds and runner
  syntax validation pass. Actual native attempts failed before usable wallet
  controls: esbuild/GLib thread creation reached the unchanged 64-task limit;
  a later run crashed in AMD graphics initialization under Xvfb; software rendering
  still failed in WebKit thread creation. Logs are
  `/tmp/burst-current-native-delegation.log`, `-bounded.log`, `-static.log`,
  `-direct.log` and `-software.log` with that same base filename. The runner used
  smaller worker pools, single-core affinity and finally software rendering,
  always inside the existing desktop wrapper. No cap was raised. These are
  failed acceptance runs, not passes or evidence of a delegation protocol fault.
  These failures led to the host validation recorded above; release gates remain closed.
- C03/C10/C12 governance counting now has non-empty killed-process publication
  coverage, including an encrypted parameter-scope grant and two secondary
  ballots. The fixture has no blanket delegation, so the expected two Yea and
  one Nay votes on each proposal require the encrypted-key path.
  At each of four stages (durable commit, economic publication, authority
  publication and completion), the test kills its own child process, compares
  durable state, restarts, checks runtime caches, rejects an old-epoch payment
  and admits a current-epoch payment. After each restart the recipient retrieves
  and decrypts the confirmed grant through the API. The owner then submits a
  generation-two revocation, which is accepted and removes the incoming index.
  The certificate is fixture-supplied;
  this is not a distributed-quorum or native-app journey. All three affected
  crash journeys pass in `/tmp/burst-key-only-publication-crash.log`
  (3.47 seconds); strict node/test Clippy passes in
  `/tmp/burst-key-publication-crash-lint.log`. The earlier governance-only case
  passed in `/tmp/burst-publication-crash-regressions-recovery.log`.
  Earlier failures are retained
  in `/tmp/burst-governance-publication-crash.log` (JSON comparison cannot encode
  populated governance map keys) and `/tmp/burst-publication-crash-regressions.log`
  (a stale fixture omitted mandatory governance state). The comparisons now use
  complete decoded governance equality, and the fixture persists its replayed
  governance state. Production missing-state checks and release gates are unchanged.
- C04/C10/C12 post-cut revocation now passes in the partitioned counting journey
  (`/tmp/burst-count-boundary-key-revocation.log`, 141.60 seconds). An isolated
  node provisionally admits a parameter-scope revocation timestamped after the
  pending count. It cannot finalize in that epoch. After healing, the certified
  count removes the old-epoch operation; a newly signed revocation then confirms.
  The earlier ballot still determines the closed tally. Exact results, activation,
  owner generation and recipient indexes survive source restart and fresh-node
  recovery. This completes the selected post-cut/retry case, not the full deadline
  matrix or crash-boundary qualification. Strict node/test Clippy passes in
  `/tmp/burst-count-boundary-key-revocation-lint.log`. Both checks used the
  desktop wrapper. Public gates remain closed.
- C04/C10/C12 opposite-arrival revocation passes in
  `/tmp/burst-concurrent-key-revocation.log` (139.67 seconds). The existing
  partitioned journey now delivers a secondary ballot to two nodes and its
  owner's proposal-scope revocation to the isolated third node. It verifies
  receipt, prevents cross-partition delivery, and checks that neither operation
  finalizes without quorum. After reconnection both operations confirm. The
  revoked transitive vote contributes no weight at certified counting; exact
  totals and scoped records survive source restarts and fresh-observer recovery.
  Historical ballot admission remains valid after revocation. This selected
  concurrency case is complete; counting-boundary races, crashes during
  publication and native acceptance are not. Strict node/test Clippy passes in
  `/tmp/burst-concurrent-key-revocation-lint.log`. Validation stayed within the
  desktop wrapper. No production code or gate changed.
- C04/C10/C12 elected-ballot counting now passes in the existing three-human
  governance journey (`/tmp/burst-certified-ballot-fork.log`, 127.66 seconds).
  A provisional secondary Nay ballot is replaced by a signed-quorum Yea ballot.
  Receipt of one and two human votes leaves the provisional head unconfirmed;
  the third confirms the replacement. Exact certified totals remain two Yea
  and one Nay after activation, source restarts and fresh-observer recovery.
  The losing ballot is absent on all three sources before counting. The fixture
  retains transitive grants, ordered scoped revocations and owner precedence.
  Its signed-quorum setup is shared with the endorsement-fork journey; the
  post-consolidation checks pass in `/tmp/burst-certified-forks-shared.log`
  (ballot journey 123.24 seconds; endorsement journey 66.10 seconds). Strict
  node/test Clippy passes in `/tmp/burst-certified-forks-shared-lint.log`.
  All checks used the desktop wrapper. No public gate changed. This completes the selected
  fork-plus-count case, not C12 or public-release qualification. Concurrent
  revocation, crash boundaries and native acceptance remain required.
- C04/C10/C12 existing coverage was inspected and rerun instead of duplicated.
  `/tmp/burst-current-partitioned-key-count.log` passes
  `partitioned_secondary_ballots_and_counting_wait_for_quorum_then_recover`
  on the current worktree in 146.14 seconds under the desktop wrapper.
  `governance_certified/key_ballots.rs` exercises encrypted transitive parameter
  grants, proposal-specific revocation before and after ballots, direct-owner
  vote precedence, invalid signer/dependency rejection and exact certified vote
  totals. Its partition helper checks that neither a ballot nor a counting
  checkpoint finalizes without quorum, then heals the partition. The journey
  activates the approved rate, performs a payment, restarts the three sources,
  bootstraps a fresh observer, and checks proposal outcomes plus confirmed
  incoming/outgoing delegation API records on all recovered nodes.
  These selected certified-counting, API-record and recovery cases are verified;
  they must not be described as wholly absent. Revocations are ordered and the
  clock is controlled. The ballot-fork extension is recorded above; concurrent
  revocation races, crash boundaries and native acceptance remain required.
  No source code or public gate changed for this earlier audit.
- C03/C04/C11/C12 rollback now reconciles persisted delegation graphs by owner
  and constitution state by amendment. Global/scoped entries share one owner
  scope; amendment content, votes and conflicts share one amendment scope. Global
  document or depth changes require the complete recorded after-state. Cycles
  retain ordinary delegation semantics: they resolve to no delegated vote rather
  than becoming rejected transactions. Reverse delegation indexes rebuild from
  the restored graph. All images and restored outputs keep the existing byte
  bounds; only the two named metadata keys enter the storage reconciliation hook.
  `/tmp/burst-governance-related-undo.log` passes independent and repeated undo,
  cross-scope conflict rejection, cycle resolution, global-state protection and
  malformed snapshot rejection. `/tmp/burst-governance-related-undo-writer.log`
  passes signed proposal, legacy delegation and revocation replacement through
  failure injection and database reopen. The fixture now restores governance
  from the writer before each operation and retains the bootstrap constitution;
  it no longer relies on a reused candidate or an empty constitution.
  `/tmp/burst-governance-related-undo-regressions.log` passes the governance and
  constitution suites, storage callback restrictions, rollback regressions and
  strict governance/constitution/storage/node lint.
  The gated fork path now restores all three engines inside the rollback writer,
  reuses ordinary governance execution and publishes them only after commit under
  the admission lock order. Compact governance and legacy delegation operations
  are eligible. Full-content proposal fork support is described below;
  secondary-key payloads now reach the shared executor behind their existing
  gate. Ordinary fork activation remains excluded and retains certified activation.
  `/tmp/burst-governance-fork-worker.log` passes both delegation replacement
  directions and restart followed by a signed delegate/revoke operation, alongside
  existing verification fork regressions. `/tmp/burst-governance-fork-lint.log`
  passes strict node lint. `/tmp/burst-governance-fork-wire-recovery.log` adds
  two-node signed-TCP delegation replacement in both directions, forged-vote
  rejection, restart on each node and a subsequent signed operation. The shared
  verification network journey still passes. This uses the genesis one-human
  roster, not a multi-human partition. Final strict node lint also passes in
  `/tmp/burst-governance-fork-final-lint.log`; `git diff --check` is clean.
  The first network-test compile failed on
  a moved test value (`/tmp/burst-governance-fork-wire.log`); it is not passing evidence.
  Full-content candidates now atomically cache their block and authenticated
  content, bounded to 256 candidates and 512 KiB of content per candidate.
  Restart re-admits cached content; installation stages it with its block and
  governance effects. The chunk service can serve cached proposals, and finalized
  cleanup removes the paired cache records without deleting ledger content.
  `/tmp/burst-full-content-fork-recovery.log` passes both directions with a
  50,000-character constitutional proposal, restart before election completion,
  exact content recovery and existing fork regressions. This worker injects
  election authorization; it does not prove a network quorum. The first build
  failed on test access to the transfer pool (`/tmp/burst-full-content-fork.log`).
  `/tmp/burst-full-content-finalization.log` also verifies that finalization removes
  candidate content but retains the winning ledger content. The overly broad
  cleanup-filter run was stopped during compilation and is not passing evidence.
  `/tmp/burst-fork-content-transfer.log` exposed nested LMDB reader transactions
  in the new serving fallback. Transaction-bound cache readers fix that defect;
  `/tmp/burst-fork-content-transfer-recovery.log` passes TCP chunk recovery from
  both archived history and cached forks without admitting either as a block.
  `/tmp/burst-fork-content-store.log` passes cache bounds, immutable paired writes,
  oversized-content rejection without partial records and reopen recovery.
  `/tmp/burst-full-content-fork-lint.log` passes strict storage/node lint.
  `git diff --check` is clean. These `/tmp` logs remain development evidence,
  not durable revision-bound release attestations.
  `/tmp/burst-full-proposal-network.log` now joins content transfer, signed-TCP
  election installation and restart in one journey. Only the submitting node
  receives the 50,000-character proposal directly; its peer fetches the content.
  Both replacement directions pass, forged votes leave the incumbent unconfirmed,
  both restarted nodes recover the exact proposal, and signed withdrawal or a
  new full proposal succeeds afterward. Existing verification/delegation wire
  journeys still pass. This is a two-node genesis-roster test, not independent
  multi-human quorum, partition or crash-boundary qualification.
  `/tmp/burst-full-proposal-network-lint.log` passes strict node lint;
  `git diff --check` is clean. No public gate changed.
  C03/C04/C12 encrypted key operations now reuse the ordinary governance executor
  during fork evaluation and installation. Their authority and index records
  use the existing journaled writer; ballot grant references already enter the
  block dependency set. No second delegation authority engine was added.
  `/tmp/burst-encrypted-delegation-fork-recovery.log` passes parameter-scoped
  encrypted grant replacement in both directions, restart, subsequent grant or
  owner revocation, and rejection of the stale grant against the recovered
  generation-2 tombstone. Only the named recipient can decrypt the fixture's
  grant. The legacy delegation worker also passes. Election authorization is
  injected; this is not wire/quorum evidence or a delegated-ballot journey.
  The initial test build used a nonexistent blanket-key scope and compared grants
  without an equality implementation (`/tmp/burst-encrypted-delegation-fork.log`).
  The corrected test uses the selected per-parameter scope from decision 20.8
  and compares authenticated grant identities. Per-proposal replacement, index
  views, delegated ballots and concurrent revocation still need end-to-end evidence.
  `/tmp/burst-key-fork-regressions.log` passes all worker and existing signed-network
  fork journeys; `/tmp/burst-key-fork-lint.log` passes strict node lint.
  `/tmp/burst-encrypted-key-network.log` adds the encrypted parameter grant to the
  signed-TCP journey. Both replacement directions, forged-vote rejection, restart
  on both nodes and subsequent owner revocation/new grant pass. Incoming/outgoing
  index records match those follow-up operations. Existing full-proposal,
  verification and legacy-delegation wire journeys still pass. This uses the
  genesis one-human roster; dependent ballots, proposal scopes, revocation forks,
  multi-human partitions and app-level recipient/index views remain unqualified.
  Final strict node lint passes in `/tmp/burst-encrypted-key-network-lint.log`;
  `git diff --check` is clean and all four public gates remain false.
  C03/C04/C12 ballot dependency checks now cover both proposal and parameter
  scopes, each with and without owner revocation before ballot arrival. Signed
  proposal, encrypted grant, revocation and secondary-signed ballot operations
  use ordinary writer-bound execution. Historical ballots remain admissible;
  the count-cut projection includes a live grant's vote and excludes a revoked
  grant's vote. Replacing the grant removes its dependent ballot and any owner
  revocation descendant, restores the recipient head, and retains the proposal.
  Injected abort preserves the original state; reopen before and after replacement
  preserves the same outcome. `/tmp/burst-key-ballot-fork.log` covers the initial
  two-scope case; `/tmp/burst-key-ballot-revocation-fork.log` covers all four cases.
  The fixture starts with verified account anchors and shortened proposal rules.
  It is transactional evidence, not onboarding, live publication, finality or
  network qualification. No protocol behavior changed to make these checks pass.
  `/tmp/burst-key-ballot-fork-lint.log` passes strict node lint; `git diff --check`
  is clean. Public gates remain closed.
  `/tmp/burst-key-ballot-worker.log` now carries grant-to-ballot rollback through
  a running node for both scopes. Signed bootstrap endorsement and account
  opening establish the recipient; signed proposal/grant/ballot operations then
  enter ordinary admission. Replacing the grant removes the recipient's ballot
  from storage and live governance, restores its account head, and preserves
  the proposal. After restart, a fresh grant and ballot are accepted and the
  live ballot is restored. The prerequisite blocks are confirmed before the
  competing branch is installed. Election authorization is injected; proposal
  windows and endorsement requirements use explicit test parameters. This proves
  live publication and positive restart behavior, not signed-network quorum,
  production-clock timing, concurrent revocation or crash-boundary recovery.
  `/tmp/burst-key-ballot-worker-lint.log` passes strict node lint; `git diff --check`
  is clean. Public gates remain closed.
  `/tmp/burst-key-ballot-network.log` now verifies this dependency journey over
  signed TCP votes for both selected scopes. Both nodes receive signed onboarding,
  proposal, grant and ballot operations. Signed final votes confirm prerequisites
  and install the competing branch without injecting election outcomes. Both nodes
  remove the dependent ballot from storage/live governance and restore the
  recipient head; each restarted node accepts a fresh grant/ballot pair. The
  original injected-worker variant also passes using the shared journey. This
  uses two nodes and the installed genesis one-human roster, shortened governance
  timing and sequential restart. Multi-human quorum/partitions, concurrent
  revocation ordering, direct ballot forks and crash-boundary recovery remain
  open under C03/C04/C12. No production behavior or public gate changed.
  `/tmp/burst-key-ballot-network-lint.log` passes strict node lint;
  `git diff --check` is clean.
  `/tmp/burst-direct-ballot-network.log` adds a direct secondary-ballot conflict
  in both scopes. The grant remains confirmed; signed TCP votes elect a competing
  Nay ballot over the stored Yea ballot on the recipient chain. Both nodes publish
  the winning choice. After each restart, resubmitting the loser does not restore
  it, owner revocation succeeds, and another recipient operation is accepted.
  The historical Nay ballot remains while the recovered count-cut projection
  assigns no delegated vote to the revoked grant. Existing grant-dependent
  rollback journeys also pass. This uses the genesis roster and sequential
  revocation, not a full certified counting round under concurrent revocation,
  multi-human partition or crash-boundary recovery. No production behavior changed.
  `/tmp/burst-direct-ballot-network-lint.log` passes strict node lint;
  `git diff --check` is clean. Public release gates remain closed.
  Compact proposal/vote/support fork journeys, multi-human full-content recovery
  and fault injection, secondary-key forks, overlapping dependency closure and adversarial
  distributed qualification remain open under C03/C04/C10-C12. No capability row
  is complete from these checks. All public gates remain closed.
- C03/C04/C10 governance-engine undo now treats each proposal, its votes, timing,
  support counts and secondary ballots as one conflict scope. Unrelated proposals
  survive; later changes in the affected scope must be rolled back first. Global
  queued rule changes require the complete recorded after-state. Semantic equality
  avoids false conflicts from map serialization order, and ballot indexing avoids
  rescanning every ballot for every proposal. The existing bounded storage hook
  permits only `BrnMeta/governance_engine`; protocol parameters, primary indexes
  and consensus records remain outside it.
  `/tmp/burst-governance-scoped-undo.log` passes independent/repeated undo,
  overlapping-field rejection, global-rule protection and malformed encodings.
  `/tmp/burst-governance-scoped-undo-writer.log` passes signed proposal replacement
  through the shared writer/rollback path, preserving another proposal, aborting
  after replacement staging without changing original bytes, and reopening the
  database after both replacements. The preceding integration attempt failed to
  compile because its fixture used the wrong BRN-store accessor; it is not passing
  evidence. These are component/storage checks, not elections or runtime
  publication. Constitution/delegation snapshots, governance fork admission and
  publication, overlapping dependency closure and network recovery remain open.
  `/tmp/burst-governance-scoped-undo-regressions.log` passes the governance suite,
  storage callback restrictions, existing rollback checks and strict
  governance/storage/node lint. No public gate changed.
- C03/C04 verification fork execution now shares ordinary admission's endorsement,
  challenge and vote effects behind the existing certified-verification gate.
  Installation restores verification inside the rollback writer and publishes it
  with BRN/TRST only after commit. `/tmp/burst-verification-fork-publication.log`
  passes endorsement replacement in both directions, dependent account removal,
  unchanged live state during candidate evaluation, restart and a subsequent
  signed operation. Independent endorsement progress still survives replacement
  after reopen. The worker test injects election authorization; it is not signed
  network quorum evidence. `/tmp/burst-verification-fork-lint.log` passes strict
  verification/storage/node lint. The earlier worker run was stopped to correct
  the fixture's epoch; its next run failed because a canonical-block scheduler
  cannot register a cached fork. Neither is counted as passing evidence.
  `/tmp/burst-verification-fork-shared-recovery.log` passes the controlled-time
  multi-node revocation/re-verification journey in 155.09 seconds after sharing
  the executor. Pending TRST restoration, new spending, restarted sources and a
  fresh observer retain matching state. This is ordinary lifecycle recovery,
  not fork quorum or production-clock qualification.
  `/tmp/burst-verification-fork-candidates.log` passes valid vote and challenge
  candidate evaluation against a competing payment branch. Evaluation leaves
  the incumbent, verification snapshot and stake balances unchanged after reopen;
  the gated-off path still refuses these candidates. Installation and signed
  network election coverage for those two block types remain open.
  `/tmp/burst-verification-fork-regressions.log` passes the existing payment-fork
  worker suite and final strict lint. Replacement preserves independent cemented
  payments and shared-origin consumers, removes received losing branches, and
  recovers without resurrecting balances or inventing quorum. These checks also
  inject election authorization. Temporary logs remain investigation evidence,
  not durable revision-bound release attestations under C01.
  `/tmp/burst-verification-fork-wire-recovery.log` now passes endorsement fork
  replacement in both directions over authenticated TCP between two nodes.
  Forged final votes leave the incumbent unconfirmed; a signed final vote from
  the genesis human installs the replacement on both nodes. Each node then
  reopens and accepts a new signed operation using the recovered verification
  state. The test never registers or confirms an election directly. Strict node
  lint passes. This covers the initial one-human roster, not multi-human quorum,
  partition recovery, or signed challenge/vote fork installation. The earlier
  `/tmp/burst-verification-fork-wire.log` passed a weaker restart check; the final
  version also requires successful post-restart execution.
  `/tmp/burst-three-human-verification-fork-recovery.log` passes both endorsement
  replacement directions with three independently represented humans in 43.21
  seconds. The test observes received final votes on every node: one and two
  people cannot replace the incumbent; the third vote reaches the configured
  67% quorum. Governance then counts and activates normally. Restarted sources
  and a fresh observer recover the winning endorsement exactly once and confirm
  a payment under the governed parameters. Votes are signed and sent over TCP;
  automatic payment voters are paused during the fork, and election outcomes are
  never injected. This is connected-network coverage, not partition or Byzantine
  qualification. Ordinary and fork admission now share their clock source,
  including the existing controlled test clock; production bounds are unchanged.
  The initial run timed out because the test expected raw tallies of 1 rather
  than the protocol's 10,000 units per person. The diagnostic run was interrupted;
  the bounded tally diagnostic then proved the mismatch. None is passing evidence.
  `/tmp/burst-three-human-fork-regressions.log` passes both ordinary/fork
  60-second admission-boundary checks, the signed one-human fork/restart journey,
  and strict node lint after the shared-clock change.
  Governance forks, overlapping dependency closure and signed-network lifecycle
  fork qualification remain release blockers. All public gates remain closed.
- C03/C04 rollback can reconcile disjoint verification snapshots through the
  existing bounded undo callback. State, selections, appeals and attempt records
  share a target conflict scope. Verifier cooldowns and chronological Neither
  history share another. Overlapping progress is rejected, not overwritten.
  `/tmp/burst-verification-scoped-undo-integration.log` passes the verification
  suite and signed-endorsement replacement after database reopen. Replacing one
  endorsement preserves the other's progress and BRN burn; replacing the second
  does not resurrect the first. `/tmp/burst-verification-scoped-undo-regressions.log`
  passes failure after replacement staging, repeated reopen, existing rollback
  and dependency checks, storage callback restrictions and strict lint. The
  public gated-off runtime's unsupported-lifecycle refusal remains tested.
  `/tmp/burst-verification-scoped-undo-live-regression.log` passes cooldown/history
  coupling, live payment-fork/restart regressions and verification lint. Global
  verification-rule rollback now requires the entire recorded after-state;
  later progress cannot silently retain effects of a removed rule change.
  `/tmp/burst-verification-undo-rule-dependencies.log` passes focused
  rule-dependency checks and combined strict lint.
  This verifies transactional rollback, not network-authorized lifecycle fork
  replacement. The gated verification execution/publication evidence is recorded
  above; governance reconciliation and overlapping dependency closure remain
  incomplete.
- C03/C04 ordinary admission and payment replacement now share the writer-bound
  predecessor/pending validation and economic ledger staging step. Both paths
  read signed predecessor balances inside the writer; ordinary admission no
  longer passes its earlier read into economic application. Lifecycle effects,
  commit and publication remain caller-owned. The existing payment-transition
  checks pass in `/tmp/burst-shared-writer-transition.log`.
  `/tmp/burst-shared-writer-regressions.log` passes copy-on-write/restored-state
  equivalence, aborted-write isolation, malformed predecessor refusal and strict
  node lint. Its `live_payment_fork` filter selected zero tests and provides no
  live evidence. `/tmp/burst-shared-writer-live.log` passes the actual fork
  integration target, including received-branch replacement, independent cemented
  payments, shared-origin consumers, repeated replacement and restart without
  inventing quorum. The certified three-human governance journey also passes
  proposal/count/activation and delayed observer recovery in 63.73 seconds.
  `/tmp/burst-shared-writer-verification.log` passes the full controlled-time
  verification/challenge/re-verification journey against shared staging in
  181.23 seconds, including restored pending TRST, new spending and matching
  recovery on restarted nodes and a fresh observer.
  This removes duplicate staging logic; identity/governance fork application,
  selective rollback and full runtime publication remain incomplete.
- C03/C07 lifecycle admission and non-compact startup now restore verification
  from the writer transaction through one strict decoder. Admission no longer
  clones the live orchestrator. Fresh genesis persists its verification snapshot
  with the account, economics and governance snapshots; orphaned verification
  data prevents reinitialization. Missing established state no longer starts an
  empty orchestrator. Old databases need authenticated recovery, not fabricated
  state; upgrade qualification remains open under C21.
  `/tmp/burst-verification-writer-state.log` passes staged-state visibility,
  abort isolation, missing/corrupt startup refusal and orphan preservation,
  followed by strict node lint. Initial verification rules now match configured
  parameters in fresh genesis and replay. The non-default-rule equality check
  passes in `/tmp/burst-verification-unified-live.log`; its live journey fails
  while waiting for certified challenge settlement, before re-verification.
  `/tmp/burst-verification-writer-diagnostic.log` reproduces a verification
  snapshot mismatch during checkpoint recovery. The field comparison in
  `/tmp/burst-verification-publication-binary-diff.log` identifies equal votes
  in different arrival orders. Snapshot encoding now sorts unordered current
  votes, prior-vote evidence and endorsements without changing panel ranking or
  the chronological Neither window. The temporary comparison code was removed.
  The JSON diagnostic in `/tmp/burst-verification-publication-diff.log` could not
  represent protocol values and did not identify the mismatch. The initial
  canonical-evidence test used an invalid fixture address and failed before its
  assertions in `/tmp/burst-verification-canonical-evidence.log`.
  `/tmp/burst-verification-canonical-evidence-corrected.log` passes the verification
  suite, including ordering, panel-rank sensitivity and snapshot migration. The
  live journey passes challenge settlement but then times out waiting for the
  re-verified human's agreed authority on every node. Its lint step did not run.
  Per-node authority and persisted verification diagnostics were added to that
  boundary. `/tmp/burst-reverified-authority-diagnostic.log` reproduces the
  failure and identifies a transition-dependency mismatch in applied replay.
  The live last-arriving vote and replay's last vote can differ. Applied recovery
  now uses the existing checked dependency reader to order the local settlement
  trigger after its sources. These links grant no authority; signed replay and
  complete cache comparison still have to pass. This preserves decisions 6.15
  and 36.2: fresh verification can restore identity and originated TRST.
  `/tmp/burst-reverified-transition-order.log` passes the live journey in 163.90
  seconds. It verifies agreed five-human authority after fresh re-verification,
  retained BRN without accrual during the pause, receipt of restored pending
  TRST, new spending, and matching recovery on every restarted node plus a fresh
  observer. This closes this controlled-time journey, not C03/C07 as a whole.
  `/tmp/burst-reverified-order-regressions.log` passes dependency permutations,
  missing/changed dependency refusal, existing identity arrival-order recovery,
  economic-cache corruption refusal and strict verification/node lint.
  `/tmp/burst-inactive-reverification-recovery.log` passes the separate inactivity
  journey in 166.58 seconds. TRST remains valid while BRN accrual is paused;
  fresh verification, restored spending and restart/fresh-observer recovery pass.
  `/tmp/burst-deadline-transition-order.log` passes certified deadline settlement
  in 119.16 seconds and early-vote activation/deadline recovery in 137.36 seconds.
  Both check the agreed absent panel, stake effects, changed human authority,
  spending rules and matching restart/fresh-observer recovery.
  Real-clock/adversarial scheduling,
  lifecycle forks, upgrade compatibility and full application acceptance remain.
  All four public release gates remain false.
- C02/C04 current-epoch historical tails now request existing durable final votes
  through the batched confirmation loop. A bounded in-memory queue grants an
  exact hash an age exception only after valid final signatures reach the
  installed roster's human quorum. Duplicate, non-final, unknown-signer,
  forged and insufficient votes grant nothing. Normal signature, work, epoch,
  dependency and economic validation still run; the exception neither installs
  a snapshot nor creates an election result. The queue expires and can reacquire
  proof after restart; it does not erase durable signing reservations.
  `/tmp/burst-historical-final-votes-batched.log` passes rejection checks and the
  live three-human governance/restart/fresh-observer journey with both proposal
  and post-checkpoint payment older than 60 seconds, plus strict node lint.
  `/tmp/burst-historical-final-votes-transport.log` passes the transport suite.
  The strengthened live journey starts only one source and the observer. It
  checks that one genuine final vote leaves the old payment unapplied, then
  starts the other two representatives. The diagnostic run in
  `/tmp/burst-historical-tail-quorum-diagnostic.log` passes minority non-admission
  and subsequent quorum recovery in 70.19 seconds. The earlier run in
  `/tmp/burst-historical-tail-quorum-recovery.log` timed out after representatives
  returned; its cause is unresolved. A passing rerun does not close that issue.
  Review also found that a full admission queue discarded an authorized block
  while stopping further requests. Both vote handlers now restore retryability
  after a failed queue handoff, retaining unique-human weights. The focused
  queue-full, duplicate-vote and successful-retry check passes in
  `/tmp/burst-historical-tail-queue-recovery.log`. Its live run failed before
  minority proof collection: the old payment arrived about 32 seconds after
  startup, at the next frontier scan. Lint did not run after this failure.
  Initial frontier discovery now stays pending until it can be sent to a
  connected peer, instead of losing the first scan when no peer is ready.
  `/tmp/burst-historical-tail-initial-discovery.log` passes the full live journey
  in 40.99 seconds under unchanged deadlines, followed by strict node lint.
  `/tmp/burst-historical-tail-bootstrap-regression.log` passes the bootstrap
  regression suite, including persisted-cursor retries and response binding.
  The queue-pressure fix is separate from this delay. The original timeout
  after representatives returned remains unexplained; no release gate changed.
  `/tmp/burst-historical-tail-interrupted-proof.log` extends the same live
  journey through an observer restart after one final vote. Reopening the same
  database starts with no cached proof or admitted payment. The observer
  reacquires the available vote and still leaves its account head unchanged;
  after the other representatives return, all four nodes confirm the payment
  and agree on governance state. The journey passes in 81.95 seconds, followed
  by strict node lint. This verifies graceful interruption of partial proof
  collection, not SIGKILL, disk faults or hostile partition scheduling.
  The runtime path is inside the existing certified gate. This closes that
  controlled-clock current-epoch tail case, not cross-epoch bypass, large-tail
  qualification, hostile scheduling or full C02/C04 release acceptance.
- C02/C10 certified governance recovery now explicitly crosses the ordinary
  timestamp window. The existing three-human RPC/P2P journey produces counting
  and activation certificates with three independent representative keys. It
  stops all sources, advances the controlled clock until the proposal is 61
  seconds old, then restarts them with a fresh observer. All four recover the
  same certified epoch, activated proposal, ballot totals, governed parameter
  hash and confirmed post-activation payment. The payment tail remains inside
  the live window in this earlier run; the final-vote recovery entry above
  records the subsequent aged-tail implementation and verification.
  `/tmp/burst-certified-governance-aged-recovery.log` passes in 63.43 seconds.
  This reuses the existing certified runtime instead of weakening timestamp
  checks. It is controlled-clock live-service evidence, not real-clock timing
  qualification or permission to open the gates. Full C02/C10 remain incomplete.
- C03/C10 governance admission restores governance, delegation and constitution
  from its own writer transaction instead of cloning live state. Fresh genesis
  persists all three atomically. Missing or malformed snapshots reject admission
  without advancing the account head; orphaned snapshots prevent reinitialization.
  `/tmp/burst-governance-writer-state-corrected.log` passes writer visibility,
  aborted-write isolation, strict snapshot decoding and delegation checks, but
  its live late-observer test timed out. The diagnostic rerun in
  `/tmp/burst-governance-writer-state-diagnostic.log` passes the signed governance
  journey, late replay, zero-rate reversal and restart under unchanged deadlines.
  The timeout remains unexplained, not dismissed as resolved. Strict node lint
  passes in `/tmp/burst-governance-writer-state-clippy.log`; genesis snapshot
  preservation and economic recovery pass in
  `/tmp/burst-governance-genesis-recovery.log`. C21 compatibility remains open:
  older databases missing these snapshots need authenticated reconstruction,
  not an empty-state backfill. This does not enable lifecycle forks or complete
  C03/C10. All four gates remain closed.
  Startup now uses the same complete restore instead of separate constitution
  and delegation fallbacks, following decision 26.1. Repeated missing/malformed
  snapshot startup checks preserve the original bytes and pass in
  `/tmp/burst-governance-startup-recovery.log`. The live rerun after this change,
  `/tmp/burst-governance-unified-startup-live.log`, reproduces the late-observer
  timeout: its RPC reports `proposal not found` after the original nodes activate.
  `/tmp/burst-governance-sync-diagnostic.log` reproduces this with a connected
  observer still at genesis. The admission code enforces a 60-second live
  timestamp window; this legacy fixture supplies no historical certificates.
  `/tmp/burst-governance-bootstrap-admission.log` passes when the proposal arrives
  at that boundary, including zero-rate reversal and restart with the unified
  startup restore. Added debug events distinguish received bootstrap responses
  from admission rejection. This establishes positive restart evidence, not
  delayed-history recovery. Boundary B now explicitly requires live certified
  governance followed by observer recovery beyond the timestamp window, while
  preserving rejection of uncertified backdating. C02/C10 remain blocked.
  `/tmp/burst-historical-admission-boundary.log` passes the controlled-clock
  60/61-second boundary, certificate-authenticated old-history admission and
  tamper rejection, plus strict node lint. This is component evidence; it does
  not close the live certified-governance journey.
- C03 publication lock order now matches ordinary governance admission:
  governance, delegation, constitution, BRN, TRST, then verification. Checkpoint
  installation and startup recovery previously acquired these in the reverse
  relative order. Their current sequencing limited overlap; no existing deadlock
  is claimed. `/tmp/burst-publication-lock-order.log` passes mixed governed
  verification recovery, live snapshot installation/replacement with local and
  peer proofs, publication crash recovery, related race/failure checks and strict
  node Clippy. This removes an inconsistent synchronization contract before
  lifecycle fork publication is added. It does not enable those forks or prove
  arbitrary concurrent lifecycle safety. Release gates remain closed.
- C02/C04 fork planning streams validated account rows instead of collecting a
  capped whole-ledger inventory. A storage fixture with 131,073 additional empty
  account records now produces the same dependency-closed rollback plan.
  Elected replacement collects only affected account heads before commit, then
  updates or removes those runtime frontiers under the existing locks. Unrelated
  frontiers are no longer rebuilt. `/tmp/burst-scoped-fork-publication.log` passes
  rollback safety checks, all six live replacement/restart journeys and strict
  node Clippy. Live assertions cover retained independent heads and removed opens.
  The large fixture tests inventory traversal, not a fully populated 131,073-wallet
  economic network. Provisional-block, undo-byte and economic restoration limits
  remain. Identity/governance branch replacement still requires lifecycle-aware
  rollback and publication; its restriction and all four release gates remain.
- C05/C10 live numeric governance now exercises three consecutive rate changes,
  `123 -> 0 -> 123`, through signed proposals, paid endorsements and ballots over
  RPC/P2P. A late observer joins before the zero-rate change. All four nodes
  activate both later proposals; existing BRN funds the reversal while accrual
  is zero. Restart preserves the restored rate, proposal outcomes and cumulative
  endorsement burns. `/tmp/burst-live-governance-reversal.log` passes the real-clock
  journey; `/tmp/burst-live-governance-reversal-clippy.log` passes strict lint.
  This uses the existing Dev bootstrap electorate, not independent post-bootstrap
  human quorum formation. It verifies reversibility and governance at rate zero,
  not every governable parameter, emergency path or full C10 completion. Initial
  public launch remains 1 BRN/hour. No production gate or monetary rule changed.
- C02/C07/C10 governed verification activation now passes live snapshot bootstrap
  and restarted payment admission. The source history counts and activates a
  timeout change before activating an already selected panel. Both live nodes
  expose the authenticated current parameter hash; recovered lifecycle state,
  activation records and panel context match. Payments use the governed hash,
  not genesis parameters. `/tmp/burst-governed-activation-live-corrected.log`
  records the passing journey. The initial failure in
  `/tmp/burst-governed-activation-live.log` was a fixture ordering error: it
  replayed across a counting cut before importing its certificate. The shared
  archive setup now imports and authenticates that certificate first.
  `/tmp/burst-governed-activation-integration.log` passes existing live challenge
  panel/stake and fraud/inactivity outcome recovery plus strict node Clippy.
  Source finality and time are supplied by fixtures. This is combined recovery
  evidence, not live proposal voting, real-clock onboarding, mixed-history
  longevity or full C07/C10 completion. All four release gates remain closed.
- C02 payment rollover now covers burns at heights 32, 64, 96 and 128, each
  received in the following checkpoint. `/tmp/burst-payment-rollover.log` passes
  component recovery. `/tmp/burst-payment-rollover-live.log` retains the initial
  90-second cold-start timeout and the unequal-clock TRST comparison failure.
  Repeated checkpoint signature checks now use a bounded 4,096-entry success
  cache keyed by the full payload, public key and signature. Roster, quorum and
  checkpoint-context checks still run on every call; misses use strict Ed25519.
  `/tmp/burst-checkpoint-signature-cache-checked.log` verifies context mutations,
  strict rejection, eviction and authority checks after cache hits.
  `/tmp/burst-payment-rollover-live-checked.log` passes both live catch-up cases
  under the unchanged readiness deadline, BRN/TRST equality at a common expiry
  clock, restart, new payment admission and strict consensus/node Clippy.
  The source fixture supplies finality, and those live nodes share a process.
  The strengthened cold-start journey now launches the receiver in a separate
  process with an unwarmed signature cache. Its first run failed the unchanged
  90-second readiness deadline; `/tmp/burst-isolated-retirement.log` retains the
  failure. Peer connectivity remained present, but the final epoch was not
  installed. `/tmp/burst-isolated-retirement-progress.log` shows zero accepted
  checkpoints throughout the diagnostic rerun. The subprocess setup omitted
  fixture protocol parameters because `NodeConfig` skips them during serialization.
  The child used defaults, so these failures do not establish a production sync
  defect. The corrected handoff carries parameters explicitly and has a round-trip
  regression check. `/tmp/burst-isolated-retirement-corrected.log` passes that
  check and independent-process cold catch-up at height 130 in 87.4 seconds,
  with the original 90-second deadline and desktop limits. It also passes
  graceful receiver shutdown, reopened BRN/TRST state equality, original archive
  retention, payment admission/restart and strict node Clippy. The first receiver
  does not share the source's signature cache. This is one successful run with
  little timing margin, not a latency guarantee or completion of C02.
  Distributed-quorum formation and mixed verification/governance longevity remain
  outside the passing payment-recovery evidence.
- C02/C03 account validation now decodes one account at a time from the caller's
  LMDB writer. Economic afterimage checks and runtime publication use that shared
  iterator. Frontier validation checks stored heads directly and checks the exact
  index count, instead of copying all accounts and frontier rows into a map.
  `/tmp/burst-streamed-account-storage.log` passes writer-view/abort behavior,
  malformed rows, mismatched keys, invalid confirmation heights, frontier
  corruption, and a positive 131,073-account scan beyond the old inventory cap.
  `/tmp/burst-streamed-account-integration.log` passes both live snapshot
  replacement journeys, sparse-source observer bootstrap and strict store/node
  Clippy. This removes these validation copies, not all recovery allocations or
  lifetime ceilings. Projection, undo reconstruction and signing coverage now
  also stream existing accounts and use direct lookups in the same writer.
  `/tmp/burst-streamed-account-projection.log` passes projection/rollback checks,
  the large-inventory undo check, both live replacements and strict Clippy.
  Its initial signing filter matched zero tests and is not evidence.
  `/tmp/burst-streamed-account-signing.log` passes the corrected five selection
  tests, including installed finality, vote promises and certified boundary
  signing. Retained-history and suffix validation now keep a streamed, count-bound
  account fingerprint instead of serialized account copies. Read/write scans
  share row validation. Finality reconciliation and suffix planning also stream
  accounts; the latter checks base membership without an address-set copy.
  `/tmp/burst-account-inventory-fingerprint-check.log` verifies changes in order,
  count and fields plus read-error propagation. `/tmp/burst-account-inventory-recovery.log`
  passes consistent-read behavior, retained proof/state checks, finality, live
  replacement/bootstrap and strict Clippy. `/tmp/burst-streamed-suffix-planning-final.log`
  adds same-count base-identity substitution rejection and repeats live recovery
  and strict Clippy for the final planning changes. Fingerprints only detect local
  changes; authenticated history still establishes authority. Pending validation
  now borrows individual rows, compares each claim with replay, and checks exact
  inventory/index counts. It no longer builds a second pending map or encoded
  whole-map comparisons. `/tmp/burst-streamed-pending-recovery.log` passes a
  131,073-row storage inventory, malformed/index-corrupt rows, missing/extra claims,
  amount/source/time/return-credit changes, payment-delta recovery and both live
  replacements. That run's lint failed on type complexity; the alias-only fix
  passes strict lint in `/tmp/burst-streamed-pending-clippy.log`. Individual-row
  bounds remain. No timeout or loss of claims was introduced under decision 9.7.
  Economic afterimage/runtime checks now compare persisted fields directly instead
  of encoding whole BRN/TRST states twice. `/tmp/burst-economic-state-comparison.log`
  verifies every persisted merger-graph field, wallet snapshot fields, permitted
  expiry-cache differences, rejection of changed rights, payment recovery and live
  replacement; strict TRST/node Clippy passes. Restoration now borrows TRST bytes
  from the same writer and decodes BRN rows incrementally. It retains the original
  row/byte limits and metadata validation. `/tmp/burst-borrowed-economic-restoration.log`
  passes borrowed writer-view/abort behavior, inventory limits, streamed decode
  failure propagation, payment recovery, both live replacements and strict
  BRN/store/node Clippy. These remove encoded copies, not the decoded engines or
  the normalized TRST comparison copy. No memory benchmark is claimed.
  `/tmp/burst-borrowed-economic-forks.log` passes all six fork-replacement journeys,
  including successive replacements, cemented dependencies and restart.
  `/tmp/burst-borrowed-economic-crash.log` passes actual SIGKILL at all five live
  replacement publication stages. Restart preserves durable state, old signing
  promises and payment admission. This validates the changed restoration path;
  it does not qualify unsupported fork kinds or every crash/recovery combination.
  BRN startup now uses one immutable read snapshot for metadata and borrowed rows,
  replacing separate reads and a collected encoded inventory. Reader and writer
  restoration share key validation. `/tmp/burst-brn-startup-snapshot.log` verifies
  a consistent old view across a new commit, the subsequent new view, malformed
  keys and streamed restoration. `/tmp/burst-brn-startup-integration.log` passes
  all eight startup/recovery journeys, both live replacements and strict
  store/BRN/node Clippy. The compatibility collection API remains available;
  production startup no longer uses it. No protocol or stored format changed.
  Retained-history planning now streams local bodies into the existing pinned
  scratch store; publication compares borrowed rows without copying the bodies
  again. `/tmp/burst-staged-retained-storage.log` passes sparse inventory,
  index corruption, limits and read/write-view checks.
  `/tmp/burst-staged-retained-integration.log` passes scratch corruption checks,
  local/peer-backed ancestry, forged-signature and changed-inventory rejection,
  both live replacements and strict Clippy. Scratch bytes grant no authority.
  Runtime frontiers, retained metadata, witness inventories, replay engines and
  the primary pending map still allocate. Existing count/byte limits remain;
  no memory benchmark or longevity sign-off is claimed. C02/C03 remain incomplete,
  with all four release gates closed.
- C15's isolated NAT acceptance suite passes on the dedicated Azure validation
  host with current wire-33 binaries. Evidence is retained under
  `target/validation-evidence/nat-20261014/attempt2`, including the terminal
  exit-0 journal, compiler and binary hashes, node logs and signed transaction
  reports. Source manifests are in the parent directory. Outbound-only NAT,
  firewall disconnect/reconnect, same-port and distinct-port clients, double
  NAT/CGNAT, manual inbound forwarding, UPnP renewal/recovery, crash lease expiry,
  reassigned mappings, alternate-port discovery and shutdown cleanup pass.
  `attempt1` retains the failures: peer readiness preceded RPC startup; failed
  double-NAT setup then contaminated dependent scenarios. A shared bounded RPC
  check now waits for every node on the expected network. The runner stops
  dependent cases when topology restoration fails. Focused readiness tests
  verify delayed startup, wrong-network and invalid responses; host guards and
  shell syntax also pass. Recovery listeners and protocol rules are unchanged.
  This proves the isolated transport scenarios, not physical-router coverage or
  distributed human consensus. Wallet journeys use one Dev representative.
  C15 remains partial. Other capability statuses are unchanged, and all four
  release gates remain closed.
- C15 reciprocal node authentication is implemented in the live connection paths.
  `/tmp/burst-mutual-handshake-before.log` first reproduces registration of an
  unproved listener identity. Generation 33 now requires role-separated Ed25519
  proofs over both node identities, fresh cookies, networks, parameter hashes,
  versions and ports. Registration follows proof verification in both directions.
  The unused one-way authentication branch in message dispatch is removed.
  `/tmp/burst-mutual-handshake-initial.log` passes the original regression and
  legitimate recovery-listener traffic. `/tmp/burst-mutual-handshake-integration.log`
  passes transcript/context mutation, inbound legacy-signature/identity/network/
  replay rejection followed by successful proof retrieval, stalled-frame retry,
  registry deduplication, wire tests, sparse-source bootstrap, both replacement
  journeys and strict node Clippy. `/tmp/burst-mutual-handshake-final.log` passes
  outbound missing/forged/changed-context proof rejection, all three actual SIGKILL
  recovery stages with restored content/lifecycle/payment behavior, independent
  `protoc` conformance, cookie-manager tests and final strict node Clippy.
  This closes the reproduced listener-identity gap under decision 23.17. It does
  not establish encrypted transport, consensus authority, full hostile-network
  qualification or real-router/NAT acceptance. C15 as a whole remains open, as do
  the broader completion ledger and all four release gates.
- C02/C15 outbound recovery now shares a bounded retry schedule across bootstrap,
  cache and discovery callers. Failures back off from 2 to 60 seconds; success
  resets the delay. At most 4,096 failed endpoints are retained for five minutes.
  A 15-second deadline covers the whole connection attempt, including the partial
  handshake body that previously had no timeout. `/tmp/burst-bounded-dial-recovery.log`
  passes retry progression/reset/expiry/capacity, a real TCP partial-challenge
  timeout followed by cooldown and redial, registry tests and strict node Clippy.
  `/tmp/burst-batched-record-crash-baseline.log` passes the existing three-stage
  SIGKILL journey before these changes. `/tmp/burst-bounded-dial-live-recovery.log`
  passes that journey after the changes, with snapshot-free restart, proposal
  content, lifecycle state and resumed payments at each stage. Sparse-source
  bootstrap and recovery-listener failure cleanup also pass. Inbound cookie
  limits, fixture deadlines and desktop limits are unchanged. These checks close
  the outbound partial-frame stall and uncoordinated-retry gaps, not all C15 work.
  Source inspection found that outbound peer identity is not reciprocally proved
  before registration. The C15 plan now prioritizes mutual authentication and its
  impersonation regression; no adversarial runtime result is claimed yet.
  Real NAT/router qualification and the broader release ledger remain open. All
  four release gates remain false.
- C02 exact committed-head retrieval now batches up to 64 independent cursors.
  Large bodies return a whole-head prefix within the existing 1 MiB raw-body
  budget; the client authenticates the complete prefix before requesting the
  remaining roots. Wire generation 32 adds the typed request. Signed blocks,
  authority rules, 16 KiB chunks, four server loads per second and desktop limits
  are unchanged. `/tmp/burst-exact-head-batches-checked.log` passes canonical-root
  bounds, large-body prefix serving, 64-head recovery and late-signature rejection.
  That run then fails an obsolete wire-version test expectation. After correcting
  it, `/tmp/burst-exact-head-batches-integration.log` passes wire codec tests,
  historical-witness tests including partial continuation and two positions of
  one account, live sparse-source bootstrap, both live replacements, all four
  record-only catch-up/restart journeys and strict node Clippy. The earlier
  compilation errors and failed expectation remain in their logs.
  `/tmp/burst-exact-head-schema-and-inventory.log` passes 129 committed heads in
  three bounded service requests, independent `protoc`/Rust schema agreement for
  every message including the new request, and strict node Clippy. The request-
  count fixture uses the transfer state machine, not TCP; the live journeys above
  provide the smaller network integration evidence. No C02 sign-off
  follows: large-network pacing, mixed-lifecycle interruption and budget-edge
  qualification remain open. All four release gates remain false.
- C02/C14 growing-state probe now reuses the signed-account export/transfer/import
  journey, with separate processes and ledger-local scratch storage. 4,000 and
  8,000 opens complete (reported process peak RSS 82,664 and 119,764 KiB).
  The original 16,000-account fixture filled its 32 MiB map; the 128 MiB probe
  fixture then accepted the checkpoint but export rejected its >8,192-block tail. Evidence is
  `/tmp/burst-snapshot-resource-{4000,8000,16000}.log` and
  `/tmp/burst-snapshot-resource-expanded-16000.log`. No 32,000-account run occurred
  because the batch stopped on failure. This moves bounded staged boundary replay
  ahead of further copy reductions in C02. Peak RSS includes the fixture, source
  database and retained archive, not a production node; C14 remains unqualified.
  The ordinary 4,000-account assertion now covers all added accounts; that check
  and strict node Clippy pass in `/tmp/burst-snapshot-resource-probe-checked.log`.
  Payment, verification and governance replay now share a fallible applied-block
  reader instead of requiring a borrowed history map. `/tmp/burst-applied-block-reader.log`
  passes owned/borrowed economic parity, absent/read-error propagation, real-state
  snapshot compatibility, settled rewards, proposal catch-up and replacement/restart;
  strict node Clippy passes. Compact replay now uses ledger-local anonymous files
  for applied bodies in local, peer-transferred and indexed-tail paths.
  `/tmp/burst-staged-applied-blocks.log` passes staged corruption/truncation and
  failed-append rejection, missing-directory refusal/cleanup, memory/disk economic
  parity, compact exports, 4,000-account transfer, rewards, proposal recovery,
  derived undo and replacement/restart. Strict node Clippy passes in
  `/tmp/burst-staged-applied-blocks-check.log`. The staged hash index still grows.
  The shared ordering engine now orders hashes with on-demand dependency reads;
  existing callers use its in-memory compatibility adapter.
  `/tmp/burst-hash-replay-order.log` passes exact sorted-root/dependency ordering,
  missing/cyclic/already-applied rejection, source-error propagation, economic
  snapshot compatibility, rewards, proposal recovery and replacement/restart;
  strict node Clippy passes. This is not a fix for the 16,000-account failure.
  Epoch replay now uses the hash-ordering callback directly. Selection and
  governance preparation scan a fallible memory/file input reader; governance
  retains dependency metadata rather than block bodies.
  `/tmp/burst-streamed-input-tests.log` passes scan-order parity, missing/duplicate
  input rejection, staged read-error propagation into both scans, compact export,
  4,000-account transfer, settled rewards, proposal catch-up, derived undo and
  replacement/restart. Strict node Clippy passes. Continuation now stages retained
  witnesses and new blocks directly, without a combined cloned vector. Deadline,
  outcome and governance activation read that staged input after complete replay.
  `/tmp/burst-streamed-lifecycle-tests.log` passes economic-state parity, governed
  rate activation and required ordering witnesses, then fails the TCP deadline
  journey with a stack overflow. Debugger traces in
  `/tmp/burst-streamed-lifecycle-stack{,-2}.log` locate large nested recovery
  futures. Recovery now selects its branch before boxing and boxes the public
  metadata future instead of embedding it in callers' async frames. Tests and
  resource limits are unchanged. `/tmp/burst-streamed-lifecycle-final.log` passes
  TCP deadline/outcome recovery, certified appeal continuation, rewards,
  4,000-account transfer, derived undo, replacement/restart and strict node Clippy.
  Tail planning now stages signed bodies and orders hashes without building an
  ordered body vector. `/tmp/burst-staged-tail-tests.log` passes suffix commitment
  and signed-byte pinning, required historical witnesses, dependent payments,
  verification spending, proposal validation, governance startup, compact export,
  4,000-account transfer, live proposal catch-up and replacement/restart. Strict
  node Clippy passes. Peer suffix collection now stages received account batches;
  legacy attached bodies are staged after decoding. `/tmp/burst-staged-transfer-tests.log`
  passes attached/missing-suffix import, authority/cache checks and live record-only
  catch-up with restart. Its crash journey passes suffix receipt but times out
  waiting for `checkpoint_content_received`. The diagnostic rerun in
  `/tmp/burst-staged-transfer-crash-diagnostic.log` passes all three SIGKILL stages,
  snapshot-free restart, proposal preservation and payment admission. It also
  records rejected same-IP handshakes until the source's rate-limit window resets.
  The initial timeout remains failed evidence. Reconnect timing under consecutive
  crashes is not qualified by the later pass and remains C02/C15 recovery work.
  Final strict node Clippy passes in `/tmp/burst-staged-tail-final-check.log`.
  Suffix retrieval now resumes an authenticated cursor in batches of at most
  128 blocks and stages each batch before requesting more. The cursor checks the
  complete opaque-witness sequence before changing position. In
  `/tmp/burst-batched-suffix-tests.log`, wrong-context/account, reordered,
  duplicate and repeated proofs leave the cursor unchanged; parent-height and
  cache separation checks pass, as does live full-proposal catch-up. The stronger
  transport fixture adds 8,200 signed blocks to one certified account chain.
  `/tmp/burst-batched-suffix-long-chain-checked.log` passes exact recovery through
  more than 64 batches without installing receiver blocks, plus strict node
  Clippy. This proves long-chain transport, not large-boundary export, installation
  or startup. The remaining collection owners are listed together in the plan.
  Startup now retains row lengths and digests instead of body copies.
  `/tmp/burst-indexed-row-observations.log` passes startup checks and strict node
  Clippy, including a work-nonce change that leaves historical membership valid
  but invalidates an earlier checked byte observation. Installation stages
  authenticated bodies beside the ledger and consumes fallible rows once.
  `/tmp/burst-single-pass-projection-node.log` passes reconciliation, complete
  payment/governance state parity, live replacement and strict node Clippy.
  `/tmp/burst-single-pass-projection-recovery.log` passes projection checks and
  all five SIGKILL replacement stages with preserved promises and restarted payments.
  A final guard prevents staged input from concealing a pre-existing missing
  indexed body. `/tmp/burst-single-pass-projection-checked-store.log` passes that
  regression, late-source-error rollback, successful owned-row installation and
  strict storage Clippy. An earlier validation attempt was refused below 4 GiB
  available RAM; a test-fixture field error was corrected before the passing run.
  Neither failure counts as passing evidence. The recovery run preceded this
  final corruption guard. `/tmp/burst-single-pass-projection-final-node.log`
  passes node projection and indexed startup on the final guard.
  Signing coverage now shares the checked tail's pinned storage and streams archive
  rows. Live successor installation also streams rows from that archive and a read
  view opened under the clean exclusive writer, without a body vector.
  `/tmp/burst-streamed-signing-installation.log` passes shared-file lifetime and
  read-failure checks, signer-journal reopen, atomic certificate persistence and
  four live catch-up/restart journeys including full proposal content. Its helper
  module filter ran zero tests and supplies no evidence. Strict Clippy then found
  the test module before production items; moving it fixed that layout error.
  `/tmp/burst-streamed-signing-lifecycles.log` passes deadline settlement, governed
  rate activation, certified appeal restoration/transfer, selection coverage and
  strict node Clippy. Historical proofs and proposal contents remain shared
  in-memory collections; sharing them does not remove their aggregate bounds.
  Finished historical witnesses now use pinned files with their original context
  and height. Discovery and recovery reuse that storage across cuts.
  `/tmp/burst-staged-historical-proofs-checked.log` passes exact proof reconstruction,
  corruption propagation into replay, dependency planning, payment/governance
  parity, undo recovery, live catch-up/restart and strict node Clippy. The first
  attempt was deferred below the desktop's available-memory threshold.
  Compact processing now shares full-history's 131,072-block / 64 MiB boundary
  budgets. Committed-head lookup validates each matching receipt once per batch,
  preserving exact positions, unknown-hash omission and conflict rejection.
  `/tmp/burst-aligned-boundary-4000-checked.log` passes the expanded installation/
  reopen journey and batched-head assertions after correcting a fixture type error.
  `/tmp/burst-aligned-boundary-16000-checked.log` and
  `/tmp/burst-aligned-boundary-32000.log` pass certification, export, transfer-state-
  machine checks, authenticated installation and reopened startup. Reported process
  high-water marks are 300,856 and 546,992 KiB; elapsed test times are 28.86 and
  58.06 seconds. These include fixtures/source/archive memory and do not qualify
  production performance. The reproduced 16,000-account export failure is fixed.
  `/tmp/burst-aligned-boundary-regressions.log` passes witness transport/rejection,
  live sparse-source bootstrap, live snapshot replacement and strict node Clippy.
  C02 remains incomplete: large live-network pacing, mixed-lifecycle/interruption
  qualification and maximum-budget combinations remain. Proof requests and
  proposal content still retain bounded in-memory collections. Wire batches,
  desktop resource limits and all release gates are unchanged.
- C02 TRST wallet and merger payloads now stream their existing binary formats
  into snapshot byte strings. Record preflight uses the same bounded field writers,
  without rebuilding encoded payload buffers. `/tmp/burst-streamed-trst-snapshot.log`
  passes TRST persistence/revocation/restoration checks, binary and real-state byte
  compatibility, canonical size/depth rejection, settled verification rewards,
  full-proposal catch-up/restart and live replacement/restarted payments. Strict
  node Clippy passes. `/tmp/burst-streamed-trst-transfer.log` passes the replayed
  4,000-account export/transfer/restoration fixture. Import now releases TRST/merger
  payloads immediately after decoding, and graph validation/wallet undo compare
  streamed bytes without a second encoded body. `/tmp/burst-streamed-snapshot-validation.log`
  passes mismatch/length/error handling, TRST restoration/undo checks, real-state
  byte compatibility, economic projection, settled rewards and live replacement
  with restarted payments; strict node Clippy passes. `/tmp/burst-streamed-snapshot-undo.log`
  passes recovery of payment deltas after expiry-cache changes without trusting
  local undo. Sorted ordering indexes, nested records and complete decoded
  state still allocate; the under-1-GB target is not qualified. C02 remains open,
  with the 64 MiB state bound and all release gates unchanged.
- C02 checkpoint continuation now hashes canonical state output without keeping
  an export byte vector, at genesis and after accepted boundaries. Byte-backed
  and streamed manifests share context/size validation; replay still validates
  state before deriving a commitment. `/tmp/burst-streamed-state-commitment.log`
  passes manifest equality against the byte-backed path, invalid commitment bounds,
  quorum/context binding, 4,000-account transfer, full-proposal catch-up/restart
  and live replacement/restarted payments. Operator export now prepares its body
  in an anonymous file beside the ledger and uses the same authenticated archive
  writer as byte-backed preparation. `/tmp/burst-disk-prepared-snapshot-checked.log`
  passes byte/signature equality, repeat export after destination failure,
  corrupted/truncated/extended file rejection, failed-encoding cleanup,
  4,000-account transfer, compact export and live replacement/restarted payments.
  `/tmp/burst-disk-prepared-daemon-cli.log` passes the freshly built debug daemon's
  attestation, minority refusal, quorum export, no-overwrite and offline verification
  journey. Installed-base persistence now reuses the disk-backed preparation and
  streams into its existing transactional pages. `/tmp/burst-disk-installed-snapshot.log`
  passes economic equivalence to full installation, dropped-projection rollback,
  stored-base reopen/corruption checks, reconciliation guards and live replacement
  with restarted payments. `/tmp/burst-disk-installed-crash.log` passes SIGKILL at
  all five replacement boundaries, exact old/new archive recovery, retained signer
  promises and restarted payments. Strict node Clippy passes. Byte-backed preparation and
  full-state import still allocate. The 64 MiB limit and all release gates
  remain unchanged; C02 is not complete.
- C02 byte-string fields now write existing payloads directly, without building
  byte-valued canonical trees. Lifecycle/proposal payloads are borrowed during
  export and owned after decoding; blob maps reuse the ordered map writer.
  `/tmp/burst-direct-snapshot-blobs.log` passes exact old/new byte equality across
  byte-string length boundaries, decoded ownership, output failure propagation,
  signed-body staging, real-state compatibility, 4,000-account transfer, settled
  verification/rewards and live replacement/restarted payments. Strict node Clippy
  passes. `/tmp/burst-direct-blobs-proposal-recovery.log` passes full-proposal body
  catch-up and restart; this is not full governance-lifecycle acceptance.
  TRST/merger serialization, sorted-key indexes, prepared bytes and decoded engine
  state still allocate. No bounded-state or C02 completion claim is made, and
  release gates remain closed.
- C02 top-level map and sequence fields no longer build collection-wide value
  trees. Maps sort encoded keys with borrowed value references, then normalize
  values individually. Sequences preserve order and normalize one item at a time.
  The schema selects these encoders for account, pending, BRN, lifecycle/proposal,
  endorsement, counterparty and historical-selection collections.
  `/tmp/burst-streamed-state-collections.log` passes old/new byte equality on real
  replay state, collection headers at 23/24 and 255/256 entries, tuple keys, u128,
  nested-depth limits, colliding encoded-key rejection and output failures.
  The 4,000-account transfer, active and settled verification recovery, and live
  replacement/restarted payments also pass. Strict node Clippy passes. Key indexes
  still grow with map size; nested records, blobs, prepared bytes and decoded
  engines remain resident. No lifetime-memory or full C02 sign-off follows, and
  all release gates remain closed.
- C02 canonical state encoding now normalizes one top-level field at a time.
  Export and both import consistency checks use that path. One schema declaration
  generates serde fields and field writers, so the two field lists cannot drift.
  `/tmp/burst-fieldwise-canonical-state.log` compares the new bytes against the
  old whole-tree encoder on authenticated replay state. The comparison passes.
  `/tmp/burst-fieldwise-canonical-integration.log` passes nested map/u128 and
  sequence encoding, the existing depth boundary, float/duplicate-key rejection,
  aggregate byte bounds and output failure propagation. The 4,000-account transfer,
  active committee/deadline recovery, settled verification/rewards and live
  replacement/restarted payments also pass, along with strict node Clippy.
  This removes the combined value tree, not all growth-dependent memory. Large
  individual maps still build trees; prepared bytes, blobs and decoded engines
  remain resident. No schema/signature change or release gate change was made.
- C02 export now borrows replay maps, rates, parameters, governance and historical
  selection records instead of cloning them into the serialization view. Import
  moves decoded BRN wallets/rates into the engine; persisted loading and snapshot
  restoration share metadata validation. Version-5 field encoding is unchanged.
  `/tmp/burst-borrowed-snapshot-state-checked.log` passes direct/persisted BRN
  equivalence across a rate change, retained burned/staked amounts, invalid
  metadata and duplicate-wallet rejection, plus authenticated economic-state
  round-trip against independent replay. The initial test assertion compile error
  is retained in `/tmp/burst-borrowed-snapshot-state.log`.
  `/tmp/burst-borrowed-snapshot-integration.log` passes the BRN suite, reconciliation,
  4,000-account transfer/failover, active committee/deadline recovery, settled
  verification/reward recovery, live replacement/restarted payment admission and
  strict node Clippy. This removes specific copies, not the full resident state:
  canonical value trees, prepared bytes, blob fields and engine data remain.
  No bounded-memory or C02 completion claim is made. All release gates stay closed.
- C02 economic-body staging now follows `read_in`'s supplied scratch directory.
  Bodies above 1 MiB use anonymous files there; a missing directory fails before
  body consumption and never falls back to process temporary storage. The legacy
  `read` API is unchanged. `/tmp/burst-state-staging-directory.log` passes signed
  small/file-backed body decoding, canonical comparison, corruption/truncation
  rejection, selected-directory placement, cleanup and startup download/recovery.
  `/tmp/burst-state-staging-transfer.log` passes the existing transfer/failover
  journey expanded to 4,000 real signed account opens, with an asserted economic
  body above 1 MiB and restored manifest/state checks. Strict node Clippy passes.
  The plan now names the remaining coupled copies in state extraction, canonical
  encoding, prepared bytes and engine restoration. No C02 completion follows:
  whole-state memory, the 64 MiB body limit, total storage accounting and full
  mixed-lifecycle large-state recovery remain. All release gates stay closed.
- C02 operator archive admission policy passes focused and node integration checks.
  `snapshot_transfer.max_archive_bytes` defaults to the previous 83,886,104-byte
  bound and reaches serving, fetching, startup and live installation. Codec and
  durable page storage no longer impose that fixed total history ceiling; frame
  and economic-body bounds remain. `/tmp/burst-archive-budget-focused.log` proves
  chosen-budget refusal before staging and compatible config defaults.
  `/tmp/burst-archive-budget-integration.log` proves transfer/restored state above
  the old ceiling, reopening 81 MiB of stored pages, existing rollback/digest checks
  and startup/payment recovery. The large authority fixture is synthetic, not a
  replayed identity lifecycle. `/tmp/burst-archive-budget-live.log` adds exact-budget
  startup download, live over-budget refusal, successful base replacement and
  payment recovery after lowering the admission budget to 25 bytes. Both install
  paths share the same bounded file reader and length-change checks. Strict node
  Clippy passes. Initial memory refusal and the corrected compile failure remain
  in `/tmp/burst-archive-budget.log` and `/tmp/burst-archive-budget-checked.log`.
  Validation ran only after memory recovered; no resource limits were changed.
  C02 still needs bounded economic state, complete resource accounting and
  mixed-history large-archive crash/recovery acceptance.
  No capability became complete and all release gates remain closed.
- C02 archive framing v3 is implemented for authority headers exceeding v2's
  16 MiB boundary. Small exports retain v2 bytes. Export, file-backed import and
  peer prefix authentication share bounded per-record decoding and the existing
  certificate file; the final snapshot certificate precedes body acceptance.
  `/tmp/burst-framed-snapshot-integration.log` passes export/transfer/restored-state
  equality with an authority header beyond 16 MiB, malformed/reordered/truncated
  frame rejection, sparse export, reconciliation and existing TCP/startup journeys.
  The large-header fixture uses synthetic quorum-valid authority changes. It does
  not prove a corresponding identity lifecycle from blocks or a large-header
  crash/replacement journey. Peer inactivity timing excludes local decoding time.
  `/tmp/burst-framed-snapshot-daemon.log` passes a freshly built debug daemon's
  real CLI verification of the large v3 archive and oversized-frame refusal,
  without creating a ledger directory. Strict node Clippy passes. All work used
  the desktop limiter. The 80 MiB aggregate archive limit, 64 MiB body and bounded
  certificate/beacon record remain unfinished architecture work; no gate changed.
- C02/C13 snapshot wire lengths and offsets are now `u64` in wire generation 31.
  The schema and Rust codec agree on values above 4 GiB and near the 64-bit limit.
  Every envelope and handshake rejects other generations; this does not rely on
  old peers truncating widened values. All participating peers must upgrade.
  Current archive admission still rejects oversized downloads before creating a
  staging file, and offset matching/serving checks happen before narrowing.
  `/tmp/burst-wide-snapshot-wire-integration.log` passes independent `protoc`
  conformance, protocol vectors, local admission, long transfer with failover,
  TCP/restart, live startup and strict node Clippy under the desktop limiter.
  The initial compile error is retained in `/tmp/burst-wide-snapshot-wire.log`;
  `/tmp/burst-wide-snapshot-wire-checked.log` retains the stale golden-vector
  failure. Malformed-payload fixtures were also moved to the current version so
  they still exercise payload rejection. Signed block and archive encodings are
  unchanged. The v2 archive ceiling and whole-state limits remain; no gate changed.
- C02 peer header authentication now reads the existing staged archive file.
  The separate header vector is removed. Incomplete headers read only the
  24-byte framing prefix; complete headers use the shared certificate-file
  decoder and do not consume economic-body bytes during authentication.
  `/tmp/burst-file-backed-prefix-authentication.log` passes framing and truncated
  input, long deterministic-clock transfer with peer failover, existing TCP and
  restart journeys, live download before admission, and strict node Clippy.
  The desktop limiter was unchanged. The aggregate v2 header ceiling, whole-state
  limits and coordinated new-format migration remain open; no gate changed.
- C02 transfer timing no longer discards a progressing download at 120 seconds.
  Peer inactivity still expires after five seconds. Startup renews its 120-second
  deadline only when successful handling increases received bytes, not on offers,
  retries, duplicate offsets or errors. `/tmp/burst-snapshot-progress-timeout.log`
  passes the timer decisions, cancellation, existing peer-transfer/restart cases
  and strict node Clippy. A later positive test in
  `/tmp/burst-snapshot-peer-resume.log` exports replayed state with 2,000 additional
  signed account opens and normal quorum attestations. Production transfer handlers
  receive its chunks four seconds apart on a deterministic clock, beyond 120
  seconds, with exact final bytes and restored accounts. After peer loss, they
  resume at offset 32768 with the same digest instead of redownloading the prefix.
  Duplicate chunks do not advance the transfer. The existing peer TCP/restart
  cases and strict Clippy also pass. This is not a wall-clock TCP throughput test.
  In-process failover retains the private file only after header authentication;
  failed candidates are bounded by the connected peer set. Exhausting those peers
  permits a fresh attempt. Persistent partial-file resume is an optional
  efficiency improvement, not an explicit requirement in decisions 11.1-11.3 or
  26.3. Earlier updates overstated it as a required blocker. Safe crash recovery
  remains required; this distinction does not open a release gate.
  `/tmp/burst-snapshot-progress-startup.log` passes live authenticated download
  before admission, installation and restarted payment admission. No gate changed.
- C02 v2 import now decodes authority into the shared anonymous certificate file
  one step at a time. Stored-base reads, startup, live installation and peer
  archive validation use it; recovery reuses the file instead of copying its
  history. The legacy in-memory API remains compatible. Trusted non-genesis bases
  remain valid, but installation still requires configured-genesis ancestry.
  `/tmp/burst-staged-authority-integration.log` passes exact v2 header encoding,
  fragmented reads, restored economic state, invalid/truncated input, sparse
  re-export, journal reconciliation, peer transfer/restart and live replacement.
  Replacement at five actual process-death boundaries restores payment admission;
  that test took 191 seconds. Strict node Clippy passes. All ran under the limiter.
  `/tmp/burst-staged-authority-import.log` retains the first compile failure from
  four missed callers; the corrected focused run is
  `/tmp/burst-staged-authority-import-checked.log`.
  `/tmp/burst-staged-authority-daemon-cli.log` passes a fresh debug daemon build
  and real attestation/export/offline-verification journey with the new reader.
  CLI verification uses anonymous scratch storage in its working directory and
  creates no ledger directory. The aggregate v2 header limit, peer prefix buffer, state-body
  limit and versioned long-transfer work remain. No capability or gate is signed off.
- C02 export CLI validation is complete for the certificate-file export change.
  `/tmp/burst-streamed-export-daemon-cli.log` runs the previously ignored real
  daemon journey against the fresh debug artifact recorded in
  `/tmp/burst-snapshot-daemon-build.jsonl`. Independent attestations, no-overwrite,
  minority refusal without output, quorum export, offline verification and stale
  height refusal pass. Verification creates no ledger directory. Both build and
  test used the desktop limiter. C02 archive framing, lifetime state limits and
  the other capability blockers above remain; no release gate changed.
- C02 sync initialization now authenticates the accepted tip from genesis when
  no snapshot exists, using the same record/witness replay as startup and the
  beacon worker. It rechecks record observations under the writer and rejects a
  snapshot appearing during preparation. The legacy full loader is test-only
  for this initialization entry point.
  `/tmp/burst-genesis-sync-authority.log` passes initialization after retired-record
  restart, damaged archive refusal, successful retry after restoring the original
  bytes, live two-epoch installation with restarted payments and strict Clippy.
  Transfer import now shares the installed-epoch mode decision with agreement.
  Without a snapshot it replays from genesis, accepts successors and checks
  repeated decisions against their replayed certificate and manifest. Late
  proofs are written only after rechecking replay observations under the writer.
  `/tmp/burst-genesis-transfer.log` passes successor import, duplicate import,
  economic installation and retired-history restart without a snapshot.
  `/tmp/burst-genesis-transfer-refresh.log` passes valid/invalid late-proof
  transfers, duplicate attachment, unchanged original bytes, reopen/publication
  and strict Clippy. A cursor now schedules archived records one body at a time.
  `/tmp/burst-genesis-transfer-integration.log` passes the non-live snapshot-history
  suite, including sparse cursor traversal, deleted-cursor continuation, malformed
  identity refusal, rollover recovery and strict Clippy.
  `/tmp/burst-transfer-sync-driver.log` passes chunk validation, backoff, later-tip
  scheduling and unsolicited-response checks. Incremental receivers no longer
  rebuild a full historical export just to compare a downloaded duplicate;
  the importer authenticates and compares the replayed decision instead.
  The live timeout is diagnosed. `/tmp/burst-readiness-source-pair.log` catches
  the source shutting down when snapshot installation changes the sync worker's
  captured view; the receiver then has no peer for its witnesses. Beacon replay
  has the same race during record retirement and late-proof attachment.
  Sync and beacon recovery now share checked current-tip replay. A changed
  authenticated base or explicitly stale read set restarts authentication.
  Missing bases and unchanged corrupt state remain errors. `StoreError::StaleRead`
  distinguishes optimistic-read conflicts from malformed stored data.
  `/tmp/burst-current-tip-stale-read.log` passes live delayed-proof refresh with
  retained and pruned source records, both-node restart and strict Clippy.
  `/tmp/burst-current-tip-integration.log` passes deterministic snapshot and
  late-proof changes during witness retrieval, corrupt-base/record/tip refusal,
  the non-live snapshot-history suite, live replacement of an existing snapshot
  using peer proofs, and strict Clippy. The diagnosed startup race is closed;
  C02/C09 as whole capabilities remain incomplete. Earlier diagnostic retries
  alone were not evidence of a fix.
  `/tmp/burst-current-tip-beacon-regression.log` also passes relay interruption,
  exact-round committee recovery, committed-change rechecks and corrupt-queue
  shutdown with preserved evidence. All validation used the desktop limiter.
  The deterministic fixture's earlier failures omitted pre-snapshot history or
  expected a witness request from a tail with no dependencies; those logs are
  retained as failures, not protocol evidence.
  Fresh and installed receivers request record-only transfers. Missing suffix
  blocks use certificate-bound witness cursors with parent-height floors; proposal
  bodies use a separate recovery pool that never queues live admission. Existing
  tail budgets apply before retrieval. Legacy attached bodies also pass semantic
  replay before the normal atomic certificate/history commit. Runtime authority
  selection and release gates are unchanged.
  `/tmp/burst-live-genesis-record-catchup.log` verifies snapshot-free cold start,
  matching lifecycle state and payments before/after receiver restart.
  `/tmp/burst-live-genesis-proposal-catchup-checked.log` adds proposal-body
  preservation under a certified roster cut, not a full governance voting lifecycle.
  The agreement relay is withheld in these journeys; checkpoint sync stays connected.
  `/tmp/burst-certified-suffix-integration.log` covers actual peer retrieval,
  parent-height limits, cache separation, fallback, timeout and authority recovery.
  `/tmp/burst-transferred-suffix.log` covers malformed attached bodies without writes.
  `/tmp/burst-recovery-content-integration-checked.log` covers multi-chunk archived
  content, unchanged metadata, no live admission and startup service gating.
  `/tmp/burst-record-only-default-regressions.log` covers malformed asynchronous
  import backoff, continuation requests, snapshot-based live catch-up and Clippy.
  Compatibility evidence remains in `/tmp/burst-requested-record-transfer-checked.log`.
  Earlier diagnostics remain in `/tmp/burst-genesis-record-bootstrap.log` and
  `/tmp/burst-recovery-content-fetch.log`; the latter's metadata assertion was
  corrected to compare initialized state before/after. The initial content
  integration log retains a missing test-import compile error. The initial live
  proposal log retains a fixture missing its signed content body. These failures
  are not passing evidence. `/tmp/burst-record-cold-start-crashes-checked.log`
  verifies actual SIGKILL at suffix receipt, proposal-content receipt and checkpoint
  acceptance. Each fresh receiver restarts without a snapshot, preserves full
  proposal content, matches lifecycle state and admits a valid payment. Clippy passes.
  The initial crash log failed because one case's payment propagated to the shared
  source. Each post-recovery payment is now isolated from that source; lifecycle
  comparisons remain unchanged. All validation used the desktop limiter.
  `/tmp/burst-live-handoff-progress.log` passes live genesis-to-130 catch-up,
  original archive bytes, snapshot absence, both-node restart and payments. The
  readiness window is unchanged. Its height trace shows continuous progress with
  slower catch-up as history grows; it is not a lifetime scalability sign-off.
  Sync wakes installation after acceptance and waits for local payment authority
  to leave the parent before another download. Peer backoff and authority checks
  remain. The importer and installer now share replay reuse, checking prior record
  and beacon bytes. Snapshot changes invalidate reuse; retirement is accepted only
  with byte-identical archived records. Provisional economics and journals are
  still reread. `/tmp/burst-shared-replay-cache.log` passes replay equivalence,
  relocation, changed active/archived bytes rejection without writes, and Clippy.
  `/tmp/burst-shared-replay-integration.log` passes fresh genesis-to-130 catch-up,
  original archive bytes, restart and payments with the same readiness window.
  Both mixed-history cold starts, snapshot replacement and SIGKILL at preparation,
  commit, economics, authority and completion also pass recovered payment admission.
  No compiler or validation service remains active.
  Prior evidence remains in `/tmp/burst-cached-handoff-mixed-regressions.log`
  for both smaller cold starts, full content, restarted payments and Clippy.
  The retired-history, cached-history and installation-handoff initial live logs
  retain readiness failures. The cached-history diagnostic was stopped after it
  exposed next-record imports racing local installation and paying peer backoff.
  Initial cache/handoff check logs retain fixed test-access and trait-import errors.
  Authority arrays, observation sets and whole-state/header bounds remain C02/C03
  blockers. No release gate changed.
  The next architectural slice is mapped in `COMPLETION_PLAN.md` across compact
  replay, installed metadata, signer journals and snapshot reconciliation/transport.
  Source inspection confirms unchanged rosters are already shared; no duplicate
  roster cache is needed. Historical authorities also protect old promises and
  electorate checks, so dropping old anchors without replacing those lookups is
  not a valid fix. The bounded history reader and streamed authority format are
  planned, not implemented or verified. The round-journal verifier now consumes
  a fallible authority stream instead of requiring an array. It reads one journal
  body at a time and checks decisions against the next authenticated epoch.
  `/tmp/burst-streamed-journal-checked.log` verifies preserved decisions and
  unfinished rounds, rejection of broken/truncated streams without metadata
  changes, existing snapshot-reconciliation preservation cases, and strict Clippy.
  `/tmp/burst-streamed-journal-recovery.log` verifies recovery from peer proofs
  and rejection of concurrent journal changes through the installed-state caller.
  The first run retained in `/tmp/burst-streamed-journal-authority.log` failed an
  empty-database assertion. The corrected test compares initialized metadata
  before/after; it does not change protocol acceptance. Callers still construct
  dense authority arrays and the journal-key inventory remains bounded by the
  existing storage limits. No full capability became complete, and no release
  gate changed. All checks used the desktop limiter and have finished.
  Accepted-record reconciliation and head staging now consume sequential authority
  too. `/tmp/burst-streamed-record-reconciliation.log` verifies the existing
  snapshot-preservation and archived-successor checks, staging above height 128,
  and unchanged metadata after a late stream error or malformed path. Strict
  Clippy passes. Active and archived records are checked against their historical
  parent before head staging. The installed base must occur on the path. Callers
  still retain dense arrays; fingerprints and active-key inventories also remain.
  This advances the planned reader integration without completing C02/C03/C16.
  Snapshot installation now uses an authenticated iterator instead of retaining
  the derived authority array. Configured-genesis checks remain, and all passes
  consume the final snapshot certificate. Archive authentication shares the same
  iterator. `/tmp/burst-snapshot-authority-iterator.log` verifies snapshot
  reconciliation, sequential authority through height 129, rejection of damaged
  intermediate and terminal certificates, and strict Clippy.
  `/tmp/burst-snapshot-authority-live.log` passes live replacement of an existing
  base with preserved accepted history. Validation finished within desktop limits.
  Startup recovery arrays, the whole authority header and other retained history
  still block lifetime-memory completion. No release gate changed.
  The compact recovery record reader now consumes sequential authority, matches
  selections in order and retains parent/child authorities only for active records.
  `/tmp/burst-streamed-recovery-records-checked.log` passes broken-input refusal,
  replay equivalence with byte-identical retirement and corrupted-record refusal,
  peer-proof journal recovery, both live genesis-only catch-up journeys including
  full proposal content, and strict Clippy. The earlier log retains a compile
  failure from one test caller missed during the interface change. Recovery still
  owns the historical array for other consumers, so C02 remains incomplete.
  All validation finished under the desktop limiter. Gates remain closed.
  Vote-counter and snapshot-attestation validation now walks authority once,
  resolving journal epochs and checking historical electorates without retaining
  anchors. `/tmp/burst-streamed-counter-authority.log` verifies preserved counters
  and attestations, missing/interrupted authority refusal without writes, installed
  recovery with preserved promises and a later decision, damaged/foreign journal
  refusal, peer-proof recovery with concurrent-change detection, and strict Clippy.
  Journal inventories remain resident under existing bounds. Closed-epoch/finality
  checks and metadata ancestry still use recovery's array. C02 remains incomplete;
  no release gate changed. The bounded validation job finished successfully.
  The remaining finality-promise and closed-epoch journal checks now use sequential
  authority passes. Unknown/uninstalled promise epochs remain errors, and closure
  destinations must occur after their source within installed authority.
  `/tmp/burst-streamed-finality-journals.log` passes promise preservation with a
  later local decision, damaged/foreign journal refusal, peer-proof recovery,
  actual process crashes through successor publication with resumed payments,
  and strict Clippy. Recovery still supplies its full historical array; replacing
  that ownership and the direct metadata ancestry access remains C02 work.
  All checks finished within desktop limits. No capability or release gate was
  marked complete.
  Recovery's authority array is now replaced by an anonymous certificate file
  beside the database. It stores roster changes once per changed epoch, reuses
  unchanged rosters, and reauthenticates each sequential reader from genesis to
  its pinned tip. Readers have independent offsets; no trusted anchors are
  deserialized. The OS releases the temporary file after all handles close, also
  after process death. This avoids a cache in memory-backed system temp storage.
  `/tmp/burst-disk-authority-integration.log` passes changed/unchanged roster
  traversal, interleaved readers, damaged/oversized/truncated frame refusal,
  replay equivalence, journal preservation, both mixed-history live cold starts,
  snapshot replacement and process-crash recovery with resumed payments. That log
  ends with a test-module placement lint failure, subsequently corrected without
  behavior changes. The first two logs retain a missing test import and a redundant
  closure lint failure. The 130-epoch live check and repeated strict lints are in
  `/tmp/burst-disk-authority-retired-live.log`; both pass. The fresh receiver reaches
  height 130 within the unchanged readiness window, preserves original archive
  bytes, restarts and admits payments. The complete fixture takes 200.55 seconds;
  that is not a catch-up-only timing or lifetime scalability sign-off. The service
  finished under the desktop limiter.
  Whole authority headers, selection receipts, observations and journal inventories
  remain resident. This change does not complete C02 or open any release gate.
  Snapshot serving and downloads now stage archives in anonymous files instead
  of whole-archive vectors. Downloads discard the header buffer after authentication.
  Serving stages beside the node database, preserving read-only source access.
  Final digest, archive authentication, fsync and no-overwrite publication remain.
  `/tmp/burst-file-backed-snapshot-transfer.log` passes transfer unit checks,
  live download before admission, SIGKILL at partial/prepared/published download
  boundaries with restarted payments, and strict Clippy. The crash journey took
  126.15 seconds; it was allowed to finish, not restarted during a quiet interval.
  `/tmp/burst-file-backed-snapshot-staging-checked.log` passes the final staging
  wiring, TCP transfer/discovery/restart, malformed peer archives, startup download
  and strict Clippy. The preceding staging log retains four missed test-constructor
  compile errors, corrected without changing acceptance. Header/aggregate bounds
  and 32-bit wire lengths remain C02 work. All checks finished within desktop limits;
  no release gate changed.
  Snapshot export now reuses replay's authenticated certificate file instead of
  collecting another full authority vector. The archive writer serializes steps
  individually; historical beacon checks share that source. The duplicate export
  collector and unused anchor-retention callback were removed. The emitted format
  and its header limit remain v2. `/tmp/burst-streamed-authority-export.log` passes
  independent attestations, peer transfer/discovery and full-node restart, plus
  strict Clippy. Its daemon CLI journey is ignored pending a freshly built executable;
  an initial commentary claim that it passed was corrected. It is not evidence.
  `/tmp/burst-streamed-export-sparse.log` passes sparse/restarted-base export,
  genesis replay equivalence, changed-history refusal and certificate-file corruption
  checks. These checks finished under desktop limits. New archive framing, reader
  integration, storage/wire limits and long-transfer progress remain C02 work.
  The next archive migration is now reconciled across export, decoding, paged
  storage, startup/live installation and wire transport in `COMPLETION_PLAN.md`.
  Source inspection found the same aggregate cap at each admission/storage layer,
  plus 32-bit wire lengths and a 120-second total transfer deadline. Changing only
  the header would leave valid long histories blocked elsewhere. The plan records
  compatibility, incremental quorum authentication, progress/resumption and atomic
  recovery acceptance together. This was a source audit and plan update, not a
  format implementation or new passing validation. No builds or tests ran for it.
  `/tmp/burst-archived-snapshot-reconciliation.log` passes snapshot installation
  over already archived successors, late-beacon preservation after reopen,
  missing/corrupt/wrong-height record rejection and changed-body rollback.
  Strict Clippy passes. Reconciliation authenticates each archived body against
  the incoming authority path without loading all bodies together.
  Compact acceptance now retires the oldest active records under the same writer
  as the new certificate and head. Count and byte limits remain unchanged;
  original archived bytes are preserved and conflicting archives abort acceptance.
  `/tmp/burst-automatic-record-retirement-integration.log` passes 130 real roster
  checkpoints with economic installation and no snapshot creation, archive
  conflict rollback at height 129, reopen recovery of epoch 130, historical
  record export and strict Clippy. This is a component integration journey,
  not a 130-epoch running-node or large-record byte-boundary test.
  The two earlier logs, `/tmp/burst-automatic-record-retirement.log` and
  `/tmp/burst-automatic-record-retirement-checked.log`, failed because the fixture
  omitted frontier and identity-binding updates. Recovery checks were preserved;
  the fixture now writes the representative/frontier and calls the normal
  identity-binding helper.
  `/tmp/burst-retirement-replay-regressions.log` passes the other snapshot-history
  tests, including operator rollover, late proofs, TCP agreement and witness
  retrieval. The already-passing automatic-retirement journey and live crash
  matrices were excluded; one existing test remains ignored. All checks used
  the unchanged desktop limiter. No validation job remains running.
  `/tmp/burst-live-automatic-retirement.log` fails source readiness at height 130.
  The native witness worker charged each successful local chunk the peer retry
  interval; the component witness service had not exercised that scheduling.
  Local progress now schedules another bounded poll immediately. Packet fairness,
  cancellation and peer backoff remain separate.
  `/tmp/burst-live-retirement-local-progress.log` passes the no-peer scheduling
  regression, then a live receiver starting with 128 active records, fetching
  and installing checkpoints 129–130, byte-preserving retirement, old-epoch
  payment refusal, valid payment admission, both-node restart and another payment.
  Neither node creates or installs a snapshot. Strict Clippy passes. Setup reuses
  the 130-checkpoint fixture and LMDB's consistent backup API at height 128.
  This verifies live count-bound retirement and restart, not large-record byte
  pressure or killed-process retirement boundaries.
  `/tmp/burst-local-witness-scheduling-regressions.log` passes authenticated local
  and peer retrieval, corrupt-local fallback, forged-membership refusal, cache
  bounds, cancellation, worker shutdown and packet/timer fairness. All checks
  used the unchanged desktop limiter; no validation job remains running.
  `/tmp/burst-retirement-byte-pressure-checked.log` passes the extracted production
  retirement transaction with 62 MiB of active payload and a 16 MiB successor.
  Heights 1, 2 and 10 move in numeric order; an archive conflict after earlier
  moves rolls back. Original bytes and unrelated metadata survive reopen.
  SIGKILL after retirement, after successor/head staging and after forced commit
  leaves the complete old or new state, never a partially retired inventory.
  These payloads are deliberately opaque storage fixtures, not large certified
  ledgers. The same job passes live record-only catch-up/restarted payments with
  and without a source snapshot, and strict Clippy. The first log,
  `/tmp/burst-retirement-byte-pressure.log`, retains a crash-test marker parsing
  failure; the marker check now handles the test runner's output prefix.
  The count/byte window and its transaction crash boundary are verified.
  Fresh-bootstrap full-history bounds and lifetime state/authority limits remain.
  No release gate changed.
- C02/C09 installed-selection publication now locates the installed certificate
  by its strictly increasing cutoff, instead of scanning a lifetime suffix under
  a cumulative 64 MiB limit. It reuses payment-authority validation inside the
  writer and checks the matching certificate's epoch, network, genesis, height,
  roster, quorum and parameter commitment. No new index or storage format exists.
  `/tmp/burst-installed-certificate-lookup.log` passes bounded-search tests through
  `u64::MAX`, real archived-selection publication, inconsistent descriptor refusal
  and strict Clippy. The synthetic height test establishes at most 64 probes;
  it does not authenticate an artificial ledger or benchmark large bodies.
  `/tmp/burst-installed-certificate-live.log` passes payment-authority checks,
  archived late-beacon publication after reopen, and live delayed-beacon refresh
  with retained/pruned source records and both-node restart. Full replay remains
  responsible for authenticating the accepted path; lookup does not replace it.
  `/tmp/burst-installed-certificate-replay.log` passes the remaining snapshot and
  lifecycle regressions. Live crash matrices and both 130-checkpoint rollover
  journeys were excluded; their paths were not changed. One existing test is
  still ignored. Desktop limits were unchanged and the validation job completed.
  Dense authority history and whole-state limits remain C02 blockers. Live
  count-bound retirement is verified above. No whole capability or release gate changed.
- C02/C09 late beacons can now be persisted separately from original archived
  selection bytes without requiring a snapshot. Recovery observes the separate
  proof, verifies its exact selection round and rechecks it before publication.
  Export reuses local economic replay when no snapshot covers the selection;
  snapshot-header construction remains separate from that replay.
  `/tmp/burst-archived-late-beacon-export.log` passes proof arrival after manual
  retirement, idempotent attachment, unchanged archived bytes, reopen, committee
  publication, authenticated export and corrupt-proof refusal. Strict Clippy
  passes. Earlier logs retain a test ownership compile error and use of the
  selection-only loader on a mixed chain; neither was passing evidence.
  `/tmp/burst-archived-late-beacon-integration.log` also passes export when an
  older snapshot predates the retired selection, the non-live snapshot-history
  suite including rollover, beacon-worker retry/restart checks and strict Clippy.
  Refresh scheduling, automatic retirement and lifetime bounds remain open.
  No release gate changed and no capability is complete.
- C02/C09 the live beacon queue now uses genesis-backed incremental recovery when
  no snapshot base exists. It no longer requires the legacy full-prefix loader
  merely because an operator has not created a snapshot. Persisted-selection and
  installed-epoch lookups accept original archived records with the same identity
  checks. The revote integration fixture retires its earlier selection, resumes
  the next panel and preserves prior votes/stakes and restart state.
  `/tmp/burst-genesis-beacon-queue.log` exposed the active-only publication lookup.
  `/tmp/burst-genesis-beacon-queue-fixed.log` passes that revote, then finds older
  beacon tests without a witness service. The broader worker run was stopped and
  is not a passing aggregate. Tests now connect authenticated witness retrieval.
  `/tmp/burst-genesis-beacon-runtime.log` passes exact-round retry, interrupted
  fetch, challenge restart and archived-selection revote. Its Clippy step found
  two needless closure borrows in test setup; those are corrected.
  `/tmp/burst-genesis-beacon-integration.log` passes strict node Clippy, live
  two-epoch installation with peer proofs and restarted payments, and the queue's
  corruption/committed-change shutdown checks.
  Late proof persistence/export was open at this check and is addressed above.
  Refresh scheduling, automatic retirement and lifetime bounds remain open.
  No release gate changed and no capability is complete.
- C02/C03 startup can recover original archived checkpoint successors when their
  active copies are absent. Records still pass height, head commitment,
  certificate and economic replay checks. Writer observations cover the archived
  bytes and absence of the corresponding active keys.
  `/tmp/burst-archived-genesis-recovery.log` passes missing/malformed/wrong-height
  archive rejection, positive successor recovery, no-snapshot restart after
  manually retiring two records, and rejection of changed archives or newly
  inserted active copies. The rollover fixture also removes its snapshot base,
  reopens storage and recovers the same height-130 state from genesis and archived
  records. Strict node Clippy passes. The fixture created snapshots on its way
  to height 130; it does not establish uninterrupted no-snapshot progress.
  `/tmp/burst-archived-recovery-regressions.log` passes the remaining non-live
  snapshot-history suite and live two-epoch installation with peer proofs and
  restarted payment admission. The separately verified rollover was excluded.
  Internal record retirement, bounded authority history and historical selection
  publication/refresh remain required before removing the active-record limit.
  No capability or release gate is signed off.
- C02/C03 record traversal now borrows inventory rows and retains fingerprints,
  not all active bodies or decoded successor manifests. Archive/genesis recovery,
  immediate-successor installation and historical export load bodies individually.
  Reload rejects changed or missing bytes; publication retains writer-side checks.
  `/tmp/burst-bounded-inventory-integration.log` verifies borrowed writer traversal,
  inventory substitution/insertion/deletion checks, compact persistence and record
  retirement. `/tmp/burst-record-reference-recovery.log` verifies borrowed pinned
  reads, exact row/byte bounds, successful successor reload and peer recovery,
  changed/deleted record refusal, the storage suite and strict node/store Clippy.
  `/tmp/burst-record-reference-integration.log` passes snapshot-history regressions
  outside the live module, including historical export after height-130 rollover.
  It also passes live two-epoch installation with peer proof retrieval and restarted
  payment admission, plus inventory ordering, hidden-record and count-bound checks.
  The full live crash matrix was not repeated for this read-path change.
  Snapshot reconciliation now also borrows rows, and staged retirement holds
  fingerprints before archiving one body at a time. The shared reconciliation
  pass checks authority and collects those fingerprints without a second bulk
  read. `/tmp/burst-borrowed-snapshot-retirement.log` passes reconciliation,
  changed/missing staged-body refusal, rollback after partial archival, late
  beacon retention after reopen, height-130 rollover and strict node Clippy.
  `/tmp/burst-borrowed-retirement-live.log` passes live snapshot replacement with
  peer-fetched proofs through that retirement path.
  No capability is complete. Active records remain limited to 128 and 64 MiB;
  dense authority, whole-state limits and all release blockers below remain.
- Requirement completed: none claimed. The planning/evidence structure was consolidated.
- New authoritative decision: owner selected initial 1 BRN/hour on 2026-10-13.
- Evidence checked: source hashes; four closed gates; authority storage/limits;
  fork type restrictions; pruning task; migration implementation; live sparse and
  delayed-beacon test assertions; real native-payment versus mocked browser entry.
- Blockers remaining: every R row in the plan. The first coherent implementation
  boundary is C02/C03/C09/C16 durable authenticated state, not another isolated helper.
- Inventory consistency and whitespace checks passed in
  `/tmp/burst-completion-reconciliation.log`. This verifies the inventory, not
  protocol behavior. No runtime suites were rerun merely to recount earlier passes.
  Storage/recovery checks passed in `/tmp/burst-paged-base-integration-fixed.log`:
  aborted and committed page replacement, pinned-reader consistency, corrupt/missing
  pages, legacy archive recovery, economic reconstruction and retrieval-race checks.
  The earlier compile typo and test-fixture nested-reader failure remain in the
  preceding development logs. Neither was a passing run.
- C02 storage step verified: persistence writes pages directly and recovery
  reads them as a stream. Async recovery retains a content commitment instead of
  whole archive copies, then rechecks the page contents under the writer.
  Legacy single-value archives remain readable. This does not change the wire format.
- C02 remaining implementation: authority headers and decoded state remain bounded
  whole objects. Byte pages are not independently authenticated authority pages. Streaming authority,
  historical-parent lookup, bounded successor replay and dependency-preserving
  compaction remain required. No height or archive limit was raised.
- `/tmp/burst-paged-base-live.log` finished with exit 0. It checks live installation
  and replacement with peer proofs, interruption at publication boundaries followed
  by restarted payments, sparse-relay bootstrap without the archival peer, and
  delayed beacon refresh after restart. Strict node library/test Clippy passed.
  The two crash journeys dominate this batch's runtime; do not repeat them for
  unrelated edits. These controlled fixtures are development evidence, not a
  production-clock or public-release attestation.
- Stream validation finished with exit 0 in `/tmp/burst-streamed-base-integration.log`.
  It checks fragmented writes/reads with one-page buffers, content changes without
  descriptor changes, economic and journal recovery, live installation/replacement
  with missing-history proofs, late confirmations, failed sync, sparse bootstrap
  and delayed beacon recovery. Strict node library/test Clippy passed.
  The previous SIGKILL matrix was not rerun for this stream-only change; its
  evidence remains tied to the preceding revision. No full capability or release
  gate is signed off by this batch. No validation job remains active.
  The default link was stopped without a test result; the direct `cargo rustc`
  attempt rejected multiple targets. Neither attempt is a pass.
  The replacement build finished in 39.68 seconds using temporary Cargo options
  `-Z profile-rustflags`, `profile.test.package.burst-node.debug=0` and
  `profile.test.package.burst-node.rustflags=["-C", "link-arg=-Wl,--threads=1"]`.
  Keep these options inside `tools/limited-run.sh`; they reduce linker work, not
  resource caps or test assertions. No repository-wide compiler profile was changed.
- C02 export now uses installed-state recovery when the requested epoch is the
  base or a successor. Offline ancestry uses the existing witness transfer
  verifier and budgets. Missing local witnesses fail; this command does not
  start a peer service. Corrupt installed state cannot fall back to full replay.
  `/tmp/burst-compact-export-development-fixed.log` verifies equal full/sparse
  manifests, unchanged storage during preparation, attestation, archive import,
  replacement and reopened export. The preceding development run failed on
  test access to private helpers; it is not a pass.
- `/tmp/burst-compact-export-integration.log` records snapshot-history recovery
  checks and strict node Clippy passing. Live-install/crash fixtures were excluded
  from this run. No complete capability or release gate is signed off.
- `/tmp/burst-compact-export-lifecycle.log` verifies the local witness path against
  the independent history walk in selected governance activation, roster
  delegation, deadline settlement, challenge outcome and appeal-restoration
  journeys. This is proof-retrieval/replay evidence, not whole-wallet or
  production-timing acceptance. Authority-page transfer, lifetime limits and
  dependency-preserving compaction still block C02/C16.
- C21 storage prerequisite: schema checks now run inside database open before
  writes. Missing schema metadata is accepted only for an empty recognized store
  or the existing marked pending layout. Known index rebuilds and schema pinning
  share one transaction. Unknown/corrupt markers and unsupported schemas fail
  without overwriting data; no schema-1 conversion is claimed.
  `/tmp/burst-schema-open-validation.log` verifies compatible reopen and retained
  data, version refusals, and rollback of both metadata and index writes when a
  rebuild fails. The LMDB library suite passes.
  `/tmp/burst-schema-open-integration.log` verifies sparse export/restart,
  runtime recovery, live economic publication and retained-record checks with
  the new open boundary; strict LMDB Clippy passes. C21 remains incomplete.
- C02/C16 record retirement now shares the snapshot installation transaction.
  The archive retains exact record bytes, historical serving/export can read
  them, and delayed beacon proofs remain available to installed-state recovery.
  `/tmp/burst-record-retirement-integration.log` verifies abort, commit/reopen,
  archive-conflict rollback, delayed beacon recovery and historical export.
  Snapshot-history checks, protected undo metadata and strict node Clippy pass.
  Progress without mandatory replacement snapshots, authority growth and physical
  pruning remain open. Decision 11.3 selects on-demand snapshot creation.
  No full capability is complete.
- The first broader live run was stopped after a failure. Its focused reproduction
  in `/tmp/burst-record-retirement-live-diagnosis.log` exposed genesis initialization
  overwriting schema 2 with schema 1. The duplicate write was removed; database open
  owns schema initialization. This belongs to C21 fresh-node restart, not a new
  requirement. Fresh-node restart passes in
  `/tmp/burst-record-retirement-live-fixed.log`. That run then exposed a beacon
  worker shutdown race. Rechecking cancellation must accept a lagged shutdown
  notification as well as a received one; the first fix missed this distinction.
  `/tmp/burst-record-retirement-live-final.log` verifies the corrected live
  challenge/deadline journey, restarted payments and fatal corruption handling.
  Snapshot replacement survives SIGKILL at preparation, commit, sync, economics
  and completion boundaries with exact retained records and signing promises.
  Both failed live logs are retained and do not count as acceptance evidence.
- `/tmp/burst-record-rollover-development.log` verifies 130 roster checkpoints
  using signed representative-change blocks, semantic compact replay, old-roster
  quorum certificates and authenticated snapshot installation. Periodic reopen
  retains the expected epoch and human authority; archived records remain servable.
  This crosses the former absolute height ceiling for that exercised path.
  It uses directly cemented fixture blocks and explicitly requested snapshots.
  Mixed verification/governance/payment longevity, sparse-peer recovery at that
  scale and bounded lifetime authority storage remain open. Earlier references
  to automatic rollover incorrectly implied mandatory snapshot creation.
  Continued progress must preserve the operator-triggered snapshot policy.
- Historical snapshot export now replays only through the requested old height
  when that height is in the installed snapshot's authenticated authority path.
  `/tmp/burst-historical-export-target.log` verifies the original height-1
  manifest and attestation after advancing to height 130. A newer current tip
  no longer makes that bounded historical export exceed the replay height limit.
  The target's own replay bound remains; this does not establish arbitrary-height
  historical export or unbounded authority storage.
  `/tmp/burst-historical-export-integration.log` passes sparse export/restart,
  corrupt-base refusal, retirement/recovery and strict node library/test Clippy.
- C02/C14 authority memory: immutable human rosters now share their delegation
  list and weight map across clones. Certified transitions with the same roster
  reuse that storage; changed rosters keep separate storage and old anchors retain
  their original weights. `/tmp/burst-shared-roster-consensus.log` verifies storage
  sharing, roster replacement, signature authority and the consensus library suite;
  strict consensus Clippy passes. Wire commitments and quorum rules are unchanged.
  Dense historical anchors and archive authority headers remain. This reduces
  duplicate allocations; it does not prove bounded lifetime memory or the hardware
  performance targets. `/tmp/burst-shared-roster-integration.log` passes the
  height-130 rollover/reopen/historical-export journey and snapshot-history suite,
  including sparse peer recovery and retained selection proofs. Strict node
  library/test Clippy passes. Live installation/crash cases were excluded from
  this immutable-memory-only change; no release gate is signed off.
- C02 recovery now retains historical anchors during the same authenticated walk
  that restores selection receipts, instead of walking and verifying the entire
  authority path again. Receipt, certificate and beacon checks remain in place.
  `/tmp/burst-single-authority-recovery.log` passes snapshot-history recovery,
  delayed-beacon authentication, height-130 rollover/export and strict node Clippy.
  Live installation/crash cases were excluded because persistence and publication
  did not change. This removes duplicate verification; authority storage limits
  and snapshot-independent lifetime progress still block completion.
- C02/C09 beacon recovery now observes proof keys only for authenticated selection
  receipts. Non-selection checkpoints no longer add empty beacon reads and writer
  observations. Receipt order and authority membership are checked before lookup.
  `/tmp/burst-selection-beacon-observations.log` verifies that a late proof
  insertion invalidates prepared observations, retired proofs survive restart,
  and sparse stored recovery still authenticates proofs. A 129-anchor unit fixture
  without selections keeps only the head and inventory observations; it is not
  a full protocol-longevity test. Strict node Clippy passes.
  `/tmp/burst-selection-beacon-live.log` passes live challenge/deadline archive
  progression, restart and payments. Selection history still grows, and this does
  not close the lifetime authority or required production-timing acceptance gaps.
- Decision 26.2 timeout mismatch resolved: runtime cleanup and dependency retry
  use the selected 15-minute unchecked timeout. Both missing-predecessor and
  missing-source buckets reject expired entries when drained, even before the
  next one-minute cleanup sweep. Duplicate queued arrivals do not renew their age.
  `/tmp/burst-unchecked-timeout-admission.log` verifies fresh dependency retrieval,
  exact-boundary expiration, duplicate handling and re-admission after eviction;
  unchecked tests and strict node Clippy pass. This does not sign off all of C15.
- C02 archive I/O now decodes headers from a length-limited stream and serializes
  borrowed header fields directly. Fixed 16 KiB I/O buffers replace the extra
  whole encoded-header allocation and export-only header clone. The decoded
  authority vector and state body still remain whole objects; no limit was raised.
  `/tmp/burst-stream-header-buffered.log` verifies fragmented header reads without
  whole-header read requests, truncation/padding refusal, sparse export/reopen and
  peer snapshot transfer checks. Strict node Clippy passes. The format is unchanged;
  this is allocation reduction, not completion of lifetime authority streaming.
- C02 canonical encoding now checks serialized size with a counting sink and
  constructs the sortable value tree directly. It no longer keeps an intermediate
  encoded source buffer solely to decode that tree. `/tmp/burst-canonical-allocation.log`
  passes legacy-byte equivalence for the integer/map fixture, full-u128 and blob
  bounds, sparse snapshot export/reopen and strict node Clippy.
  Import still authenticates the complete body hash before decoding state.
  A proposed hash-while-decoding path was removed because it moved untrusted
  parsing ahead of authentication. Safe body streaming needs an authenticated
  two-pass source or bounded staging; whole decoded state and lifetime limits remain.
- C02 archive import now stages bodies through fixed-size chunks, with memory
  storage up to 1 MiB and an owned temporary file above that threshold. The quorum
  certificate is checked before staging and the body hash before state decoding.
  Exact canonical-byte and semantic checks still run against the verified bytes.
  `/tmp/burst-authenticated-body-staging-fixed.log` passes small/file-backed staging,
  bad hash, truncation and invalid-certificate checks, snapshot-history recovery,
  height-130 rollover/export and strict node Clippy. The preceding compilation
  failure is retained in `/tmp/burst-authenticated-body-staging.log`, not counted
  as evidence. This trades temporary disk I/O for a bounded input buffer; decoded
  state, canonical output and authority history remain whole objects.
  `/tmp/burst-authenticated-body-live.log` passes live archive bootstrap with peer
  proofs and restarted payments. No archive limit or release gate changed.
- C02 snapshot validation now streams canonical encoding into an exact-byte
  comparator over the authenticated body. Both decoded-state and reconstructed
  continuation checks use this path without allocating whole canonical-output
  buffers. Export and import share the same state construction and encoder.
  `/tmp/burst-canonical-stream-comparison.log` records passing snapshot recovery,
  retirement, height-130 rollover and transfer checks, but the batch failed on a
  new fixture's stale raw-length assertion. It is not a passing suite record.
  `/tmp/burst-canonical-stream-comparison-fixed.log` passes the corrected canonical
  and memory/file-backed body tests, live archive bootstrap with restarted
  payments, and strict node Clippy. Comparisons reject changed, shorter and longer
  encodings and can rewind after failure. The file-backed fixture also decodes a
  CBOR blob; it is not a large whole-economic-state benchmark. Decoded state and
  canonical value trees remain allocated. Lifetime authority, snapshot-size and
  operator-independent progress requirements remain open; no capability or
  release gate is signed off by this allocation change.
- C02 operator export now starts economic continuation from locally configured
  genesis and applies certified suffixes through the same engine used by installed
  snapshots. It does not require or create a snapshot base. The certificate loader
  still reconstructs full-history events; admission and restart ceilings remain.
  `/tmp/burst-genesis-incremental-export-fixed.log` passes equivalence against full
  replay at each fixture checkpoint, exact final state bytes, unchanged storage,
  sparse successor export/reopen and corrupt-base refusal. The initial compilation
  failure is retained in `/tmp/burst-genesis-incremental-export.log`.
  `/tmp/burst-genesis-export-integration.log` passes the snapshot-history suite,
  including bootstrap/download/live-install/replacement crash matrices, for this
  change. Its subsequent Clippy step failed because the next export-reader
  refactor was not yet connected. The overall job is not a passing check.
  No capability or release gate is signed off. Decision 11.3 remains on-demand
  snapshot creation; internal incremental replay is not a new snapshot policy.
- C02 historical export no longer constructs full-history checkpoint events.
  It reads archived records one at a time, authenticates each successor and
  replays its suffix from configured genesis. Active inventory and all consumed
  record/beacon observations are rechecked under the writer before returning an
  export. The unused full-history archival loader was removed.
  `/tmp/burst-streamed-historical-export-fixed.log` passes export equivalence,
  sparse export/reopen, corrupt-base refusal, changed-input observation checks,
  and reconstruction of the height-129 manifest after height-130 installation and
  reopen. Strict node Clippy passes. The initial attempt kept an LMDB read
  transaction open across replay and failed with `MDB_BAD_RSLOT`; its log is
  `/tmp/burst-streamed-historical-export.log`. Short-lived reads plus final
  observation checks replace that invalid transaction nesting.
  This closes the historical-export height-128 refusal, not runtime longevity.
  Active inventory still has count/byte limits; decoded state and the authority
  path still have whole-object limits. No snapshot policy or release gate changed.
  `/tmp/burst-streamed-export-replay-regressions.log` passes the remaining
  snapshot-history regressions, including lifecycle boundaries, delayed beacon
  recovery, record retirement, TCP agreement and witness transfer. Previously run
  live crash matrices and the separately verified rollover were excluded.
- C02/C03 startup recovery now has a genesis-backed source for an installed
  non-genesis epoch when no snapshot archive exists. It reuses certified suffix
  replay, metadata/economic checks and journal reconciliation. Archive absence is
  observed and rechecked before publication. Archive-backed recovery retains its
  existing commitment check; a corrupt archive cannot select the genesis path.
  `/tmp/burst-genesis-recovery-no-incremental.log` passes the new test. The test
  test covers the exact installed epoch despite a later accepted record,
  immediate-successor installation, reopen/state equivalence and rejection when
  an archive appears during preparation. The earlier runs in
  `/tmp/burst-genesis-installed-recovery.log` and
  `/tmp/burst-genesis-installed-recovery-boxed.log` were stopped after repeated
  soft-memory throttling, not counted as passes. Disabling test-profile incremental
  compilation finished within the unchanged limits in 92 seconds. The recovery
  source future is boxed; no memory, CPU or swap allowance was raised.
- C02/C03 checkpoint service mode now follows an installed non-genesis epoch,
  not mandatory archive presence. A later archive handoff stops the old worker
  before source revalidation and resumes only from checked state. A corrupt
  replacement or disappearing archive cannot leave that signer running.
  `/tmp/burst-genesis-runtime-integration-fixed.log` passes authority rotation,
  no-archive recovery/successor/reopen, archive handoff and corrupt-base refusal,
  live bootstrap with restarted payments, and strict node Clippy. The earlier
  `/tmp/burst-genesis-runtime-integration.log` found a swallowed handoff error and
  an obsolete expectation that unprovable non-genesis state returned None; both
  are corrected. Runtime mode selection also passes its focused check there.
  Active inventory and state/authority limits remain; this does not prove
  indefinite progress or the full release. No release gate changed.
  `/tmp/burst-genesis-recovery-regressions.log` passes the other snapshot-history
  regressions but fails the old missing-archive expectation. The corrected test
  rejects unprovable installed epochs and preserves empty/configured-genesis
  startup behavior. It passes in `/tmp/burst-genesis-recovery-regressions-fixed.log`,
  together with live two-epoch installation, peer proof retrieval, restarted
  payment admission and strict node Clippy. The failed aggregate run remains
  recorded; expensive crash matrices were not repeated for this change.
