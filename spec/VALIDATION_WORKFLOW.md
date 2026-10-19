# Validation without repeated work

The October 18 catch-up work repeatedly built optimized node tests and a daemon
for small changes. Each attempt also copied sources to a different build path.
Shared dependency artifacts survived, but that did not prevent recompiling the
large node crate. Subsequent live failures exposed costs that a stage-timed run
could have distinguished earlier. Keep that failure mode out of future work.

## Before spending build or test time

### Finish one candidate instead of restarting acceptance

Keep the selected daemon, wallet and dashboard fixed while closing release
gaps. Change production code only for a demonstrated blocker. Test-driver,
documentation and evidence corrections do not create a new product candidate.

| Change since the last pass | Work to schedule |
| --- | --- |
| Documentation or stale status | Review the correction. No runtime test. |
| Test setup, logging or resume handling | Check the changed driver, then run only the unresolved stage. Keep earlier assertions and failures. |
| Later ledger tip, another payment or newer snapshot | Reuse the established capability proof. Run again only if the new state exercises a required boundary absent from that proof. Recency alone is not a requirement. |
| Public creator key only | Verify identity, genesis, authority isolation and network separation on the public binary. Reuse functional staging evidence only with a reviewed source/build comparison proving that identity is the sole difference. |
| Production dependency or supported environment | Identify affected assertions from the actual diff. Run those checks and the affected integration journey; retain unrelated passes. |
| Final distributable bundle | Check exact package hashes, provenance, installation and connection once per supported target. Do not recreate every protocol journey to check packaging. |

Before the next cross-host, NAT, empty-observer or in-flight run, compare its
existing evidence with the selected candidate's relevant dependencies. A changed
binary hash requires an impact review, not automatically every scenario again.
If dependency coverage is unknown, inspect it before deciding to run. Record any
remaining uncertainty and the smallest check that resolves it.

Distinguish protocol deadlines, promised user-facing limits and test-runner wait
budgets. A failed wait stays failed. Determine which requirement it tests before
making uninterrupted replay of the entire journey a release blocker. Composite
evidence can prove its individual assertions, but cannot prove uninterrupted
operation or a timing bound it did not meet. Do not enlarge waits to hide failure.

The next broad boundary is the final identified release bundle, unless a blocker
changes shared production behavior first. Use the existing receipt tool and
drivers; do not add another validation framework for this scheduling rule.

Record four things in the completion ledger or run plan:

1. The requirement and concrete behavior under test.
2. Which inputs changed since the existing evidence.
3. The unresolved question and the cheapest check that answers it.
4. The integration boundary that will justify the next broader run.

If inputs and question are unchanged, reuse the result. For an intermittent
failure, record a reproduction hypothesis and a fixed attempt budget before
retrying. Do not rebuild an unchanged binary to retry it.

Include a change-impact table in the run plan, with these columns:
changed dependency, affected requirement/check, existing receipt/artifact,
reuse or rerun, and reason. Map actual dependencies, not just filenames. A
shared consensus change can affect several capabilities; a documentation edit
does not require a node build. Test-only changes require the affected tests,
not a new daemon when its build inputs are unchanged. Preserve earlier passing
filters from the same valid inputs instead of rerunning the entire group.

At each terminal run, replace the ledger's current handoff with the candidate
identity, result and evidence paths, remaining uncertainty, and next cheapest
check. Keep historical logs, but remove stale claims that completed work is
active. After interruption, reconcile that handoff with terminal results and
process state before scheduling anything. A stale status is a documentation
problem, not a reason to rebuild or repeat acceptance.

Before implementing or testing an item marked open, search the existing code,
drivers and terminal evidence for that requirement. Classify the gap as missing
implementation, missing assertions, missing candidate qualification, or stale
documentation. Only the first three justify development or validation work.
For stale documentation, reconcile the current plan and reuse-table rows with
the retained evidence. Keep historical failures intact. A narrower passing
scenario must not close a broader requirement.

Validation logs are commonly gitignored. Within the named evidence directory,
use `rg --files --hidden --no-ignore <directory>` or inspect the exact recorded
path before declaring an artifact missing. Plain `rg --files` can list the
scripts while hiding their adjacent terminal logs. Keep this search scoped to
the evidence directory; do not scan private keys or unrelated application data.

After a result, update every affected current summary row in the same edit.
For example, an independent-host pass must remove "independent-host remains
open" from the preceding snapshot row, while retaining its NAT and epoch limits.
Do not rerun either scenario merely to make their reports look uniform.

Do not build another validation framework when the existing receipt tool,
driver, fixtures or compiled executable can answer the question. Extend an
existing check only when its assertions miss a required behavior. Keep fresh
join, retained-state recovery and source-node restart evidence separate.

## Build once per coherent change

### Choose the build profile once

The retained node builds took about seven minutes each; the corrected
arrival-order test ran in 0.80 seconds. Compilation, not that test's execution,
was the measured cost. Do not solve this by reducing assertions.

First reuse a matching compiled executable. For the next required functional
build, `tools/test-node.sh` uses the `validation` profile, which inherits `test`
and omits debug symbols. Test debug assertions and the existing optimized crypto
dependencies remain. Compile the full node test crate only on the capped host.
Keep its target directory stable across batches. The initial profile build may
be cold; no speedup is claimed until its stage timings are recorded.

Use release builds for candidate qualification, release-only behavior or
performance measurements. Record that reason before compiling. Do not switch
profiles mid-investigation, rerun existing passes to populate a new cache, or
replace release evidence with development-profile results. The existing remote
release build remains usable for its original inputs and scope.

### Keep successful stages when a later stage fails

Record build, type-check, environment preflight and scenario execution separately
when they can be reused independently. Use the existing receipt format for each
stage, with its own actual dependency inputs and terminal log. Do not create
another runner or reporting system. A combined script should retain stage exit
codes and output so the failure location is clear.

Before compiling, check executable paths, required fixture files, available
disk/memory and port ownership. If browser startup previously failed, establish
that it can start under the same resource limits before another browser suite.
Do not run this smoke check before every healthy test run. Preserve its result
until the relevant environment changes or a new launch failure occurs.

Check receipt readability as the service user, not only as the deployment user.
For generated shell drivers with embedded JavaScript, check both languages before
starting the service. A preflight failure before a start record is not a test
failure; retain its diagnostic without rebuilding the executable.

The installed-recovery integration group exceeded the 20-minute job cap on
October 19. Split its long retirement journeys from shorter cases; do not repeat
the whole group after its deadline. Use libtest's JSON events and per-test timing
for new runs so completed, failed, ignored and unfinished cases remain distinct.
Keep the same runtime cap and the resource wrapper's 10-second stop grace on
remote units too. Confirm the owned unit has no remaining processes before the
next job. A timeout remains incomplete even when earlier cases passed.

A browser launch failure does not invalidate a completed type-check. A network
timeout does not invalidate a built binary. Reuse those stages after checking
their input identities; investigate the failing stage without rebuilding them.
If a historical combined run has no separate stage receipt, retain its logs as
historical evidence. Never manufacture a prospective receipt after the fact.
Its partial successes do not qualify the full scenario.

Use the same immutable fixture baseline for scenarios whose requirements allow
retained state. Copy it only when the test mutates it, and hash the baseline.
Fresh-install and fresh-onboarding requirements still need empty state. Before
writing another driver, search the existing tools and tests for that behavior
and identify the missing assertion in the run plan.

When extending a scenario to a different protocol state, check the fixture's
prerequisites before launching workers or a full user journey. For example,
signed governance history alone does not establish initialized payment
authority. Use the production initializer or validator to check the baseline;
do not bypass the guard or reproduce its rules in a second fixture validator.
Keep this assertion in the existing test setup, not a separate test framework.

After two unsuccessful attempts at one scenario, review the retained failures
and stage timings before scheduling another attempt. Record which assumption
failed, what evidence supports the diagnosis, and the smallest check that can
distinguish it from other causes. A new test name or a longer timeout is not a
diagnosis. Preserve successful build and scenario stages throughout this review.

Each progress handoff should list work reused, work newly run, and why the next
expensive check is necessary. If a change affects instructions or evidence
summaries only, review those edits without building the protocol. Receipt-tool
changes need their focused regressions, not a wallet or network acceptance run.

A valid receipt can describe a failed run. `check` verifies its identity and
integrity; a zero checker exit is not a passing build. Before starting a dependent
stage, require the saved result's `outcome` to be `pass` and its terminal exit
code to be zero. Never use `check ... && start-tests` as that gate by itself.

The ledger's current handoff should contain a short reuse table: artifact or
latest receipt, proven scope, what invalidates it, and next action. Keep failed
or environment-blocked checks in this table too. Never interpret an old result
as fresh evidence about a live external service.

Use one stable source path and target directory on each validation host for a
given toolchain, profile and feature set. Update only changed inputs after
checking the workspace belongs to this task. Preserve unrelated edits. Keep
dated evidence directories, source manifests and immutable candidate copies
outside that workspace. Do not relocate the source tree for each candidate.

Current VM1 workspace to retain: `/home/burst/export-handoff.fzkHaK4F/source`.
Its existing target is `/home/burst/native-validation-20261014.QzM89N/target`,
with `nightly-2026-10-11`, release profile and `private-testnet`. Check these
against the running host before use. The dated name is not a reason to move it.
Keep `/home/burst/export-handoff.fzkHaK4F/candidate` and its evidence immutable;
future candidate outputs go elsewhere. Record source changes before updating
this now-reusable workspace; old manifests do not describe later edits.

Use bounded compile checks during implementation. Batch the fix and all relevant
regressions before compiling the test executable. Run related filters together
or reuse that exact executable for further filters. Check that selected tests
actually ran. Cargo's normal freshness checks remain necessary after edits.
Do not invoke a stale executable as evidence for changed source.

Build the daemon once after the focused checks pass. Use that same hashed binary
for diagnostics, restart and fresh-join checks. Test-only edits do not justify
rebuilding the daemon when the production dependency inputs are unchanged.
Changing production inputs requires a new candidate and relevant acceptance.

Before packaging an economic correction, trace the calculation through the
engine, RPC, SDK/native commands and frontend-derived values. Check the order of
operations as well as final values. The October 19 overflow work initially missed
the frontend's subtraction-before-range-check behavior, requiring another wallet
package after the native correction. Inspect all consumers before that boundary;
do not turn this source review into another mandatory full-suite run.

After a performance failure, collect stage timings with the existing binary.
Separate compilation, export, peer wait, replay, publication and UI wait time.
Fix the measured cause in one batch. Do not repeat full acceptance after each
speculative optimization. Unchanged failures stay in the ledger.

## Reuse evidence, not assumptions

Before retrying a multi-stage workflow, audit its existing stage receipts on
the host and working directory that produced them:

```sh
node tools/validation-reuse.mjs audit stages.json
```

`stages.json` is an array of `{ "name": "build", "plan": "build-plan.json",
"receipt": "build-receipt.json" }` entries. Supply the latest receipt for each
stage. The audit reads and hashes evidence; it never runs a command or writes a
receipt. It reports `reuse` for a verified pass, `diagnose` for an unchanged
failure or incomplete run, and `unverified` for changed or missing evidence.
Exit 1 means at least one stage needs attention, not that all stages must rerun.
Use `prepare` for any subsequent attempt. This audit cannot discover undeclared
dependencies, older receipt branches, or changes to a live host.

For a late failure in a stateful wallet journey, retain the app profile, action
journal and node data. Inspect confirmed or certified-excluded action status
before resuming through the existing driver. Do not repeat funding, endorsements
or signatures to reach the failed step. A resumed diagnostic does not replace
fresh end-to-end acceptance.

`tools/validation-reuse.mjs` checks declared input and evidence contents. It
does not run tests, infer dependency closure, verify reviewer honesty, or open
release gates. Review the actual assertions against the specification first.
It cannot prevent someone from bypassing these instructions.

A plan JSON has `requirement`, `scope`, `command` (an argument array), `inputs`
(file/directory paths), and `context` strings for `toolchain`, `features`,
`network`, `topology` and `limits`. Use explicit `not applicable` where needed.
Include the executable, driver, fixtures, configuration and any external-input
capture. For source checks include the full relevant dependency source trees,
Cargo manifests/lockfile, build scripts and toolchain configuration. Include
generated and gitignored inputs when used. Hashing a manifest alone does not
check the files named inside it. Directories include new and deleted files;
there are no implicit exclusions. Do not include build output or growing logs
inside an input directory. Symlinks must be resolved explicitly.

From the same working directory:

```sh
node tools/validation-reuse.mjs begin plan.json start.json
# Run the planned check under the existing resource wrapper or capped host unit.
node tools/validation-reuse.mjs finish start.json result.json receipt.json
node tools/validation-reuse.mjs check plan.json receipt.json
```

For a subsequent attempt, replace `begin` with:

```sh
node tools/validation-reuse.mjs prepare plan.json prior-receipt.json start.json
```

This refuses unchanged attempts, including failures and incomplete runs. Changed
inputs or questions produce a new start record with the reason; they do not
inherit a pass. If evidence is missing, investigate the missing record first.
Do not edit a scope label just to defeat this check. A retry for suspected
intermittency must describe the hypothesis and attempt budget in the plan scope.
Use `begin` only for the first recorded attempt of a check, not to bypass a
refusal. Historical runs without receipts remain historical evidence; do not
invent pre-run records for them.

When inputs are unchanged, `prepare` also requires a `rerun` object with a
nonempty `reason` and positive integer `attemptBudget`. A new label or command
alone cannot authorize another run. The reason must identify the new question
or intermittent-failure hypothesis. The tool carries the consumed budget through
the receipt chain and refuses to raise or reset it while inputs are unchanged.
Always supply the latest receipt, including failures. Renaming a scope does not
reset the budget. Prior logs must still match before a new attempt is authorized.
Moving or renaming identical input files also requires a reason and consumes the
same retry budget. This conservative guard compares file contents, permissions
and directory counts without paths. A real path-sensitive change can justify a
bounded run, but cannot inherit a pass. Evidence reuse still requires the exact
path-sensitive inventory, command and context.
This is a sequential workflow guard, not a global scheduler: supplying an older
receipt or starting a separate chain can bypass it and is prohibited.

The result JSON supplies `outcome` (`pass`, `fail` or `incomplete`), terminal
`exitCode`, executed `assertions`, `review` explaining what the evidence proves,
and `logs` paths. No zero-test pass. Records are write-once. Inputs changing
between begin and finish invalidate the attempt. Missing or altered evidence
invalidates reuse. Failed records remain failures and tell the caller not to
repeat an unchanged run. Keep receipts private; they contain paths and context.

Reuse a retained ledger for diagnosis or restart testing. Keep an immutable
copy and record its identity. It does not prove fresh onboarding. Old unit-test
evidence also cannot qualify a different release binary. External services,
time-dependent behavior and topology changes need captured context or a new
check; matching local hashes alone is insufficient.

All desktop and remote resource caps still apply. This workflow adds no runner
and provides no uncapped execution path.

Docker containers need their own limits; a capped shell does not constrain work
started by the Docker daemon. The NAT runner requires prebuilt binaries and an
existing lab image, records their identities, and never rebuilds either. Reuse
them after checking the candidate and helper build inputs. A missing artifact
calls for that build stage only, not rebuilding every prerequisite. Changes to
the NAT driver require its affected checks, not a new protocol binary. Runner
safeguard tests do not prove NAT connectivity or signed transaction behavior.

For artifacts already on validation hosts in the same private network, prefer a
direct host-to-host transfer over relaying large binaries through the desktop.
Verify exact hashes through the authenticated control connection. If a temporary
transfer server is necessary, expose only an explicit artifact directory with no
keys or user data, bound its lifetime/resources and stop it before reusing the
port. Do not open firewall rules or trust downloaded artifacts merely because
the transport stayed inside the private network.
