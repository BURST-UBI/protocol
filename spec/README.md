# Engineering records

User, operator and developer documentation lives in [docs/](../docs/README.md).
This directory holds design decisions and the records used to qualify changes.

| Location | Contents |
| --- | --- |
| [Qualification handoff](COMPLETION_LEDGER.md) | Remaining release boundaries and evidence reuse |
| [Validation workflow](VALIDATION_WORKFLOW.md) | Resource limits and how to select and record checks |
| [Decision resolutions](DECISION_RESOLUTIONS.md) | Recorded resolutions of conflicting requirements |
| `design/` | Detailed implementation contracts |
| `reviews/` | Dated audits, fixes and their validation limits |
| `evidence/` | Preserved original reports, run history and documentation baseline |
| `coverage.json` | Requirement inventory consumed by CI and release checks |

## Design notes

- [Selection deadline recovery](design/DEADLINE_RECOVERY.md)
- [Authenticated snapshot archives](design/SNAPSHOT_ARCHIVE.md)
- [Shared checkpoint-record migration](design/SHARED_ARCHIVE_MIGRATION.md)
- [Verification record storage](design/VERIFICATION_RECORD_NORMALIZATION.md)

## Reviews

Each review describes its recorded source batch. Its passing checks do not
qualify later changes or an entire release. Read the follow-up before reopening
an original finding.

| Review | Original report or related record |
| --- | --- |
| [Final code fixes](reviews/FINAL_CODE_FIXES_20261031.md) | [A1–A5 audit](evidence/final-code-reconciliation-20261030.txt) |
| [Structural fixes](reviews/STRUCTURE_FIX_TRACKER_20261030.md) | [Folder audit](evidence/folder-structure-audit-20261030.txt), [re-audit](evidence/structure-reaudit-20261030.txt) |
| [Simplicity fixes](reviews/SIMPLICITY_FIX_TRACKER_20261030.md) | [S01–S20 audit](evidence/implementation-simplicity-review-20261030.txt), [dependency choices](evidence/rsnano-divergence-justification-20261030.txt) |
| [Network hardening](reviews/NETWORK_HARDENING_REVIEW.md) | Scoped network and schema checks |
| [Release preparation](reviews/LIMITED_RELEASE_READINESS.md) | [Frozen release record](evidence/release-status-20261031.txt) |
| [Reference implementations](reviews/REFERENCE_IMPLEMENTATION_REVIEW.md) | Pinned rsnano, Nano and Tezos revisions and comparison scope |
| [Numeric policy review](reviews/NUMERIC_POLICY_AUDIT.md) | Historical parameter and encoding assessment; consult the [current parameter reference](../docs/protocol/PARAMETERS.md) |
| [Git history review](reviews/GIT_HISTORY_REVIEW_20261030.md) | Scan scope and identity findings |

Other retained reports cover [system safety](evidence/full-audit-20261030.txt),
[protocol consistency](evidence/protocol-consistency-audit-20261030.txt),
[the rsnano cross-check](evidence/rsnano-audit-crosscheck-20261030.txt),
[operation complexity](evidence/operation-complexity-audit-20261031.txt) and
[retirement capture](evidence/retirement-capture-design-20261031.txt).
The [historical plan](evidence/completion-plan-20261031.txt) and
[ledger](evidence/completion-ledger-20261031.txt) retain earlier task lists and
terminal results. Their "next" and "active" statements refer to those runs.

## Requirement inventory

`coverage.json` indexes implementation decisions and Markdown under `docs/`,
including subfolders. Section hashes bind
records to their source text. The [baseline](evidence/documentation-baseline/README.md)
retains earlier requirements and sign-offs. Editing documentation does not remove
an unresolved requirement or prove that it has been implemented.

Later explicit owner decisions govern conflicts with older text. Audit advice and
code changes do not independently authorize new protocol rules. Changed sections
require review before retaining a sign-off.

| Status | Meaning |
| --- | --- |
| `unverified` | No sign-off; implementation may already exist |
| `partial` | An implementation or validation gap remains |
| `decision-conflict` | Conflicting requirements need an authoritative resolution |
| `verified` | Scoped passing evidence and reproduction commands are recorded |
| `excluded` | Evidence and explicit scope authorization are recorded |

Run the inventory check from the repository root:

```sh
bash tools/limited-run.sh node tools/check-spec-coverage.mjs
```

`--list-open` lists unsigned entries, including the historical baseline.
`--release` fails while either index has unsigned requirements. The checker checks
hashes, record fields and evidence-file existence; it does not execute the tests
or establish that their results prove the claims. Follow the validation workflow
before scheduling runtime checks. Keep failed evidence and release gates intact.
