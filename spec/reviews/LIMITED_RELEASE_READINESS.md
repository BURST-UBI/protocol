# Limited initial release preparation

October 30 source follow-up: network hardening differs from the frozen candidate.
See [NETWORK_HARDENING_REVIEW.md](NETWORK_HARDENING_REVIEW.md) for current changes,
validation status and retained limits. October 29 statements below describe that
frozen review; they do not qualify the subsequently modified source.

Review completed on 2026-10-29. This records completed preparation and retained
qualification limits. It does not authorize publication or qualify a Live launch.
The frozen product binaries were not changed or rebuilt during this review.

## Completed preparation

| Requirement | Evidence and conclusion |
| --- | --- |
| Candidate identity | Reviewed all 1,093 frozen daemon source entries. Only validation tooling and its workflow document differ; no product source difference. All 31 Cargo manifests and five dependency lockfiles match the frozen dependency inputs. |
| Safety evidence reconciliation | Original wallet/dashboard audit scopes retained. The obsolete dashboard diagnostic is superseded by the verified retry-fixed runtime audit. Current-daemon focused/mixed proofs and exact-platform installation/connection evidence remain separate. Five retained log files match original receipt hashes using the existing validation-reuse inventory helper. No demonstrated product defect requiring another build was found in this reconciliation. |
| Retirement gate | Exact archived source defaults body retirement off and rejects enablement in production. The gate stays closed. |
| Dependency notices | Twelve target graphs resolved; collection covers 692 selected Rust and frontend dependencies, including nested vendor notices and patched heed/LMDB. Five packages use clearly identified declared-license notices; no copyright owner/year was invented. |
| Installer notices and sources | Pinned NSIS 3.11 and WiX 3.14.1 archive checksums verified; their notices and nsis-tauri-utils 0.5.3 terms included. Eight MPL package source archives match frozen lockfile checksums. Frozen daemon sources include patched-library source, licenses and change records. |
| Sidecar delivery | `/home/developer/Downloads/Burst-release-notices-20261029.zip`, SHA256 `c6d045d51878a42782f3158c9a4caaadb8e7ffc3502747f958d680172e647d02`. All 14 indexed files and ZIP integrity verified. Original signed apps, DMGs and installers remain unchanged. |
| Operator documentation | `docs/release/DOWNLOADS.md` records supported-environment limits, backup scope, uncertain-action recovery and upgrade cautions. |

## Evidence limits and gates retained

- Physical storage power-loss qualification remains unproven; no appropriate
  physical test host has been established. Synchronous LMDB commits and process
  crash/restart tests do not close that hardware gate. Retirement stays disabled.
- Existing consumer recovery evidence retains its original daemon identity.
  Platform installation/connection checks do not establish a new uninterrupted,
  funded lifecycle on every final app/daemon pairing. The reviewed change-impact
  policy does not require repeating every journey merely because a binary hash
  changed. No additional named affected assertion was identified in this review;
  do not inflate composite evidence into a broader end-to-end claim.
- Windows installation evidence used an existing WebView2 runtime. The default
  first-install bootstrapper download path is not qualified by those runs.
- SignPath is on hold at the owner's request. Windows artifacts remain unsigned.
- Live-network launch and public publication are not authorized. This preparation
  does not change the frozen Test candidate into a Live release.

The owner deferred the unproven 100+ TPS/subsecond target, further RAM optimization
and Pi 4 performance claims. Preserve existing measurements and failures; none of
these deferrals is a passing result or a promise of capacity.

## Change-impact and reuse decisions

| Changed dependency | Affected check | Existing artifact | Decision |
| --- | --- | --- | --- |
| Documentation and notice sidecar | Claim accuracy, attribution, integrity | Frozen manifests; notice/source receipts | Review and hash verification only; no runtime build |
| None: product source | Existing safety assertions | Prior scoped app audits; current-daemon focused/mixed evidence | Reuse recorded scopes; no unnamed broad acceptance rerun |
| Validation tooling/documentation | Receipt recording and scheduling | Original receipts/logs and saved host audits | Preserve original records; local copied-log verification is not a replacement remote input audit |
| No changed executable | Platform installation/connection | Existing Linux, Windows and Mac receipts | Keep exact identities; no reinstall or re-signing |

Evidence root: `target/validation-evidence/limited-release-readiness-20261029/`.
Key records: `frozen-source-review.json`, `dependency-manifest-review.json`,
`dependency-graphs.json`, `dependency-graphs-online.json`,
`safety-reconciliation.json`, `retained-evidence-integrity.json`,
`installer-notice-review.json`, `sidecar-delivery.json` and `completion-audit.json`.

Public release still requires a separate decision against the retained gates and
exact supported scope. No hardware result, signing approval or publication is
implied by completion of this preparation work.
