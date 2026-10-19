# CI repair, 2026-10-19

Baseline: protocol a565af3, CI run 39311525690; the preceding code run was
39310102263. Wallet 0054a6d and dashboard a07323d have no Actions workflows.
Passing CI is regression evidence, not whitepaper sign-off or release approval.

## Change impact and validation plan

| Changed input | Requirement/check | Existing evidence | Next check and reason |
| --- | --- | --- | --- |
| Offline delegated-action representation and accessors | Native and WASM compilation, unchanged signed actions | Native workspace tests pass; WASM and Clippy fail | Focused offline tests, WASM check and Clippy. Boxing must preserve serialized intent. |
| Unix-only example import | Windows builds must compile without unused imports | Windows compiler diagnostic | Windows service job; no lifecycle redesign. |
| Replay fixture bootstrap threshold | Selection must follow bootstrap exit | Integration fails at explicit selection preparation | Run the affected integration test with a two-human threshold; keep the guard. |
| Governance fixture certified runtime | Closed ballots survive restart and fresh catch-up | Old shard 3 rejects historical blocks after 60 seconds | Enable the existing test clock hook with wall time. Run the two ballot journeys and content journey. Do not relax timestamp validation or enlarge waits. |
| Coverage evidence references | Fresh clones can inspect the sign-off record | Local ignored logs and committed ledger references | Check retained hashes, reference durable reviewed summaries, then run inventory contracts. No runtime replay needed. |
| Formatting | Repository formatting contract | Format job diffs | One mechanical format pass and check. |
| Workflow scheduling | Changes receive relevant checks without duplicate publication attempts | Documentation push repeated all jobs | Test workflow routing; make release manual, preserve conformance prerequisites. |
| Missing app workflows | Wallet/dashboard build and browser contracts | Existing local test drivers, no hosted checks | Add reproducible frontend CI using those drivers. Native installation remains a separate qualification. |

The next broad boundary is one coherent repair push. Reuse unaffected passing
workspace and installer results for diagnosis; hosted CI still validates the
new revision. Do not rebuild the release daemon for fixture or metadata edits.
The desktop limits remain unchanged. Full node test compilation stays off the
desktop.

## Release blockers and coverage limits

The inventory has 335 unsigned sections. Some may be stale records, but CI must
not infer sign-off from test counts. The release conformance gate remains closed.
The fresh-catch-up and replay failures remain unresolved until their positive
journeys pass. Browser mocks do not prove native signing, recovery, packaging,
cross-host networking or real VPS deployment. Those checks retain their own
evidence and cannot be replaced by a green frontend job.

Routine checks are useful: formatting, lint, platform compilation, tool
contracts, crate tests, network journeys and integration tests cover different
failure modes. Repeated compilation of the same node test profile and automatic
release attempts on documentation pushes do not add that coverage.

## Local results

- 43 workflow-routing, inventory, installer/updater, sharding and evidence-reuse
  contracts pass through the desktop resource wrapper.
- Four offline action tests pass, including serialized delegation, signing,
  durable journal readback and TRST provenance. WASM Clippy passes with warnings
  denied. These checks do not qualify a new installed wallet package.
- The inventory now reports zero structural errors, still 335 unsigned sections.
  The two retained log digests match their recorded values. Panel and challenge
  receipt digests match the ledger; both receipts record pass/exit 0. CI references
  the committed review, not a machine-local ignored path. Raw logs remain local.
- Rust formatting is applied mechanically across the workspace. No runtime
  sign-off is inferred from formatting.

Ignored-test review found a second daemon snapshot CLI test that was never
scheduled. It now runs beside the existing snapshot export test, using the same
validation-profile executable. Subprocess helpers are invoked by their parent
tests; they must not be run indiscriminately with `--ignored`. Installed-wallet,
public NTS/drand, large-state and performance probes still require their named
validation environments. Routine CI is not a substitute for those release checks.

## Coverage and reuse follow-up

The hosted dashboard frontend and Linux native-command jobs pass at e6cd90c.
The deck build and server syntax check pass at 6e40a1c. Wallet's first hosted
unit job exposed its undocumented sibling-dashboard fixture dependency. The
workflow now checks out the pinned dashboard beside the wallet; the local 30-test
contract suite passes. Hosted wallet browser/native results are still pending.

Protocol d5a899a passes hosted format, Clippy, WASM, inventory, workspace tests,
compile and daemon health checks. Network and full integration results remain
pending; none of those claims is inferred from the small local checks.

The follow-up workflow compiles the node test binary once and distributes the
same SHA-addressed artifact to all six component/network workers. Downloads
verify the binary checksum before execution. Local runner contracts prove that
prebuilt mode preserves the catalog, exact filters, sharding, failure exits and
CPU limits without invoking Cargo. Hosted artifact execution remains to verify.
The existing runner is extended; there is no second test selection framework.

Deployment identity, NAT-readiness, NAT-runner and desktop-limit contracts were
missing from CI. The inventory job now discovers all `tools/*.test.mjs` files.
Their focused local run passes 17 tests; the Debian-package fixture is skipped
because this desktop lacks Debian packaging tools. Ubuntu CI must run it.

The health job now builds the private-testnet configuration with the published
disposable fixture key, then checks Dev/Test/Live isolation and restart behavior.
It uses the validation profile. Optimized executable smoke checks stay in the
manual release workflow. This fixture artifact is never a release candidate.

CI still does not automatically qualify signed/notarized desktop packages,
external NTS/drand reachability, NAT hardware, large-state resource bounds or
the owner's production VPS. The release workflow also cannot sign off the 335
open specification entries. These are explicit coverage limits, not optional
requirements silently dropped from release readiness.

## Diagnostic handoff

Run 39313057623 is terminal. The `ci-repair-validation` branch allows explicit
Windows/integration/node diagnosis without repeating unrelated jobs. Manual
filtered runs are not full-suite passes.

Windows run 39314401208 proves startup stack overflow in both SCM and a foreground
probe (exit 0xC00000FD). Service command quoting and config resolution succeed.
Boxing node construction/start futures fixes the failure: run 39315332589 passes
daemon tests and the complete Windows service lifecycle, including clean restart
and bad config. Stack limits are unchanged. The supervisor-size regression passes.

Both governance ballot tests in run 39313057623 advance through their live
journey, then fail in the old genesis-only post-run replay helper. The helper now
supplies the certified prefix preceding the exact counting cut. The later
revocation remains excluded, and reversed block order must retain the same ballot
commitment. The fresh-observer result is not signed off until the whole case passes.

The payment replay integration test passes in 31.69 seconds in run 39315812086.
That run changes diagnostics, not protocol behavior, from the earlier 300-second
timeout. The timeout remains an unexplained intermittent failure; a single
targeted pass does not establish its cause or erase it. Stage markers remain for
the next integration boundary. No wait budget, guard or quorum was relaxed.

The full component run found eight failures: five stale snapshot proof fixtures,
two assertions naming `constitution` rather than the actual corrupted
`consti_engine` key, and a wire golden still encoding version 33 instead of 36.
Snapshot tests now supply required authenticated proofs on positive paths and
check rejection at the earlier planning boundary on missing-proof paths. Invalid
suffix tests also supply valid proofs so unrelated missing witnesses cannot mask
their intended rejection. Runtime proof requirements are unchanged.

Run 39317355166 targets those eight cases, both governance ballots, constitution
catch-up, the stalled checkpoint runtime and the compact outcome stack overflow.
The preceding attempt caught an obsolete test-only accessor after its last use
was removed; it did not execute tests. Compiler failures now render from the JSON
build record. The shared executable builds and transfers successfully. The last
two network cases remain diagnostic work, not completed fixes. No node compiler
runs on the desktop.

Wallet f721634 passes the targeted reduced-motion and compact-window checks in
run 39317046171. The motion test now selects the actual origin disclosure. The
layout fix gives the signing-device message a flexible basis so its action can
stay alongside it; no warning, text size or hit target was removed. Emil's layout
guidance informed that change. Full wallet CI is run 39317237388.

Filtered browser diagnosis must select the relevant file: filtering across every
file left a no-matching-test browser process alive until the job timeout in
39314468567. The focused dashboard run completes in seconds. Full runs still
select every browser test. Network shards now finish their remaining isolated
cases after a failure and return failure with the exact names; runner contracts
verify this in both Cargo and prebuilt modes.

## Which checks are necessary

- Keep native tests, WASM, Clippy, formatting, tool contracts, network/recovery
  journeys and platform lifecycle checks. Their assertions differ.
- Remove standalone workspace `cargo check`: Clippy checks the same default
  workspace targets with the same locked dependencies and warnings policy.
- Compile the node test executable once, not independently in six workers.
- Documentation-only changes run inventory/tool contracts. Unknown inputs still
  trigger runtime checks. Release validation always runs the full suite.
- Release builds wait for conformance **and** validation. Publication remains
  manual and still depends on all five platform builds.

The GitHub API reports main is not branch-protected. CI reports failures but does
not itself prevent a direct push. Changing repository governance is separate from
repairing the workflow; no protection policy has been silently imposed.
