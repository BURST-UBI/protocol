# RPC

The node exposes an action-based HTTP API. Send JSON to `/` on the RPC listener,
which defaults to `http://127.0.0.1:7077`. This is not JSON-RPC 2.0: requests use
`action`, not `method`, and have no request ID.

## Make a request

```bash
curl http://127.0.0.1:7077/ \
  -H 'Content-Type: application/json' \
  -d '{"action":"account_balance","account":"YOUR_ACCOUNT_ADDRESS"}'
```

Replace `YOUR_ACCOUNT_ADDRESS` with a valid BURST address. A successful response
has a `result` object. For example, an account with five BRN and two TRST returns:

```json
{
  "result": {
    "brn_balance": "5000000000000000000",
    "trst_balance": "2000000000000000000"
  }
}
```

An action failure returns an `error` string instead. Check both the HTTP status
and the response body. Authentication, request-size and capacity failures can
reject a request before its action runs.

Amounts are decimal strings in raw units. One BRN or TRST is `10^18` raw.
Use integers such as JavaScript `BigInt`; converting a balance to `Number` can
lose precision. Timestamps are Unix seconds.

## Read account state

| Action | Input | Use |
| --- | --- | --- |
| `account_info` | `account` | Head, balances, verification state, representative and confirmation height. |
| `account_balance` | `account` | Current BRN calculation and recorded TRST balance. |
| `brn_components` | `account` | Eligibility periods, spending, stakes and rate history for a local BRN calculation. |
| `trst_portfolio` | `account` | TRST totals classified by state, including pending outgoing value. |
| `account_history` | `account`, optional `count`, `cursor` | Page through account blocks. |
| `account_pending` | `account`, optional `hash`, `threshold`, `count`, `cursor` | Payments and return credits available to receive. |
| `account_representative` | `account` | The account's selected representative. |
| `verification_status` | `account` | Current verification state and related verification information. |

`account_history.history[].amount` is the block's TRST balance, **not the amount
sent by that block**. Similarly, `block_info.brn_balance` is cumulative BRN
committed by burns and stakes, not the spendable allowance returned by
`account_balance.brn_balance`. The names are retained in the wire format;
applications must distinguish them.

For spendable TRST selection, use the portfolio and origin APIs. A balance total
alone does not tell you which holdings can fund a payment.

## Page through results

List actions that use `PaginationParams` default to 100 entries and clamp `count`
to 1–1000. Pass the returned `cursor` unchanged to the same action with the same
filters. Stop when it is absent. A final cursor can lead to an empty page.
`governance_proposals` has a stricter limit: use `count` of 100 or less; the
committed-state reader rejects larger pages.

History runs from older to newer account heights. The cursors do not hold a
snapshot across requests. If the head changes while history is being read, the
request can fail and should be retried. Missing local block bodies are fetched;
unavailable history returns an error rather than an incomplete successful page.

```json
{"action":"account_history","account":"YOUR_ACCOUNT_ADDRESS","count":25}
```

`account_pending` also accepts one source `hash`. That form returns at most one
entry and no cursor. Its `threshold` is an inclusive minimum amount in raw units.
A pending entry includes `return_credit`; an expiry return credit acknowledges
value already returned, rather than delivering it again.

## Submit a signed block

Create and sign the block locally with `burst-wallet-core`, then submit it using
`process`. Keep private keys out of RPC requests.

The `block` field is a **string containing the serialized block JSON**, or
hex-encoded bincode bytes. It is not a nested JSON object. If `signed-block.json`
contains the exact serialized block produced by your signer:

```bash
python3 - <<'PY' > process-request.json
import json
from pathlib import Path
block = Path('signed-block.json').read_text()
print(json.dumps({'action': 'process', 'block': block}))
PY
curl http://127.0.0.1:7077/ \
  -H 'Content-Type: application/json' \
  --data-binary @process-request.json
```

Wrapping the original text this way preserves large integers inside the signed
block. Do not parse and re-serialize it through JavaScript `Number`.

The result includes `hash`, `accepted` and sometimes `detail`. `accepted: true`
means the node accepted or queued the block; it does not mean consensus has
confirmed it. Query `block_info` and inspect `confirmed`:

```json
{"action":"block_info","hash":"YOUR_BLOCK_HASH"}
```

Use `include_signed_block: true` when you need the full signed block. The returned
`signed_block` is also a JSON string. `blocks_info` accepts a `hashes` array and
returns individual block results or errors.

If a submission times out, retain its signed bytes and hash. Query its outcome
before preparing a replacement. See [pending actions](../wallet/WALLETS.md#offline-actions).

## Work and protocol state

| Action | Use |
| --- | --- |
| `params_hash` | Signing compatibility, payment epoch, block version and work threshold. |
| `protocol_params` | Current parameter values; amounts remain decimal strings. |
| `work_generate` | Generate work for a predecessor hash or an account-bound opening root. |
| `telemetry` | Network, counts, uptime and signing-clock status. |
| `peers`, `peers_detailed` | Peer connections. |
| `representatives`, `representatives_online` | Representative information. |
| `bootstrap_status` | Bootstrap phase and creator-authority information. |
| `checkpoint_sync_status` | Checkpoint synchronization state. |

`work_generate` accepts `hash`, optional `difficulty`, and optional `block_type`.
Use the wallet's work preparation methods to choose the correct root and threshold.
The returned `multiplier` is a threshold ratio, not an estimate of computation time.

The node does not populate `representatives_online` yet. An empty response does
not mean that all representatives are offline.

## Governance, verification and provenance

These queries support the corresponding application screens:

| Area | Actions |
| --- | --- |
| Governance | `governance_proposals`, `governance_proposal_info`, `consti` |
| Verification | `verification_activity`, `verifier_assignments`, `appeal_review` |
| Delegation | `delegation_info`, `delegation_keys` |
| TRST history | `trst_origins`, `trace_origin`, `merger_graph_stats` |
| Recovery | `confirmed_account_slot`, `payment_exclusion` |

Use the typed requests in
[`burst-rpc-messages`](https://github.com/BURST-UBI/protocol/tree/main/rpc/messages/src)
for their fields. Some depend on experimental features or specific checkpoint
state; a listed RPC action is not a promise that its operation is enabled on Live.

## Compatibility and test actions

`send`, `burn` and `receive` reject unsigned requests and direct callers to
`process`. They are not server-side wallet APIs. `wallet_create` generates an
address and public key but discards the private key; it cannot create a usable
wallet. Create wallet keys locally instead. `wallet_info` aliases `account_info`,
and `node_info` aliases `telemetry`.

The faucet and private-key-taking helpers are for disposable Dev/Test fixtures.
They require the faucet option and are rejected on Live. This includes
`wallet_create_full`, `burn_simple`, `send_simple`, `receive_simple`,
`change_rep_simple` and the simple governance/verifier helpers. Do not build a
wallet integration around sending private keys to these endpoints. The test helper
`governance_propose_eviction` ignores its `reason` field and submits no content.

`genesis_endorse` is a separate bootstrap administration action. It requires
`Authorization: Bearer <token>` and is subject to the creator's bootstrap window.
The response's `verified` field reports the target state after endorsement
confirmation; a confirmed endorsement alone need not verify the target.
Similarly, verifier helpers return the requested `opted_in` state, not confirmation.
See [RPC access controls](../node/SERVICES.md#rpc-security-boundaries).

## Batches and limits

A request body can be an array of up to 32 action objects. Each action consumes
rate-limit capacity. Batches are not an atomic ledger transaction. For listener,
work-generation and WebSocket limits, see [RPC and networking](../node/SERVICES.md).

Source: [HTTP routing and response envelopes](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/server.rs),
[request and response types](https://github.com/BURST-UBI/protocol/tree/main/rpc/messages/src),
[handlers](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/handlers.rs).
