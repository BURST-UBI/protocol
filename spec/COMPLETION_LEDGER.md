# Qualification handoff

Documentation cleanup does not qualify a new release candidate. Source comments,
current explanations and requirement indexing have been reviewed separately from
runtime acceptance. Nothing is published by this handoff.

## Evidence to consult before scheduling work

| Record | Recorded scope | Reuse boundary and next action |
| --- | --- | --- |
| [Final audit fixes](reviews/FINAL_CODE_FIXES_20261031.md) | A1–A5 implemented; strict node test compilation and 85 focused tests recorded | Review changed dependencies before reuse. These are development checks, not packaged end-to-end acceptance. |
| [Structural fixes](reviews/STRUCTURE_FIX_TRACKER_20261030.md) | Folder ownership, service composition, wire and application boundaries | Preserve the scoped receipts. Published dependency pins and clean remote builds remain separate from private testing. |
| [Simplicity fixes](reviews/SIMPLICITY_FIX_TRACKER_20261030.md) | S01–S20 and D01 dispositions with focused evidence | Do not reopen old findings from the original audit without checking their resolution. |
| [Release record](evidence/release-status-20261031.txt) | Frozen candidate and platform evidence | Match exact artifacts and requirements before reuse; newer source is not covered automatically. |
| [Coverage index](README.md) | Current sections and unchanged historical requirements | Unsigned requirements remain unsigned. Inventory validity is not a test result. |

## Earlier November 1 CI repairs

Native workspace Clippy and the wallet-core/groups/web-runtime WASM Clippy check
pass. The twelve failing network tests now pass in focused runs: two ingress and
creator-authority tests, eight agreement and fork-recovery tests, and two live
publication crash-recovery tests. The intermediate ten-test run remains recorded
as eight passes and two failures; the two failures passed after correcting the
shared cache assertion's obsolete aggregate-state reads.

The fixtures now use existing normalized-state readers, Dev peer limits for
localhost meshes, distinct ingress payloads, and the existing post-bootstrap
creator-authority rule. Production peer limits, consensus rules, deadlines and
release gates are unchanged. A storage regression also checks empty and selected
metadata prefixes and callback-error propagation against direct database reads.

These are development checks, not a complete CI or package-acceptance pass.
Process-kill recovery does not establish physical power-loss safety. Private
receipts retain source identities, terminal results and earlier failed attempts.
Before restarting validation, inspect those records and actual process state.

The wallet's native command exposed a `Send` compilation failure in group trust
evaluation. Collecting the bounded futures before buffering fixes it without
changing trust decisions or the shared request limit. All 21 trust-policy tests
pass, including a new native `Send` regression, and WASM Clippy passes again.
Wallet and dashboard TypeScript and native test compilation pass. Their Node
unit/driver checks pass 30 and 2 tests respectively. These app checks do not
replace browser journeys, native signing journeys or platform package acceptance.

## November 1 candidate qualification

The selected daemon source is `675bf08c6a46fda53b0b03e209e0d23e58a679fd`.
Dashboard `247ec107c963e8f5851602c2e959239f8d582ae5` and wallet
`2dbebe93aab4d33fd4ebfeb5f667a32095eb31ea` are pushed and their CI passes.
The wallet's latest change is test-only. Dashboard `7e91452` subsequently fixes
the native focus test; its focused Windows UI run and full MSI native suite pass.
Protocol changes after the daemon selection repair test fixtures and CI scheduling;
they do not change its binaries.

| Evidence | Proven scope | Remaining action |
| --- | --- | --- |
| Linux package and native receipts | Exact Debian payloads installed; dashboard IPC/RPC/navigation; wallet security plus eight native journeys | Reuse within recorded inputs and assertions. |
| Mac runs 41301897029, 41303701554, 41304084806, 41304394252, 41304895220 and 41305448181 | ARM and Intel builds, packages, signing/notarization and native wallet/dashboard checks; daemon identity/restart checks recorded separately | Artifacts archived; no further Mac run required for these scopes. |
| Windows build, installer and native receipts | x64 daemon service checks; wallet and dashboard MSI/NSIS payloads and registration; native wallet-core and dashboard RPC/navigation/focus checks | Recorded scopes complete. Existing WebView2 only; first-time runtime download and public Windows signing remain unqualified. |
| Protocol CI 41305069783 plus isolated 41307174117 | Full run completed with one stale-fixture failure; the corrected target passes at `65c54f9`, with all other checks retained | Test-only correction; composite evidence, not a new full-suite run. |

Tauri JavaScript package versions now match the already locked Rust dependencies.
All 49 frontend files remain byte-identical to the selected assets; the retained
Linux packages are reused with an explicit dependency comparison, not relabelled
as new builds. Mac native wallet checks cover keypair/backup guards; Windows
wallet checks cover the existing fresh wallet-core journey. Neither scope implies
the full Linux wallet lifecycle passed on every platform. Windows wallet MSI
installation completed before its test used an obsolete product ID; package
metadata, registration and exact payload were verified without reinstalling.
Dashboard NSIS evidence combines the completed RPC/concurrent-read checks with
a focused UI pass after fixing an immediate focus assertion. The MSI dashboard
passes all three native drivers together. The existing focus wait deadline is
unchanged; the test now waits for the dialog library's deferred restoration.

The restart fixture used a positional-row reader after startup had migrated
holdings to stable rows. It now uses the production storage reader, retaining
independent outcome/identity revocation and corrupt-state rejection assertions.
Governance fixtures separately check the default release-gate refusal and the
experimental activation/replay path. No deadlines or production gates were relaxed.

Follow [the validation workflow](VALIDATION_WORKFLOW.md) and the private receipt
handoff before scheduling further work. Nothing is published; Live and physical
retirement remain gated. Website and video changes are outside this task.

## Historical evidence

The complete previous ledger, including candidate identities, failed attempts,
reuse tables and requirement IDs, is preserved in the [historical ledger](evidence/completion-ledger-20261031.txt).
Its “current,” “next” and “active” statements describe their dated runs. They are
not new instructions or proof that those jobs are still running. Read the relevant
terminal result before treating an old open item as unfinished implementation.

## Remaining qualification boundaries

The candidate above is frozen. Identify affected requirements before any further
change, and reuse only evidence whose inputs and scope still match. Private testing
does not authorize publication or Live launch.
Public signing/distribution, physical body retirement, production-parameter
performance and unsigned requirements retain their separate gates. Further RAM
optimization is deferred. A large synthetic ledger or isolated lookup does not establish
production throughput; fixtures must represent work, maturity, electorate and
traffic settings. Fresh onboarding and resumed recovery remain separate checks.

Earlier preparation and failures are retained in the
[release review](reviews/LIMITED_RELEASE_READINESS.md),
[complexity record](evidence/cx-remaining-work-20261031.txt),
[pilot record](evidence/testnet-pilot-20261031.txt) and
[CI record](evidence/ci-repair-20261031.txt).
