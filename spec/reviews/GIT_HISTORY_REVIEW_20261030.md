# Git history and identity review — 2026-10-30

## Result

No confirmed credential leak or matching Live genesis seed was detected in the
scanned text. This is not an assertion that repositories contain no personal or
operational information, or that arbitrary binary/encoded secrets are impossible.

Remote main history contains personal-email author/committer metadata in 56
protocol commits, 3 wallet commits and 3 dashboard commits. Old protocol history
also contains `burst_data/data.mdb` and `burst_data/lock.mdb`; deleting these from
current source did not remove old objects. These runtime artifacts can expose
ledger/peer operational information and should not have been versioned. Current
remote main trees no longer contain those runtime/dependency artifact directories.
Names, public keys, public certificates, release signatures, public endpoints and
ordinary authorship are not automatically secrets. Source documentation and
validation notes also contain local paths and operational identifiers; this
review does not promise anonymity of the project's owner or infrastructure.

## Standard future-commit configuration applied

At the owner's latest request, all three repositories now use the owner's existing
GitHub handle and GitHub-provided `users.noreply.github.com` address for both Git
author and committer. The temporary project/invalid-domain identity was replaced.
Only repository-local `user.name`/`user.email` changed; global settings, signatures,
working branches and existing commits did not. No commit or push was performed.
This hides the personal mailbox in future ordinary local commits; it does not
anonymize the linked GitHub account. GitHub web/merge settings and explicit author
or environment overrides are separate and can override local configuration.

Keep historical attribution intact unless a deliberate cleanup is authorized.
A `.mailmap`, `.gitignore` or future email change cannot erase existing history.
If old personal metadata/runtime objects must be removed, prepare rewritten
copies, review all affected branches/tags/PR refs and coordinate a force-push and
clone replacement. Hosted PR refs, caches and existing third-party clones can
retain old content. No such rewrite was performed. Credentials, if ever found,
need revocation first; rewriting alone does not make them safe again.

## Scan coverage and dispositions

Evidence directory: `target/validation-evidence/mainnet-history-20261030/`.
Reports are local and ignored by Git. Secret scanner output is fully redacted;
identity inventories are private local files, not committed audit attachments.

* Gitleaks v8.30.1 was downloaded from its official release and checked against
  the published checksum. Default rules were explicitly enabled, without project
  allow-comment suppression or a finding baseline. Scans ran under limited-run.
* Scanned available branch/tag/reflog history plus every locally stored text blob,
  commit and tag object, including dangling objects. Also scanned tracked and
  nonignored untracked working files; no external private key files were read.
* Compared advertised remote refs with local objects. Fetched missing remote
  history into isolated `refs/audit/mainnet-history-20261030/*` refs, including
  advertised pull-request refs. Scanned the newly fetched objects/commits only,
  reusing the initial results for unchanged objects. No working branch moved.
* The combined object inventories contain 213 commit objects and 19,482 blob
  objects across three non-shallow repositories; initial working-file inventory
  covers 1,776 files. Binary objects were counted but not parsed as text (884
  skipped binary objects/files across the initial scan counters). Archives,
  binary database contents, Git LFS payloads absent locally, hosted build logs,
  unadvertised server refs, releases and chat history are not exhaustively covered.
* Initial protocol history produced 39 generic-key candidates: manifest hashes,
  a public fixture identity, a localStorage draft key and ordinary prose. The
  all-object/working scan also found vendor documentation/generated-code examples.
  Reviewed candidates did not establish issued credentials. Newly fetched history
  produced no additional scanner findings. Public evidence PEM was a certificate,
  not a private key.
* A separate check derived Ed25519 public keys from 4,689 unique 64-hex/repeated-byte
  seed candidates in initial text and compared them with the pinned Live identity:
  no match. The remote-delta check also found no match. This does not cover every
  possible seed/mnemonic encoding and is not a reason to expose the actual seed.
* Targeted app-password-shaped strings and password assignments were reviewed:
  test inputs, dependency examples and a hyphenated validation scenario name.
  No Apple app-specific credential was identified in those candidates.

Initial/delta `summary.json`, mapped redacted candidates, `live-key-check.json`,
`remote-refs.json` and `exposure-review.json` preserve the scope. Scanner exit files
record terminal success; scanner success means execution completed, not absence
of every class of leak. Two setup commands used an incorrect relative path and
started no scan; correcting the absolute path resolved that setup issue. No
failed scenario was relabeled as a pass and no unchanged full scan was repeated.

## Remaining precautions

Existing ignore rules already exclude `.env`, seeds, PEM/private-key containers,
local build output and protocol LMDB files; history still needs separate review.
Public certificates may be intentionally distributed even though private key
containers must stay private. GitHub noreply is an authorship privacy measure,
not secret scanning or a guarantee that names/emails never appear in file bodies.

The Apple app-specific password previously pasted into the conversation should be
revoked/replaced independently; a Git scan cannot remove it from chat history.
No credential was printed or tested against a live service during this review.

See [the mainnet bootstrap review](../../docs/node/NETWORK_LAUNCH.md) for the
launch procedure and outstanding timestamp/funding decisions.
