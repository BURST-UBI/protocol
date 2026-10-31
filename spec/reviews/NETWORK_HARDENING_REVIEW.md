# Network hardening follow-up, 2026-10-30

This record covers its dated source batch. Earlier stages below are historical;
consult the final disposition before reopening a finding. Later source edits require
input-scope review before reusing its receipts.

Current handoff: implementation and focused validation complete. Six network
bandwidth/egress tests and two Protobuf schema integration tests passed; the node
library type-check also passed. Compiler warnings remain in the retained logs.
No validation job is active. This is a source follow-up to the October 29 frozen
candidate, not a new qualified release. Frozen downloads remain unchanged.

## Decisions

Keep bounded RPC/work/HTTP/WebSocket handling, navigation restrictions, connection
admission and cancellation cleanup from the hardening changes. These limits do
not change transaction signatures, economic rules or consensus validity.

Protect checkpoint agreement plus checkpoint requests/discovery as control
traffic. Certificate, snapshot and witness body transfers remain bulk. Classify
canonical local Protobuf envelope headers using existing Protobuf decoding
primitives; do not allocate and decode payloads for every recipient. The existing
byte-based broadcaster is retained. Noncanonical envelopes fall back to bulk;
this hint does not authenticate messages. Recovery permission checks still fully
decode and enforce the existing allowlist.

Control frames wait for bandwidth within the existing 30-second writer deadline
instead of disappearing after successful enqueue. Queues remain bounded and full
queues still reject submissions. Socket failure, cancellation or deadline expiry
can still lose queued traffic; transport acceptance is not delivery confirmation.
The existing protocol retries remain necessary. Bulk overload drops are retained.
This does not claim guaranteed fairness between competing outbound sockets.

Ingress IP buckets are local abuse protection before ledger validation. Each peer
bucket receives weight 1, local and retained recovery origins weight 4; work orders
blocks within each origin. IPv4-mapped addresses normalize to IPv4; IPv6 uses /64.
Shared-IP users share capacity, while multiple network origins can obtain more
turns. IP is not a person, voting identity or Sybil-proof entitlement. Connection
limits and ingress caps bound resource use, not economic rights. The downstream
balance-based election scheduler and all signature/validity checks remain intact.
The policy is retained explicitly rather than described as a semantics-preserving
speed optimization. No global end-to-end PoW ordering is promised under overload.

## Change-impact and validation plan

| Changed dependency | Requirement/check | Existing artifact | Reuse or rerun / reason |
| --- | --- | --- | --- |
| Network bandwidth and egress | Control pacing, ordering, bounded queue and admission lifetime | `network-receipt.json`: six selected tests passed | Reuse: unchanged network dependencies verified by `network-reuse-review.json`; no repeated run |
| Wire envelope classification and registry | Agreement/control versus snapshot bulk; recovery allowlist unchanged | `node-check-receipt.json`: type-check passed; `schema-receipt.json`: both integration tests passed | Reuse for this source scope; changing wire tags, classification or registry routing requires impact review |
| Ingress explanation | Accurate model and overload trade-offs | Existing fair queue and priority queue implementation | Documentation review; no ingress algorithm change |
| Frozen packages | Exact artifact installation/signing | October 29 frozen artifacts and receipts | Remain evidence for those artifacts only; no rebuild/re-sign in this follow-up |

Evidence: `target/validation-evidence/agent-change-review-20261030/`.
No broad acceptance rerun is scheduled. Release qualification for changed binaries
remains separate from these focused development checks.

Network check: `network-receipt.json`, six tests passed, exit 0. Changes to the
network implementation or its dependency inputs invalidate this focused result.

The original network receipt conservatively inventoried consumer node sources too.
Subsequent classifier API visibility and schema-test edits affect only that consumer.
`network-reuse-review.json` verifies 499 remaining recorded inputs and retained logs
using the existing receipt inventory helper; no network dependency changed.

Schema check: `schema-receipt.json`, both tests passed, exit 0. Independent protoc
encoding exercised every wire message variant, including agreement traffic and
snapshot/witness bulk bodies. Malformed/truncated header fallback also passed.
The existing node-unit roundtrip helper now asserts classifications as well; that
full node-library suite was not compiled or executed locally. The separate schema
integration executable provides the classification runtime evidence for this change.

Next action: none for these focused corrections. Before distributing a new binary,
review affected network acceptance and package qualification against the selected
candidate; these development passes do not qualify newly built release packages.
