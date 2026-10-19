# Whitepaper conformance

Current execution plan: [COMPLETION_PLAN.md](COMPLETION_PLAN.md).
Current capability status: [COMPLETION_LEDGER.md](COMPLETION_LEDGER.md).
These replace the historical iteration queue, not the indexed source requirements.

BURST is not complete against its whitepaper. This directory records the target
and the evidence still needed. It does not redefine intended functionality as a
bug or treat disabled functionality as implemented.

## Scope and authority

`coverage.json` indexes all 97 level-one through level-three whitepaper headings
and all 291 numbered implementation-decision sections in the current working
tree. These 388 entries include narrative and grouped requirements; they are not
388 independent features. Both source files were read in full for this pass.
Other documentation, the website, deck, UI and every implementation path have
not received a complete new audit in this pass.

Source hashes pin the exact text reviewed. `head_sha256` records the Git HEAD
version as a comparison point because both documents already had local changes.
The initial inventory pass did not edit either source document. Iteration 161
amends selection recovery with the owner's authorization; see
`DEADLINE_RECOVERY.md`. The owner delegated interpretation
of the six recorded conflicts on 2026-10-10. `DECISION_RESOLUTIONS.md` records
those choices and their reasons separately from implementation sign-off.

Run from `protocol/`:

```sh
node --test tools/check-spec-coverage.test.mjs
node tools/check-spec-coverage.mjs
node tools/check-spec-coverage.mjs --list-open
node tools/check-spec-coverage.mjs --release
```

The normal check verifies inventory consistency and reports open entries. CI
runs that check. `--release` also fails while entries lack sign-off. The publishing
workflow requires that check before creating or updating its public release.
Platform builds can still produce CI artifacts while publication is blocked.
It checks recorded evidence, not the truth of a claim or the result of a fresh
test run. Reviewers must reproduce the commands before signing off. A passing
inventory check does not establish protocol safety or whitepaper parity.

`unverified` means not reviewed to sign-off, not necessarily missing.
`partial` means a known implementation or validation gap remains.
`decision-conflict` needs an authoritative resolution.
`verified` requires scoped evidence and a passing reproduction command.
`excluded` requires recorded scope authorization. Future work and difficult
requirements must not be silently excluded.

## Resolved source conflicts

These choices follow the owner's delegation. See `DECISION_RESOLUTIONS.md` for
the reasoning and remaining verification requirements.

| ID | Sources | Resolution |
| --- | --- | --- |
| C01 | Decisions 6.12, 9.5, 37.1 | Preserve 256 inline merge inputs. Base encoded limits are 1024 bytes for ordinary blocks and 9216 for merges; C07 adds bounded stake-release evidence within the existing 64 KiB message limit. |
| C02 | Decisions 4.9, 14.1, 14.4, 31.9 | Predecessor-root work, account-bound opening work and nonce-first Blake2b-64, introduced in block version 2 and retained in version 6. Updated peers and a fresh development ledger are required. |
| C03 | Decisions 12.1, 12.9, 12.16 and checkpoint clarification | Voting authority comes from agreed confirmed humans and delegation, never tokens or provisional local attendance. The gated payment handoff is integrated; adversarial validation remains. |
| C04 | Decisions 15.13, 33.10 | Keep the public endorsement graph. Do not promise endorser anonymity. |
| C05 | Whitepaper verification rewards, decision 33.8 | No protocol endorser reward. Verifier compensation and challenge rewards remain in scope. |
| C06 | Decisions 8.5, 8.6, 12.5, 37.1 | No memos and no artificial five-minute cementation delay. Quorum, causal confirmation and durable commit remain required. |
| C08 | Decisions 7.4, 17.4, 17.7 | Certified appeals review the latest judgment, correct its reward origins and do not pay duplicate fraud bounties. Release remains gated. |
| C09 | Decisions 15.3, 20.2 | Rolling Neither history, initially 100 assignments and a strict 50% threshold. Both values are governable. |
| C10 | Decisions 15.5, 20.1, 20.2, 20.11 | Govern the verifier balance minimum and emergency timing/thresholds. Enforce emergency participation separately from approval; restore the selected 100 BRN proposal-endorsement default. |

C07 records the signed stake-release encoding used to preserve decisions 5.5,
5.8 and 5.9. It is an implementation choice, not another source contradiction.
Block version 4 introduced complete vote sets. Wallets batch evidence when
needed. Version 5 also signs the installed payment epoch; version 6 retains
both and adds bounded scoped-key operations.
Ordinary builds retain the genesis context. The gated runtime installs certified
epochs, permanently closes the old context and preserves its final promises.
Agreed deadline releases have live recovery evidence but remain gated pending
the remaining adversarial and lifecycle checks.

## Known implementation gaps

An unsigned section is not automatically missing code. A working component is
not automatically a usable feature. The current code boundaries are below;
the numbered notes retain the detailed evidence and limitations.

| Work | Current code boundary | Completion evidence |
| --- | --- | --- |
| Certified verification and governance | Running-node integration exists; ordinary builds keep source gates closed. | Production-clock and beacon recovery, independent partitions/crashes, remaining lifecycle interactions, native app flows. |
| Fork recovery | Payments and verifier opt-in/out support bounded replacement. Disjoint TRST entries and independent merges sharing an origin survive; identity and governance branches remain rejected. | Dependency-closed replacement for remaining block kinds, overlapping holding/revision updates and independent-signer partitions, without erasing final promises or cemented history. |
| History growth and sync | Gated snapshot bootstrap, live installation/replacement, peer history proofs and compact successor recovery have running-node, restart and process-crash evidence. | Remaining adversarial schedules and lifecycle combinations, storage-fault/power-loss validation, bounded inventory compaction, and pruning that preserves restored or re-expired provenance. See `SNAPSHOT_ARCHIVE.md`. |
| Numeric rules | All 41 stored parameters have governance mappings. Verifier balance and emergency rules now use those values. | Remaining activation interactions and constant classification are tracked in `NUMERIC_POLICY_AUDIT.md`. |
| Transport and concurrency | Typed Protobuf peer messages are implemented. The running node authenticates blocks in bounded CPU workers before serialized economic application. | Finish live transport recovery checks, measure economic preparation and state-copy costs, and complete account-scoped processing under decision 25.7 with cross-account atomicity preserved. A single LMDB writer is not itself a gap. |
| Wallet and groups | Native flows and a functional WASM client exist. Bounded, live group transitivity is opt-in and connected to wallet settings and payment evaluation. | Live-ledger browser journeys, remaining native flows, packaged-platform graph checks and weighted policy. See `../wallet_core/BROWSER.md` and `../groups/README.md`. |
| Operations and product | Deploy tooling and apps exist. | Performance targets, devices, NAT/firewall cases, upgrade/recovery procedures and usable error/notification paths. |
| Research claims | The whitepaper identifies simulation and adversarial analysis as unfinished. | Assumptions, simulations, incentive/security analysis and bounded-community observations. Unit tests do not establish economic equilibrium or unique humanity. |

Later randomness phases, privacy research and key recovery retain their stated
future scope. They do not replace finishing the phase-one drand path. No source
requirement is removed by this grouping.

Concurrency assessment: rsnano's processing workers validate batches against
read transactions, then insert through an LMDB write transaction. BURST now
offloads format, content-hash, work and signature checks into bounded workers.
Admission rechecks current rules and state before economic application. This
does not claim concurrent economic execution. Each economic engine stays
borrowed until its first mutation. Ordinary TRST transfers do not copy BRN;
stake acknowledgements do not copy TRST. All candidates use the same validation
path. Merger indexes use copy-on-write storage, and unchanged graph snapshots
are not serialized again. Graph bytes and rollback semantics are preserved.
Wallet portfolios now use copy-on-write storage too. Their first mutation can
still copy the outer wallet map, and changed holdings still serialize a whole
wallet snapshot. BRN persistence writes changed records, but preparation still
copies and compares BRN state. The local preparation benchmarks are not
throughput results. Account-scoped mutable preparation remains open.

Iteration 159 adds startup checks for corrupt or missing TRST snapshots. New
genesis creation persists the graph and holdings atomically. Existing incomplete
development stores are refused, not silently initialized with empty economic
state. Keep the failed store for diagnosis and recover from a known-good backup
or re-sync into a separate data directory. Structural snapshot checks do not
authenticate peer-provided state and do not supply fast sync or pruning.

Iteration 160 adds a separate quorum-authenticated economic snapshot format.
It retains replay continuation state, verifies human-weighted signatures before
decoding the state body, and checks imported authority transitions against the
previous certified electorate. Independent replays produce the same bytes.
Snapshot-tail execution still needs complete signed ancestry for dependency
checks. Iteration 161 adds a bounded authenticated archive and the read-only
`snapshot verify` command. Iteration 163 adds on-demand attestation and export
commands, a durable signer journal and an executable operator journey. Peer
archive staging is opt-in. Iteration 164 adds peer availability queries with an
independently configured minimum height; offers gain no authority until their
archive headers authenticate. See `SNAPSHOT_ARCHIVE.md`. Atomic snapshot
installation and permission to prune that ancestry remain unfinished. Ordinary
source gates remain closed.

These are identified gaps, not a claim that all unverified entries have been
audited. The entry inventory prevents the unreviewed remainder from disappearing.

1. Normal humanity verification and challenges remain disabled in ordinary builds.
   The gated runtime now has running-node evidence for certified normal selection,
   signed votes, rejected challenges and upheld fraud settlement. The installer
   commits replay-derived economic state and the next human electorate atomically.
   Tests include old-epoch rejection, new-epoch payments, restart and fresh-node
   catch-up. Roster-only updates also have live evidence, including dilution of
   genesis authority before bootstrap exit. See iterations 78 and 79 in
   `../REFERENCE_IMPLEMENTATION_REVIEW.md` for test scope and logs.

   Normal committees now have an agreed activation boundary in the gated runtime.
   It binds the certified selection and exact beacon proof to a shared voting
   deadline, which survives restart and fresh-node catch-up. Normal deadline
   completion has focused replay/settlement and four-node live evidence, including
   returned-stake spending, restart and fresh-observer recovery.
   Challenge activation/completion now has node integration and live deadline
   evidence for activation before and after an early vote, including refund
   spending, restart and fresh-node catch-up. Fraud re-verification also has
   live restoration and recovery evidence in iteration 88. Iteration 124 adds
   coincident challenge settlement and mixed pending expiry for fraud and
   inactivity, including late-receive rejection and recovery. Iterations 129–130
   add a connected certified fraud appeal, reward-origin corrections, wallet
   review/signing, all-node restart and fresh-observer recovery. Native desktop
   appeals, adversarial handoffs and
   unbiased recovery after missed beacon deadlines remain open.
   Iteration 136 adds a connected restoration, failed repeat appeal and later
   reversal to fraud, including sponsor stake accounting, reward-origin history,
   revocation of new payments, four-node restart and fresh-node catch-up.
   The connected inactivity appeal also passes restored accrual without fraud
   rewards or revocation, followed by restart and fresh-observer recovery.
   An honest 2+2 partition at the fraud and appeal deadlines now passes unchanged
   identity, held stakes and unchanged payment epoch until quorum recovers,
   followed by settlement, restart and fresh-observer recovery. It does not
   establish Byzantine safety or independent production-clock behavior.
   An incomplete challenge does not revoke the target or unlock its stakes.
   A collected upheld verdict needs a certified settlement boundary. Local clock
   ticks cannot substitute for either boundary. Controlled-clock fixtures and
   historical drand proofs do not establish production timing or beacon secrecy.
   Keep `verification/src/lib.rs`'s release gate closed. The remaining prerequisites
   are tracked in `../CHECKPOINT_SELECTION.md`.
   The upheld-challenge journey also verifies claiming the challenger and
   correct-verifier rewards, spending returned stakes, and recovering those
   effects. Completed panels no longer leave an unsigned activation blocking
   their next replay-derived transition. See iterations 86 and 87 in the review.
2. Payments use the installed human electorate. Gated certified roster updates
   preserve old-context promises and reject cuts that erase cemented state.
   Independent-signer partition and crash/handoff liveness remain unverified.
   Live fork replacement supports payments, merges and verifier opt-in/out.
   Three-way snapshot restoration preserves unrelated wallets and merger graph
   entries, including independent consumers sharing a burn or merge origin.
   Changes to the same holding, merge or revision still fail closed. Identity/governance forks and
   unprovable orphaned fork payload cleanup remain. Finalized-position cleanup and a signed two-node merge
   replacement are tested; they do not establish Byzantine partition safety.
3. Certified governance now has independent-human and native-wallet evidence
   for parameter and constitutional proposals, direct and scoped-key voting,
   activation, withdrawal, restart and fresh-node catch-up. Iterations 119–123
   record those journeys. All 41 stored numeric parameters have governance
   mappings, including the rolling Neither threshold and window added in
   iteration 131. Partitioned ballots and revocations, remaining concurrent
   activation cases, hard-coded numeric rules and production-scale replay still
   need evidence. Full constitutional content and scoped-key release gates
   remain closed. A shared-development-key test does not establish
   independent-voter governance safety.
4. Fresh-node catch-up now includes an eight-hour-old certified prefix in the
   six-node settlement fixture. Certificate validation and semantic replay grant
   exact-hash age exceptions; arbitrary peer history cannot bypass admission's
   age window. The gated installer reconstructs state from authenticated history;
   it does not trust a peer's economic snapshot. General weeks-old recovery,
   history beyond the last certified cut and pruning are not complete.
   Re-expiry and re-verification must preserve provenance.
   The partial confirmed-cut audit now recomputes balances and provenance
   from signed blocks, without stored economic afterimages. Its historical
   timestamp policy now also authenticates bounded checkpoint history before
   network admission. Live economic validation and future-time bounds remain.
   It also reconstructs creator bootstrap verification, pending normal
   endorsements and verifier opt-in/out. Overlapping normal endorsement and
   creator admission of one target are rejected as ambiguous. Supported creator
   admissions now replay across early bootstrap epochs.
   Initial certified normal panels after bootstrap exit now reconstruct
   verification, stake settlement and rewards from their ancestor certificate,
   beacon and signed votes. Complete certified revotes, rejected challenges and
   certified upheld outcomes also replay. Supported governance and pending-expiry
   cuts now replay and recover through live certified journeys; the current
   interaction gaps are tracked in the review's "Next correctness work" section.
   Unsupported histories still fail closed. This does not migrate an old ledger or
   authorize unsupported lifecycle effects.
   TRST snapshots persist origin ownership, including pending and merged origins,
   and tests exercise revocation across restart. Ordinary snapshots use version 3;
   version 4 adds outcome-specific invalidation reasons when present. Iteration
   127 records component evidence; iterations 129–130 add the connected
   certified fraud-appeal journey. Older
   portfolio-only snapshots require explicit replay. No automatic migration or
   deletion is provided; use a disposable data directory for current tests.
5. Wire generation 30 uses typed Protobuf envelopes, as selected in decision
   23.3. Signed blocks retain their existing encoding. Independent schema checks
   and bounded coverage-guided decoder fuzzing have passing evidence in iteration
   136. Block application still has one economic writer despite decision 25.7's
   selected per-account parallelism.
6. Wallet-core now queries live groups, inspects original creators behind merged
   and returned payments, and distinguishes actual outages from negative or
   malformed responses. Its bounded directory reader and explicit protocol
   fallback have component tests. Consumer group setup, explicit directory
   selection and automatic acceptance now pass browser and real-node native
   tests in iteration 57. Iteration 137 adds opt-in, bounded group-to-group trust
   with live endorsement queries and receiver-pinned endpoints. Weighted custom
   rules and directory publication/discovery remain incomplete. The WASM client now compiles and runs
   cryptography, signed RPC submission and group requests in Chromium and a Web
   Worker. Its RPC contract fixture is not a live-ledger journey or a standalone
   browser wallet. Wallet and node UI completion cannot be inferred from that test.
7. Burst-operated threshold randomness is unimplemented. Commit/reveal lacks
   complete authenticated membership, agreed deadlines and recovery. These are
   later phases, not evidence that phase-one selection is integrated.
8. Economic and Sybil-resistance claims need adversarial analysis and trials.
   Performance targets, broad NAT/router coverage, device testing and release
   operations need their own evidence. Unit tests cannot prove these claims.

## Review map

Every decision chapter has an entry point below. No row is a sign-off. The
machine-readable section records remain the source of review status.

| Decision chapter | Implementation entry points | Evidence still needed for chapter sign-off |
| --- | --- | --- |
| 1. MVP | `node`, `wallet_core`, wallet and node GUI sibling repositories | Every included lifecycle works through the running apps. |
| 2. Performance | `node`, crate `benches/` directories | Reproducible latency, throughput, memory and disk tests on target hardware. |
| 3. Licensing/legal | `LICENSE`, dependency manifests | Repository/dependency licensing review; do not infer legal classification from code. |
| 4. Cryptography | `crypto`, `ledger/src/state_block.rs` | Key/address/signature vectors, keystore tests and version-2 work integration. |
| 5. BRN | `brn`, `node/src/ledger_bridge.rs` | Rate history, staking, overflow and restart behavior through signed blocks. |
| 6. TRST | `trst`, `node/src/expiry_commit.rs` | Merge/split rounding, pending expiry, changed expiry and un-revocation. |
| 7. Revocation | `trst`, `verification`, `node` | Certified challenge to proportional revocation and notifications, with crashes. |
| 8. Transactions | `transactions`, `node/src/ledger_updater.rs` | Every send/accept/reject path and the no-memo decision. |
| 9. Block format | `ledger`, `node/src/block_processor.rs` | Size policy, extensibility, version transitions and encoding vectors. |
| 10. DAG | `ledger`, `node/src/fork_resolution.rs` | All block-kind forks, pruning dependencies, epoch upgrades. |
| 11. Snapshots | `node/src/confirmed_history.rs`, `store_lmdb` | Certified export/import, availability, incremental replay and catch-up. |
| 12. Consensus | `consensus`, `node/src/confirmation_processor.rs` | Confirmed-only weights, ordinary quorum policy, independent signers and partitions. |
| 13. Representatives | `consensus`, account store | Opt-in/default/delegation/offline behavior and durable changes. |
| 14. PoW | `work`, RPC and block admission | Automatic wallet precomputation, supported upgrade paths, priority and anti-spam tests. |
| 15. Verification | `verification`, `node/src/endorsement_commit.rs` | Certified selection and all timeout/revote/penalty outcomes with public endorsements. |
| 16. Endorsements | `verification`, `node/src/endorsement_commit.rs` | Confirmed ordering, conflicts, duplicates, expiry and no endorser reward. |
| 17. Challenges | `verification`, `node` | Admission through settlement, concurrent cases, genesis challenges and rewards. |
| 18. Reputation | `verification/src/voting.rs`, account store | Persisted history and its specified effect on eligibility. |
| 19. Randomness | `vrf`, `verification/src/selection_request.rs` | Running exact-round committed selection, outage recovery and manipulation tests. |
| 20. Governance | `governance`, `node/src/governance_commit.rs` | Confirmed tallying, conflicting proposals, activation, scoped delegation. |
| 21. Constitution | `consti`, governance transport | Full amendment content, limits, Unicode, versioning and durable adoption. |
| 22. Group trust | `groups`, `wallet_core/src/trust_policy.rs` | Live queries, caching, unavailable-group behavior and wallet policy integration. |
| 23. Networking | `network`, `protocol`, node transport | Protobuf, peer limits/bans, encryption decision, NAT cases and rate limits. |
| 24. Storage | `store`, `store_lmdb` | Index consistency, bounded growth, corruption and transaction failure tests. |
| 25. Operations | `node`, `daemon`, `rpc` | Selected modes, safe shutdown, authentication and per-account concurrency. |
| 26. Recovery | `node`, `store_lmdb` | Corruption recovery, old-history sync, missing dependencies and double receives. |
| 27. Launch | `node/src/genesis_state.rs`, `types/src/params.rs` | Network isolation, bootstrap exit, authority removal and parameter parity. |
| 28. RPC | `rpc` | Per-method auth, limits, pagination, batches and error contract. |
| 29. WebSocket | `websocket` and app clients | Filtering, reconnection, event loss and committed-state notifications. |
| 30. Explorer | node GUI and RPC | Account/block/provenance traversal, history availability and empty states. |
| 31. Wallet | `wallet_core` and wallet app | Backup/restore, multi-device conflicts, offline work and receive policy. |
| 32. Display | wallet and node GUI | Exact amounts, addresses/QR/URI, accessibility and device-level review. |
| 33. Economics/privacy | `brn`, `trst`, `verification`, wallet | Pending ownership, reward conflicts, public graph claims and adversarial trials. |
| 34. Build/test/deploy | CI, `daemon`, deploy scripts, wallet WASM client | Supported targets, migrations, fuzz/benchmark runs, upgrade coordination. |
| 35. Catastrophes | upgrade and governance paths | Specified recovery/migration behavior; no claim of quantum or majority-Sybil safety. |
| 36. Open design | BRN, verification, interoperability scope | Revoked balance and re-verification behavior; record deferred scope explicitly. |
| 37. Defaults | `types/src/params.rs`, runtime config and app defaults | Field-by-field match and documented test overrides under the recorded resolutions. |

## Completion rule

Resolve contradictory choices before implementing their dependent behavior.
For each remaining section, record the actual path, missing behavior and a
reproduction command. Tests must include errors and interrupted execution where
state is at risk. Component coverage cannot substitute for a running network or
wallet flow. Re-run the relevant checks after changes; keep failed attempts and
limitations in the implementation log. Do not change the target merely to make
the conformance check green.
