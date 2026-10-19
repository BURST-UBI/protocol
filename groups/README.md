# Off-chain groups

Group membership is a wallet preference, not a node consensus rule. A group
score or member count never supplies voting weight. The client checks the
original BRN creators behind a payment, not just its immediate sender.

## Public directory format

`GroupClient::fetch_directory(url)` reads this JSON over HTTP(S). An HTTP IPFS
gateway can serve the same document; the library does not resolve `ipfs://`
addresses itself. No default directory or publishing service is configured.

```json
{
  "version": 1,
  "groups": [
    {
      "id": "example-community",
      "name": "Example community",
      "description": "Describe how this group verifies its members.",
      "endpoint_url": "https://example.test/community",
      "member_count": 42
    }
  ]
}
```

The listing is untrusted. Loading it neither selects a group nor contacts any
listed endpoint. `member_count` is self-reported. The entire import fails for an
unsupported version, duplicate ID, invalid entry or response over 1 MiB. Adding
an existing ID to a registry fails instead of replacing its endpoint silently.
IDs and names are limited to 128 bytes, descriptions to 4,096 bytes and URLs to
2,048 bytes. URLs cannot contain credentials, a query or a fragment. Redirects
are not followed.

## Publish a directory

Create a draft JSON file in the format above. A group can publish a listing
containing only its own endpoint; no central registry or node transaction is
required. From the protocol repository, validate and export the draft with:

```sh
bash tools/limited-run.sh cargo run -p burst-groups --bin burst-group-directory -- draft.json directory.json
```

The output path must not exist. The command validates every entry, sorts groups
by ID and checks the wallet's 1 MiB limit before creating the output. It makes
no network requests and never uploads anything. If a write fails, discard that
incomplete output and export again to a new path; do not serve a failed export.

Serve the completed `directory.json` as a static file on your HTTPS site, or
publish those bytes through IPFS and share an HTTP(S) gateway URL. Keep the
membership API at the listed endpoint available separately. Share the directory
URL with users; in wallet setup or Settings they load it, inspect the groups,
select those they trust and save their policy. Publishing a listing does not
add a trust root to anyone's wallet. Endpoint changes require explicit review
rather than replacing a saved endpoint through a refreshed directory.

The export-to-HTTP-to-client integration check is
`cargo test -p burst-groups --test directory_publication`. Installed Linux
acceptance also passes with `NATIVE_DIRECTORY_EXPORTER` pointing to the exporter
and `native-session.mjs --embedded --groups-only`. It checks Settings loading,
explicit selection/save, pinned endpoints and real-ledger group policies.
Evidence is in `../target/validation-evidence/directory-acceptance-20261017/`.
These runs use loopback HTTP, not a public host or IPFS deployment.

## Membership API

For the example endpoint above, the client requests:

```text
GET https://example.test/community/verify/{original_creator_address}
```

```json
{"valid": true, "score": 0.9, "since": 1700000000}
```

`valid` and `score` are required. Scores must be finite and between zero and one;
`since` is optional. Responses are limited to 64 KiB. The default timeout is ten
seconds and multi-group requests run at most eight at once. Every evaluation
queries live membership; no result is cached for a later payment.

The wallet-core payment evaluator can use an explicitly selected protocol
fallback for connection/timeouts or HTTP server failures. A negative membership,
malformed response, missing group configuration or incomplete/unconfirmed origin
report is not an outage fallback. An explicit negative from a later creator
cannot be hidden by an earlier warning. Revoked contributions are already
removed by protocol accounting and do not taint surviving honest value.

The desktop wallet exposes directory loading and explicit group selection during
setup and in Settings. Users choose protocol-only checks, any selected group per
creator, or every selected group per creator. They can keep payments pending
during an outage instead of using the default protocol fallback. Invalid saved
rules pause automation until repaired. Manual receiving does not require group
approval.

The real-node desktop fixture tests merged-payment original creators, any/all
rules, live responses, outages, manual acceptance and cancellation. See iteration
57 in `../REFERENCE_IMPLEMENTATION_REVIEW.md`. Operator-exported listings now
also pass the installed-wallet check above. Public deployment remains unqualified.
Weighted custom rules are an optional extension, not a selected requirement.
A node-reported
origin list is not an independent proof or a contribution-weight breakdown.

## Group-to-group trust

Direct membership remains the default. The wallet's "Groups they vouch for"
setting permits up to eight group-to-group steps. Select the groups you trust
directly, then add any other groups whose endpoints the wallet may query. An
unchecked group is not a new trust root; it needs a live endorsement chain from
a directly trusted group. Each original creator must meet the selected any/all
root policy. Manual receipt still bypasses these optional application checks.

Groups participating in this extension serve an additional REST resource:

```text
GET https://example.test/community/trust
```

```json
{"version": 1, "groups": ["another-community"]}
```

The versioned list contains at most 32 distinct registered IDs, not URLs. Only
the receiver's saved registry resolves their endpoints. An endorsement cannot
replace a saved endpoint or import a group from a directory. The wallet queries
membership and endorsements live, follows no redirects, and keeps no responses
for later evaluations. Cycles are visited once; traversal uses the shortest
discovered path and checks at most 64 groups. The selected depth bounds which
paths can count. Each response is limited to 64 KiB and evaluation to 30 seconds.

A complete search with no membership rejects automatic receipt. Missing groups,
malformed replies and exhausted resource limits require review, not protocol
fallback. Actual endpoint outages can use the receiver's explicit fallback
choice. A valid alternative path may satisfy an any-path check despite a failed
branch. A server without `/trust` cannot establish a transitive path, but its
ordinary direct membership endpoint still works.

Queries disclose creator addresses to the groups visited. Group statements are
informational and authenticated only by the configured transport, not on-chain
certificates. Use HTTPS for remote endpoints. Scores and graph size never grant
protocol voting power or change money. Tests cover library traversal, every
original creator in a payment, saved-rule migration and the settings controls.
A native Tauri/WebKit journey against a fresh local Dev ledger also passes real
receipt, changed endorsements and explicit fallback. Packaged-platform
validation remains separate.
