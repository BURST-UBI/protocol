# BURST Protocol

A parameterized economic protocol where every monetary distribution model is a configuration.

BURST is defined by two parameters:
- **r** — the BRN accrual rate (universal income speed)
- **e** — the TRST expiry period (how long earned currency remains transferable)

Normal money is BURST with r=0 and e=∞. Every other economic model is a different configuration.

## Quick Start

For the current source comparison and tested protocol iterations, see
[Reference implementation review](REFERENCE_IMPLEMENTATION_REVIEW.md).
It distinguishes completed changes from the remaining whitepaper gaps.
The [conformance inventory](spec/README.md) indexes both source documents,
records resolved source conflicts and lists the evidence still needed.
An indexed section is not an implemented or verified feature.

This build uses block version 6 and wire version 30. Every ordinary block signs
its payment epoch, initially the configured genesis hash. Signed endorsement
and returned-stake dependencies, precomputable work and block-size limits remain.
Wire version 30 uses [typed Protobuf messages](protocol/wire.proto) and rejects
older wire generations. Signed transactions retain the Bincode format selected
in decision 8.2. The 64 KiB frame-body limit still applies. Typed checkpoint
agreement and authenticated history transfers remain supported.
Outcome installation and payment-authority transitions remain gated.
The node cannot open an older block layout or peer
with an older wire generation. For isolated testing, use a
separate fresh development data directory and rebuild the node and apps together.
Existing data is not migrated or deleted automatically. See the
[decision record](spec/DECISION_RESOLUTIONS.md) before restarting an older setup.
Bootstrap exit now has a durable one-way phase record. Existing ledgers without
that record require explicit authenticated replay; the node cannot infer past
creator authority from the current wallet count. No automatic migration exists.
After stopping an older local node,
`./dev-user.sh --data-dir .dev_user_node_v4_stake_release` starts a separate ledger
and preserves older development directories.

### Returning to development (October 2026)

The previous Azure VMs have been deleted. Treat old IPs and deployment output in
this repository as historical. A new, isolated three-VM Azure **development
validation network** has been provisioned with the owner's permission (see below).
It does not recover the old network or constitute a public/production launch.

From this directory, in three separate terminals:

```bash
./dev-user.sh                   # persistent local dev node, RPC :7077
```

```bash
cd ../wallet
npm ci
npm run tauri dev
```

```bash
cd ../node-gui
npm ci
npm run tauri dev
```

Both apps require the Tauri desktop runtime for RPC/key operations. `npm run dev`
alone only starts a browser UI preview. `dev-user.sh` retains `.dev_user_node`
across restarts; it no longer silently resets the ledger. Its faucet is a local
testing convenience, not proof that the holder is a unique human. Funding now
uses real signed endorsement/burn blocks, not node-local balance rewrites.

Run local validation through the resource limiter, one job at a time. The
network journey consumes prebuilt artifacts; it does not rebuild them or start
an optimized compiler. For example, with qualified debug artifacts:

```bash
bash tools/limited-run.sh bash ./run-validation.sh --artifacts-dir "$PWD/target/debug"
```

The cluster uses fresh temporary data directories and retains logs for diagnosis.
Build the daemon and the `validate_network`, `validate_restart_payment` and
`validate_pending_chain` wallet-core examples first. Use an appropriately
provisioned validation host for optimized artifacts or checks that exceed the
desktop limits. Pilot acceptance is tracked in `spec/TESTNET_PILOT.md`.
`--smoke-only` exits nonzero on a failed check and reports skipped checks separately.
Faucet-created state now replicates through the normal block pipeline. The focused
`test-test-tools.mjs` suite checks exact account/portfolio/pending agreement,
hard restart, and fresh-peer replay; a simple endpoint smoke check alone does not.

Validated on 2026-10-10: **1,196 protocol tests passed**, with one ignored doctest.
The equal-person policy replaces the previous hybrid/bonus-specific tests with
equal-weight, delegation, eviction, and zero-electorate safety regressions.
The non-faucet validation harness exercises genesis endorsement, real BRN burn,
pending TRST, explicit receive-open, rejection of unverified spending, verified
transfer/receive, provenance-preserving merge, delegation/revocation, and exact
agreement on frontiers, balances, BRN components, portfolios and confirmations.
The same journey passed on three local nodes and three Azure VMs. Forced process
termination/restart retained the checked state; a fourth fresh node reconstructed
it. These are recent-ledger tests, **not proof of historical bootstrap or fork
recovery**. The harness retains its temporary data and logs for investigation.

Earlier Linux Tauri/WebKit tests (not mocked IPC) exercised 24-word mnemonic
creation/restoration, encrypted backup/reload, wrong-password rejection, backup
non-overwrite, locking, live Azure balance/verification queries, signed
payments and receipt, sender-filtered auto-accept, automatic merge, transfer of a
merged output, the onboarding backup gate, and 14 dashboard RPC contracts.
Native dashboard navigation and live telemetry, peer, constitution, explorer and
merger views also passed. The latest frontend pass builds both apps and passes
11 wallet math/icon tests, 52 wallet browser regressions and 18 node browser
regressions in both Chromium and an isolated Linux WebKit runtime. These use
fixture IPC and do not retest native payments or establish macOS/iOS behavior. Current
protocol and native-build evidence is recorded by iteration in
`REFERENCE_IMPLEMENTATION_REVIEW.md`. These checks do not cover every feature in
the whitepaper.

For each app, `npm run build` checks the frontend and `cargo check --manifest-path
src-tauri/Cargo.toml` checks the native backend. Browser regression tests are in
`wallet/tests/dashboard.test.cjs` and `node-gui/tests/overview.test.cjs`; run with
their Vite servers running and Playwright available (or set `PLAYWRIGHT_MODULE`
to an installed Playwright module; `CHROMIUM_PATH` selects an existing Chromium).
They use explicit IPC fixtures, not real keys or native transaction execution.
`wallet/tests/brn.test.mjs` additionally checks exact integer amount/accrual math.
On this Arch host, `bash tools/prepare-webkit-compat.sh /path/to/cache/webkit-2272`
copies the cached browser to a fresh temporary directory and supplies pinned,
checksum-checked compatibility libraries. It prints the `PLAYWRIGHT_BROWSERS_PATH`
to use with `BROWSER_ENGINE=webkit` and the existing browser test commands.
It does not install packages or change the system libraries or source browser.
Native tests are `wallet/tests/native-{wallet,payments,onboarding}.mjs` and
`node-gui/tests/native-{rpc,dashboard}.mjs`. They require running app Vite servers, a real
`tauri-driver` session (`WEBDRIVER_SESSION`), isolated test app-data directories,
and the private development RPC tunnel on localhost:29077. They create disposable
test wallets; never point them at a personal wallet session or production ledger.

### Test funding versus real verification

The whitepaper's **Genesis: The Identity Bootstrap** and decisions **15.2,
27.1–27.2, 34.10** describe two different capabilities:

- **Real protocol:** the creator starts verified and can endorse the first
  wallets during the genesis bootstrap window (default exit threshold: 50
  verified wallets). Afterwards, ordinary endorsement and verifier voting rules
  apply. Merely running a node or holding an arbitrary verified wallet does not
  give it the creator's authority. Live genesis signing requires the matching
  private seed; Dev and Test have distinct, published disposable seeds.
- **Test convenience:** `faucet`, `wallet_create_full`, and the `*_simple` /
  eviction proposal helpers require **both** Dev/Test and explicit faucet opt-in.
  Live refuses them even if the daemon was started with `--faucet`; RPC handlers
  also independently check the actual network. Ordinary signed BRN burns,
  TRST transfers, and protocol-defined rewards are not test-only operations.

Data directories are pinned to a network before node subsystems start. Existing
unmarked ledgers must contain that network's valid genesis before being adopted;
changing `--network` cannot turn a Dev/Test database into a Live ledger. A mismatch
is refused without relabelling the ledger. Use a separate directory per network.

`faucet` burns 1 BRN from the test genesis account to send 1 TRST. It does not
verify the recipient or choose a representative on the recipient's behalf.
It returns `status: "pending_receive"`, `send_block_hash`, and `amount`, only
after confirmation. The recipient must sign its Receive (or use its configured
wallet auto-accept policy). Unverified wallets can receive but cannot spend.
Repeated funding does not change verification or reset BRN history. Funding
remains available after bootstrap closes; verification still follows the normal
protocol. There is no perpetual verifier bypass or hidden representative bonus.

For manual testing, run `bash dev-user.sh`, create a disposable wallet in the
desktop app, and call the local faucet with **that wallet's public address**:

```bash
curl --max-time 35 http://127.0.0.1:7077 \
  -H 'Content-Type: application/json' \
  -d '{"action":"faucet","account":"YOUR_DISPOSABLE_WALLET_ADDRESS"}'
```

Accept the pending payment, then choose a running representative in wallet
Settings. During bootstrap, request `genesis_endorse` for the wallet's public
address from the creator node. Verify only after that signed representative
choice has confirmed. A newly verified human contributes to quorum even when
their representative is offline; verifying an unopened wallet that cannot vote
can stall a small test network. After bootstrap, normal certified verification
is required and remains gated until integration is complete.

Do not export the wallet's private key for faucet or endorsement requests.
`send_simple` requires an explicit `origin`
from the portfolio; test RPCs no longer manufacture provenance or write ledger
balances directly. They wait for confirmation; a timeout includes the submitted
hash, so inspect it before retrying. Keep RPC loopback-only: test tools and the
genesis authority are powerful even on a disposable network.

The unsigned legacy `governance_vote` RPC is rejected on every network. The real
wallet's signed `GovernanceVote` → `process` path remains available. Governance
test helpers no longer mutate a second local engine after submitting a block.
Proposal detail and listing read the same persisted engine, and proposal creation
uses the signed block timestamp so fresh-peer replay retains its original schedule.

Both apps display the node-reported Devnet/Testnet/Mainnet and faucet status.
Older nodes that omit this metadata are labelled unknown, never assumed mainnet.
These source changes are local; the Azure section below identifies the older
deployed artifact until a separate deployment is performed.

The table below records the early development-validation pass. Several listed
gaps now have gated implementations and later evidence. Use the
[current implementation boundaries](spec/README.md#known-implementation-gaps)
for remaining work, not this historical table.

| Area | Exercised | Still needed for release |
| --- | --- | --- |
| Test isolation | Live denial, Dev/Test opt-in, authority absence, cross-network peer and data-directory rejection | Public-service abuse/rate-limit testing |
| Payments | Real burn, send/receive, provenance, merge, delegation; peer agreement and recovery | Adversarial forks and all mid-commit crash points |
| Economics | Unit/property coverage for accrual, expiry, splitting, revocation | Distributed rate/expiry/revocation lifecycle |
| Humanity verification | Genesis bootstrap and verifier opt-in/out; orchestrator unit tests | Full non-genesis selection, voting, challenges and re-verification across nodes; wallet verifier-voting controls |
| Governance | Engine tests and signed proposal replication/replay | Complete distributed voting/activation, phase-policy reconciliation |
| Consensus | Equal-person weight, ordinary confirmations, restart regression | Confirmed-only weights, durable final votes, complete fork rollback |
| Networking | Local mesh and isolated NAT/firewall/UPnP suite | Physical-router compatibility, long partitions, public-network soak |
| Apps | Frontend builds, native backend checks, wallet math and UI contracts | Full native lifecycle coverage for remaining features |

Focused test evidence for this pass: `/tmp/burst-test-tools-TrR6BR` (network
boundaries, concurrent grants, proposal/verifier actions, forced restart and
fresh-peer replay), `/tmp/burst-validation.agPt3hb2` (signed wallet journey and
new payment after restart), and `/tmp/burst-dev-cluster.cAeOMqop` (11 smoke checks,
zero failed or skipped). Workspace output is
`/tmp/burst-test-tools-workspace-final.log`; Clippy completed with warnings, not
a warning-free result. These are disposable local test artifacts, not wallet
backups or a claim that the remaining release gates are complete.
The NAT/firewall/UPnP regression run finished with zero failures; evidence is in
`/tmp/burst-nat.AUYGWwAc`. That portable build preceded the final proposal-readback,
proposal-timestamp, and data-directory guards; those later changes were checked
by the local workspace and focused replay/isolation suites, not redeployed to Azure.
Run the focused suite with the optimized daemon as shown above: an initial debug
run exceeded its 25-second RPC budget during transaction testing. The final
optimized run passed with a 45-second RPC budget; it is not a latency benchmark.

### Earlier alignment audit (October 10, 2026)

This audit predates the certified lifecycle, payment-authority and concurrency
work in the [iteration log](REFERENCE_IMPLEMENTATION_REVIEW.md). Its descriptions
of missing code are historical. Source release gates remain closed; the
[conformance inventory](spec/README.md) records current boundaries and evidence.

The subsequent infrastructure safety pass adds regression coverage for LMDB
map exhaustion, candidate retry after failure, confirmation-height preservation,
atomic deletion of both block-height indexes, bootstrap request/peer/chain
correlation, read-only expiry checks for uncommitted candidates, and governance
activation previews that cannot enqueue effects.
Deleting an old block also preserves a replacement block's height index.
Primary block/economic writes now abort on every write error; BRN/TRST changes
are staged in isolated engine copies until that commit succeeds. Candidates do
not publish their in-memory frontier or dedup entry before persistence.
Bootstrap cursors advance from persisted state, never just from received or
queued data; failed/dropped batches retry without skipping blocks. Frontier
pages are correlated and ordered, while zero-head verified accounts remain valid.
Activation-hash validation cannot leak rejected changes into the propagation
queue, and subsequent blocks use the updated configured TRST expiry period.

Those infrastructure changes preserved existing block formats, signatures, consensus weights,
governance phases, genesis authority, and BRN/TRST rules. They do **not** complete
the release gates below. In particular, activation previews being side-effect
free does not fix timer-driven governance activation or the split-commit gap.
The Azure deployment described below now includes this safety pass and the
owner-approved equal-person consensus policy. It remains a private devnet.

The economic implementation is recognizably the intended BURST, but the system
is **not yet production-ready**. The wallet computes BRN from historical rates,
burns, stakes and paused verification intervals. Native builders use the block's
cumulative BRN-spent odometer, not the RPC's spendable balance. TRST remains a
two-phase send/receive asset with retained provenance and reversible historical
expiry/revocation. Wallet auto-accept is enabled by default; default merge
tolerance is 10% of remaining lifetime, and merged outputs take the earliest
expiry. A pending challenge does not revoke normal spending (whitepaper 17.3).

The website's [TRST valuation](https://www.brst.cc/docs/economy) explicitly leaves
pricing to the market: expiry-adjusted estimates must not silently reduce a
token's ledger face amount. Its [merge description](https://www.brst.cc/docs/token_operations)
requires earliest expiry and proportional fraud revocation. Both are intentional,
not bugs to simplify away. The older [comparison page](https://www.brst.cc/docs/comparison/circles)
suggests pruning expired history; that cannot be applied blindly alongside the
whitepaper's reversible expiry, re-verification and historical reputation rules.

The audit recorded the following release gaps at that point:

1. **Historical bootstrap:** normal ingress rejects blocks over an hour old.
   An authenticated historical confirmation/checkpoint path is required. Simply
   disabling timestamp checks would permit backdated expired-TRST spending.
2. **Atomic application and fork recovery:** ledger/economic persistence and
   subsequent verification/governance/delegation effects still span commits.
   Completed-block restart tests do not cover every mid-application crash.
   Competing-fork rollback/reapplication of cross-account economic effects needs
   adversarial acceptance tests and completion. Full economic snapshots per block
   also need incremental persistence before scale testing.
3. **Governance and verification lifecycle:** prove challenge/appeal/reverification,
   rate/expiry changes, proportional revocation and governance activation across
   nodes, not just unit tests. Resolve node-local timing and ensure convenience
   RPC mutations cannot bypass consensus. Dashboard proposal listing is not proof
   of a complete governance lifecycle.
4. **Remaining wallet intent:** sender allowlisting is not origin/group trust.
   Group policies, manual rejection, watch-only devices and a single-signing-device
   coordination mechanism still need end-to-end implementation/validation.
   Keep mnemonic-only backup and historical provenance; do not remove these as
   purported bug fixes. Existing custom mnemonic derivation is preserved for
   compatibility, but must not be described as standard BIP44 without migration.
5. **Consensus policy clarified (2026-10-10):** the owner selected equal weight
   per verified person, including transaction confirmation. The reputation
   multiplier and genesis voting bonus have been removed; expired TRST remains
   reputation, not votes. This is consensus-incompatible with the prior build:
   do not mix versions in a live network. The private Azure devnet was replaced
   as a coordinated deployment, documented below.
   Weight currently reads provisional ledger accounts, not a confirmed-only view;
   that integration, shared fork-election routing, and complete rollback remain
   release gates. Local final-vote choices and vote sequences are now committed
   and force-synced before signing. The guard survives restart and rejects later
   soft or final votes that contradict a final choice at the same account/root.
   It does not protect a key used with multiple independent databases or a
   restored old database. The existing adaptive online
   floor is unchanged and still needs reconciliation with decision 12.16.
   Four versus five governance phases, secondary-key versus pointer delegation,
   and retirement of other creator permissions remain separate open questions.
   Local validation passed signed consensus delegation (exactly two human units,
   no genesis bonus), the three-node wallet journey, hard-crash recovery, and a
   fourth fresh node's catch-up. These checks do not certify adversarial forks.
6. **Operational release:** reproducible public-network provisioning, backups/restore,
   monitoring, authenticated public-network bootstrap, upgrades, resource limits,
   adversarial traffic, and load/soak tests remain. This is a dev genesis with a
   publicly known test key, never a network for assets of value.
7. **Live genesis lifecycle:** startup still initializes genesis accrual at epoch
   zero. Pin a real launch/accrual epoch before mainnet launch. Restart now
   preserves genesis revocation, deactivation and challenge state, including
   during repair of an old zero-head account record. A new database commits the
   genesis block and account together. Missing economic history on an existing
   spending genesis account stops startup instead of resetting its BRN.

### Verification hardening (2026-10-10)

Signed verification votes now reach admission checks rather than waiting for the
target address to appear as a source block. Admission rejects invalid encodings,
unselected or duplicate voters, and incorrect BRN stakes before economic
application. Legitimate/Illegitimate require the exact configured stake; Neither
requires zero.

Selection requires the endorsement threshold and a full, distinct verifier pool.
The target, endorsers, previous-round verifiers and penalized verifiers are
excluded. Reselecting an active round cannot erase votes. A penalized final
Neither vote still reaches the tally. Versioned verification snapshots now retain
Neither history; legacy snapshots retain their stored state and cooldowns, but
cannot recover vote history the old format never saved.

The node and default drand client now pin the 30-second chained mainnet specified
in decision 19.1 and verify its BLS signatures. An intermediate change selected
Quicknet; this has been corrected without changing the approved chain.
Custom clients refuse verification without a pinned BLS verifier; a matching
SHA-256 hash alone is not accepted as proof. The previous node
client did not verify signatures, and the BLS implementation had its G1/G2
variants reversed. Regression tests include Quicknet's immutable
[round 1 beacon](https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/public/1),
changed-round rejection and malformed randomness rejection. Both approved
mainnet and Quicknet have real-beacon regression vectors. Chained proofs retain
the previous signature, and proof decoding rejects trailing or oversized data.

The node fetches each certified round from one to four configured relays in
parallel. Every response must match the pinned chain, round and BLS verifier.
The first authenticated response wins; unused requests are cancelled. Tests cover
unavailable and stalled relays, wrong rounds, forged proofs, oversized bodies,
redirect rejection and interrupted-fetch recovery. These local fixtures use
historical proofs. Production-time beacon secrecy and partition recovery remain
release prerequisites.

The default TOML setting is:

```toml
drand_relays = ["https://api.drand.sh"]
```

Add only mirrors that serve the same approved chained-mainnet endpoint. Changing
this list cannot select another chain or round. Remote URLs require HTTPS;
numeric loopback HTTP is allowed for local relays. Credentials, query strings,
fragments, duplicate URLs and redirects are rejected. An outage keeps the
certified request pending. It does not substitute local or previous randomness.

The generic verifier selector checks the provider proof and deduplicates wallet
addresses. Experimental commit-reveal rejects commitment rewrites, late commits
and duplicate reveals, and waits for every sealed commitment. A missing revealer
now stops output rather than allowing a partial result. This deliberately trades
availability for resistance to selective withholding; authenticated round
membership, deadlines and recovery remain unimplemented. It is not a production
replacement for drand.

This does not complete distributed verification. The unsafe latest-beacon /
provisional-pool selection path has now been removed. Normal endorsements remain
pending; new verification votes and challenges are rejected before locking stakes.
Legacy challenge expiry penalties are paused while certified selection is
unavailable. The explicit genesis bootstrap path remains available. The status
RPC reports the selection integration gap; no stored lifecycle data is deleted.

The approved checkpoint design uses equal-human weight from the previous agreed
roster, with 67% quorum rounded up and no offline-denominator reduction.
Certificate validation, durable signing reservations and frozen exact-round
selection requests have regression tests. Distributed checkpoint agreement,
confirmed-cut replay, certificate catch-up and committee installation also have
running-node tests behind the closed release gate. Remaining prerequisites
include production-clock boundaries, independent partitions and crash recovery.
See [certified verifier selection](CHECKPOINT_SELECTION.md) for implemented checks,
runtime restrictions and the remaining work. These changes have not been deployed
to the running apps or Azure nodes.

The checkpoint safety pass also rejects address-padding aliases and weak-key
signature forgeries. RPC submissions now return known admission rejection reasons
within a bounded wait instead of making the wallet wait for confirmation of a
rejected transaction. Missing results still report pending. Multi-node validation
passed after this fix, including unchanged stakes/frontiers after rejected votes
and challenges, hard-crash recovery and a fresh node's catch-up. Evidence is in
`/tmp/burst-validation.kgAjq1Vh`.

The drand follow-up passed the full Rust workspace suite, including 46 VRF and
70 verification tests, and `bash ./run-validation.sh`. Multi-node evidence is
retained in `/tmp/burst-validation.WkTDwb9d`. This validates the provider/helper
changes and guards against payment/restart regressions; it does not validate a
shared-checkpoint selection lifecycle, which is still absent.

Validation for the preceding verification-hardening revision passed `cargo test --workspace --locked
--offline --quiet`, `bash ./run-validation.sh`, and `node ./test-test-tools.mjs`.
One documentation example is ignored by the Rust suite. Fresh-ledger evidence
is retained locally in `/tmp/burst-validation.ehIUlLvg` and
`/tmp/burst-test-tools-2u1X4G`. The network run covers signed payments, explicit
receives, merged provenance, delegation, hard-crash recovery, a post-restart
payment and a fresh fourth node. The test-tools run covers faucet isolation,
replay, concurrent grants, and refusal to mix Live/Test/Dev ledgers.

### Private Azure validation network

Resource group: `burst-validation-20261010-rg`; region: `northeurope`.
VMs: `burst-validation-1`, `burst-validation-2`, `burst-validation-3`, each
`Standard_B2s_v2` with a 32 GB Standard SSD. Only this group belongs to this test.
SSH is source-IP restricted; RPC is loopback-only and P2P stays on the private
VNet. Three main daemons plus a fourth late-join daemon on VM 3 use separate data
directories. Ledger data is retained in `/home/burst/validation-data` and
`/home/burst/validation-latejoin-expanded` (VM 3).

The three main nodes use an enabled boot-time `burst-validation.service`, from
`deploy/burst-validation.service`. Nodes 2 and 3 also use
`deploy/validation-peer.env` installed as `/home/burst/validation-node.env`.
The extra `burst-validation-latejoin` remains a transient test unit; its data is
retained but the extra process will not start automatically after a VM reboot.

On 2026-10-10 the owner authorized breaking the unused test deployment. The old
ledgers (11 accounts / 59 blocks on each process) were initially archived, then
permanently deleted at the owner's request, along with obsolete late-join test
ledgers, rollback binaries, and staged deployment copies on all three VMs.
Those remote backups are no longer available for restore. The active ledgers,
current daemon, installed services, and current configuration were preserved.

The new devnet uses equal verified-person weight with no genesis or reputation
bonus. A subsequent restart fix initializes quorum using the existing adaptive
online-weight floor; it does not invent an online sample from all eligible,
including offline, wallets. The intermediate binary was removed during the
owner-authorized cleanup.

Current portable daemon SHA-256:
`ae0c37fac5eb09fde0f5d0e01a514d75abc0a79cf666bbd57c00d95969032c1f`.
`deploy/validation-node-{1,2,3}.toml` are installed as
`/home/burst/validation-node.toml` on their respective VMs. They advertise only
private VNet listening endpoints, enabling direct peer discovery rather than a
seed-only star. The fourth process uses `deploy/validation-latejoin.toml`.
RPC stays loopback-only. The three-VM signed wallet journey, fourth-node catch-up,
and an additional signed payment immediately after restarting all four processes
passed, including after SIGKILL rather than graceful shutdown. All four processes
held three connected peers and the same 17-block ledger across twelve samples.
The final binary also passed fresh fourth-node catch-up; that fourth node's
superseded test ledger was removed during cleanup. After cleanup all four live
processes still reported 17 blocks, three verified accounts, and three peers;
the deployed daemon checksum was unchanged on all three VMs.
`wallet_core/examples/validate_restart_payment.rs` exercises new payment
confirmation after restart, not merely readability of previously confirmed data.
The same portable daemon passed the complete isolated NAT/firewall suite with
zero failures, including the populated-ledger restart regression, double NAT,
manual forwarding, and UPnP renewal, cleanup, and automatic peer discovery.
Local evidence is retained in `/tmp/burst-human-azure.RALyFYAM` and
`/tmp/burst-nat.R1HunO1j`; the workspace result is 1,189 passed / 0 failed /
1 ignored doctest. These benign-network checks do not close the adversarial
fork/finality and confirmed-only weight-view release gates above.

An earlier Azure VM 2 reboot test confirmed its main node started automatically and the
checked economic state still agreed with the other three processes.
Daily VM auto-shutdown is enabled at **02:00 UTC**. Allocated VMs incur compute charges; disks and public IPs
can continue to incur charges while deallocated. Do not delete the group to stop
compute charges if its test evidence is still needed.

Resolve the current address (do not assume an old public IP is still assigned):

```bash
az vm list-ip-addresses -g burst-validation-20261010-rg -o table
# Replace CURRENT_VM1_PUBLIC_IP with the returned address; run in a terminal:
ssh -N -L 29077:127.0.0.1:7077 burst@CURRENT_VM1_PUBLIC_IP
```

Set either desktop app's RPC endpoint to `http://127.0.0.1:29077`. The tunnel must
remain open. The permitted SSH source IP may need updating when the operator's
network changes. For local work without Azure, use `./dev-user.sh` and :7077.

### NAT / firewall validation

Use a dedicated Linux Docker validation host with at least 6 GiB RAM. Set
`BURST_NAT_VALIDATION_HOST` to that host's name and `BURST_NAT_ARTIFACTS` to a
directory containing existing executable `burst-daemon`, `validate_transport`
and `upnp_probe` binaries. Set `BURST_NAT_IMAGE` to an existing compatible lab
image (default `burst-nat-lab:local`), then run `bash ./run-nat-validation.sh`.
The runner never builds binaries or images. Prepare missing artifacts separately
under the validation resource limits and record their source/toolchain identity.
Do not run optimized builds on the desktop. Reconcile prior evidence using
`spec/VALIDATION_WORKFLOW.md` before scheduling another run.
Check Docker access as the service user before launching the suite. On the
dedicated validation host, a transient unit can use `SupplementaryGroups=docker`;
do not make the Docker socket world-writable or change the user's permanent
groups just to run this check.
The runner rejects missing/mismatched host selection and overlapping NAT jobs,
records binary hashes, and pins the container to the inspected image ID.
The runtime container has a one-CPU, 3 GiB, zero-swap, 256-process limit and a
20-minute lifetime. It creates an isolated router and clients
inside a **privileged container with no external network**. It does not modify
host firewall rules, Azure NSGs, or a physical router. RPC exposure in the lab
configuration is controller-only inside that isolated network; do not reuse its
`rpc_bind = "0.0.0.0"` setting for an Internet-facing deployment. Containers are
removed on exit; logs and test ledgers are retained in the printed temporary
evidence directory. A failed scenario makes the suite exit nonzero.

Observed on 2026-10-10:

- A single node behind source NAT passed real signed burn/receive/transfer,
  merge and delegation journeys with unsolicited inbound P2P blocked.
- After an active firewall interruption, it detected disconnection, reconnected
  without restart, and confirmed another complete signed wallet journey.
- Manual forwarding from the router's WAN port 17076 to a private node's 7076
  passed inbound handshaking and the full signed wallet journey. That private
  node started without bootstrap peers or reachout enabled, so the initial
  connection really came through the forwarded port.
- **Two default-port nodes behind one public IP now pass.** Live channels,
  broadcasts, bootstrap replies and peer metadata use handshake node identity,
  not public-IP plus local listening port. Both clients remain connected and
  complete the signed wallet journey. Inbound ephemeral ports are not gossiped,
  cached or redialed as listening endpoints. Per-IP handshake rate limits remain.
- A contrast test with different listening ports (7076/7086) retained both
  same-NAT clients and passed the signed wallet journey.
- Two same-port clients also passed through **two independent NAT layers**:
  a home router and an ISP-style `100.64.0.0/10` network. Unsolicited inbound
  traffic stayed blocked; signed transactions and ledger convergence worked
  through their outbound connections to a reachable node.
- **UPnP uses the routed LAN address**, not `0.0.0.0`. The IGD emulator validates
  SOAP and installs real Linux DNAT/forwarding rules. Tests cover discovery,
  delayed gateway availability, mapping/cleanup, conflicting external ports,
  two simultaneous clients, lease renewal, failed-renewal withdrawal and recovery,
  WAN changes to CGNAT, crash expiry, and preserving another client's reassigned
  mapping. Lifecycle tests accelerate a finite lease to four seconds; the normal
  lease is one hour, renewed every thirty minutes.
- A real daemon (isolated Test network) automatically mapped an alternate WAN
  port, advertised it through keepalives, and accepted a connection from a third
  node that knew only the bootstrap seed. Daemon shutdown removed the mapping.
  A separate signed wallet journey passed through an automatically mapped port
  while its short lease was repeatedly renewed.
- Keepalive slot zero is consistently the sender's address (or `0.0.0.0:0` when
  unavailable), never a random peer. An expired UPnP address is withdrawn.
  Explicit `advertise_address` configuration takes priority over UPnP.
- Simultaneous-connect dedup retains a successfully dialed endpoint even when
  the inbound socket wins, preventing bootstrap redial loops. Connection
  replacement and disconnect bookkeeping use the same lock order and generation
  check, so an old socket cannot mark its replacement offline.

The tests are in `network/tests/nat/`, with the real mapping probe in
`network/examples/upnp_probe.rs`. The earlier networking build had **1,181 passing
tests**, with one ignored doctest. Its crash/restart and fresh-node bootstrap checks
also passed. That binary was deployed to the three Azure validation VMs
(four node processes); saved state survived, a fresh signed wallet journey
passed on all four, and each maintained three peers across twelve samples.
Pre-upgrade binaries were subsequently deleted at the owner's request; RPC
remains loopback-only.

**Boundaries:** this is Linux router emulation, not certification against physical
routers or an ISP. CGNAT participation requires an outbound TCP path to a reachable
peer; UPnP does not create an inbound mapping through the ISP's additional NAT.
There is no STUN/TURN, hole punching or relay service, and a firewall that blocks
all usable outbound paths cannot be bypassed. Permanent-only UPnP leases are not
requested: finite leases limit stale mappings after crashes. For manual port
forwarding, configure `advertise_address = "PUBLIC_IP:EXTERNAL_PORT"` and forward
that TCP port to the node's actual listening port. Never forward privileged RPC.
Dev mode deliberately disables automatic UPnP on real routers.

Public distribution is blocked pending [pilot acceptance](spec/TESTNET_PILOT.md).
The commands below describe the deployment scripts; they are not a qualified
release or the local restart path. Do not distribute the validation network's
published-key fixture. Current Linux desktop candidate packages require x86-64,
glibc 2.39 or later, GTK 3 and WebKitGTK 4.1. Installation evidence currently
covers Ubuntu 24.04; it does not establish support for every Linux distribution.

### Linux

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh
```

### macOS

```bash
curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sh
```

### Windows (PowerShell as Administrator)

```powershell
irm https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.ps1 | iex
```

Each script verifies the downloaded binary against the release checksum manifest,
writes a config if none exists, registers a background service, and enables
auto-updates. RPC and WebSocket access default to localhost. No bootstrap peer
is assumed and installation does not enable test funding. Existing config files
keep their data-directory setting.

### What it sets up

| | Linux | macOS | Windows |
|---|---|---|---|
| Binary | `/usr/local/bin/burst-daemon` | `~/Library/Application Support/BURST/bin/burst-daemon` | `C:\ProgramData\BURST\burst-daemon.exe` |
| Config | `/etc/burst/config.toml` | `~/Library/Application Support/BURST/config.toml` | `C:\ProgramData\BURST\config.toml` |
| Data | `/var/lib/burst` | `~/Library/Application Support/BURST/data` | `C:\ProgramData\BURST\data` |
| Service | systemd | launchd | Windows Service |
| Auto-update | systemd timer (5 min) | launchd (5 min) | Scheduled Task (30 min) |

Updates use verified temporary downloads and retain the previous executable as
`burst-daemon.previous-*` (Windows uses `burst-daemon.exe.previous-*`). Failures
are reported by the service manager. An update does not start a stopped service.
Do not restore an older executable without checking ledger-format compatibility;
the updater does not roll back ledger data. A killed Unix updater can leave a
`burst-daemon.update-lock` directory. Remove that lock only after confirming no
updater is running.

### Common commands

**Linux:**
```bash
journalctl -u burst-node -f         # view logs
systemctl status burst-node          # check status
sudo systemctl restart burst-node    # restart
```

**macOS:**
```bash
tail -f ~/Library/Application\ Support/BURST/burst-node.log   # view logs
launchctl kickstart -k gui/$(id -u)/com.burst.node             # restart
```

**Windows (PowerShell):**
```powershell
Get-Service BurstNode                # check status
Restart-Service BurstNode            # restart
```

### Uninstall

**Linux:** `curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh -s -- --uninstall`

**macOS:** `curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sh -s -- --uninstall`

**Windows:** Run `install.ps1 --uninstall` in an elevated PowerShell.

## Architecture

The live node authenticates immutable block contents in bounded CPU workers.
It rechecks current rules and dependencies before economic application, then
commits ledger and economic changes through one LMDB writer. Candidates borrow
each economic engine until its first mutable access; unchanged merger indexes are shared.
This is not yet account-parallel economic execution; wallet-portfolio copying
and whole-portfolio persistence remain scaling limits.

Representative authority comes from verified people, not token balances. The
gated checkpoint path derives identity, governance and authority transitions
from certified history. Those transitions remain unavailable in ordinary
builds until the recorded release prerequisites pass. Group trust stays in the
wallet and does not grant protocol voting power.

Rust workspace organized into modular crates:

| Layer | Crates | Purpose |
|-------|--------|---------|
| Foundation | `types`, `crypto`, `utils` | Fundamental types, Ed25519/Blake2b crypto, shared utilities |
| Core Protocol | `brn`, `trst`, `transactions` | BRN computation, TRST lifecycle + merger graph, all transaction types |
| Storage | `store`, `store_lmdb`, `ledger` | Abstract storage traits, LMDB backend, DAG block-lattice |
| Verification | `verification`, `vrf`, `consensus`, `work` | Humanity verification, VRF, double-spend resolution, anti-spam PoW |
| Governance | `governance`, `consti` | 4-phase democratic governance, on-chain constitution |
| Networking | `messages`, `protocol`, `network` | Message types, wire protocol, P2P networking + NAT traversal |
| Application | `node`, `daemon`, `rpc`, `websocket` | Node orchestration, CLI entry point, JSON-RPC, WebSocket |
| Client | `wallet_core`, `groups` | Wallet library, group trust layer |
| Testing | `nullables` | Nullable infrastructure for deterministic testing |

## Building from Source

```bash
cargo build --release -p burst-daemon
```

Cross-compile for Linux arm64:
```bash
cross build --release -p burst-daemon --target aarch64-unknown-linux-musl
```

## Running tests

Run one exact test while changing a path, then use the broader groups at a
milestone. The runner checks that the name exists before executing it:

```bash
bash tools/test-node.sh focused checkpoint_sync::input::tests::slow_polls_and_busy_inbox_both_make_progress
```

Large network fixtures run one at a time by default to fit a 1024-file process
limit. Run fast tests separately so workers waiting for a network fixture do
not hold up unrelated tests:

```bash
bash tools/test-node.sh fast
bash tools/test-node.sh slow
bash tools/test-node.sh network
```

For a full sweep, run all three groups in order:

```bash
bash tools/test-node.sh all
```

On Linux, the runner pins Cargo and all test children to at most two allowed
CPUs and lowers scheduling priority by 15. It defaults to one build job and one
network fixture. `BURST_TEST_CPUS` changes the CPU budget. On other systems,
worker limits and low priority still apply, but there is no hard affinity cap.
`CARGO_BUILD_JOBS` and `TOKIO_WORKER_THREADS` can override the worker defaults.

The slow group contains the expensive combined challenge/expiry replay test.
The fast group excludes both slow and network filters; `all` stops on the first
failing group. `BURST_TEST_NETWORK_CONCURRENCY=2` enables two network fixtures
and raises the file limit to 4096. The concurrency setting accepts only `1` or
`2`. It does not relax assertions or timeouts. All runner modes use `--locked`.

CI distributes network tests across four independent runners. Set
`BURST_TEST_NETWORK_SHARDS=4` and `BURST_TEST_NETWORK_SHARD=0` (through `3`)
with `network` mode to reproduce one shard. Sorted test names are assigned once;
each selected test runs by exact name, serially. Native desktop and public-network
tests marked ignored still need their documented environments and explicit runs.
The publishing workflow requires the same-revision CI workflow and the separate
conformance release gate. Passing ordinary CI does not replace those device and
release reviews.

The workspace and all CI/release builds pin `nightly-2026-10-11` and use the
committed dependency lockfile. The executable smoke check is
`node tools/daemon-smoke.mjs target/debug/burst-daemon` on Unix. Windows uses
`tools/windows-service-smoke.ps1` in an elevated session on a disposable machine;
it refuses to replace an existing `BurstNode` service.
Iteration 148 records a passing native Windows Server 2022 run: eight daemon
tests, service start/RPC/stop/restart, invalid-config refusal, and installer
contracts under Windows PowerShell 5.1 and PowerShell 7.6.6. See
[the validation record](REFERENCE_IMPLEMENTATION_REVIEW.md#iteration-148-windows-validation-inputs-and-installer-strings).
Windows GUI bundles and signed distribution remain separate release checks.

Append `--list` to inspect a group's tests without executing them. Focused mode
also accepts it after the exact test name. Listing may still compile changed
code. Direct `cargo test` commands bypass the runner's resource limits.
Test builds optimize
signature arithmetic and Blake2 while retaining protocol debug assertions.
Recent results and their
coverage limits are in [the implementation review](REFERENCE_IMPLEMENTATION_REVIEW.md).

## Running a Dev Node

```bash
cargo run --bin burst-daemon -- node run
```

With a config file:
```bash
cargo run --bin burst-daemon -- --config testnet.toml node run
```

## Supported Platforms

| Platform | Architecture | Build |
|----------|-------------|-------|
| Linux | x86_64 | Static (musl) |
| Linux | arm64 (RPi4) | Static (musl) |
| macOS | Apple Silicon (M1/M2/M3/M4) | Native |
| macOS | Intel | Native |
| Windows | x86_64 | Native (MSVC) |

All platforms use rustls for TLS (no OpenSSL dependency).

## Testnet

The previous Azure deployment has been retired. Old seed IPs in deployment and
installer files have not been revalidated; they are not a current availability
claim. Use the local development setup above until a replacement testnet is
deployed and its seed configuration is updated.

## License

MIT License. Copyright 2025 Nitesh Gautam.

See [the whitepaper](BURST_WHITEPAPER.md) for the full design.
