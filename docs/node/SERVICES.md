# RPC and networking

## RPC security boundaries

Bind RPC and WebSocket listeners to loopback and use a private tunnel for remote
access. If you expose a public API, put authentication and traffic controls in
front of it; the limits below do not make it safe to expose an administration node. `genesis_endorse` requires an HTTP
`Authorization: Bearer <token>` header. Configure `BURST_RPC_ADMIN_TOKEN` with a
securely generated 64-character hexadecimal secret (for example, `openssl rand
-hex 32`). Without that environment variable the HTTP administration action is
disabled. Never commit the token or put it in URLs. Nodes holding a genesis seed
refuse non-loopback RPC binds; use a private tunnel for remote administration.

RPC batches contain at most 32 actions and consume rate-limit capacity per action.
At most 32 handlers run concurrently, with a 30-second request deadline. Work
requests have two active slots, a 15-second cooperative computation deadline and
a maximum requested difficulty of 64 times the protocol requirement in expected
work. These are local service limits, not consensus parameter changes. Native
clients reject JSON response bodies exceeding 16 MiB, including chunked bodies.
That bounds the collected byte buffer, not total HTTP or parsed-JSON memory.
The shared reader does not check HTTP status or define a timeout; callers apply
those policies before interpreting the RPC result.
WebSocket sessions are capped at 64 and incoming frames/messages at 64 KiB.
Block-authoring helpers share the same work capacity and deadline. Legacy
`verifier_opt_in` / `verifier_opt_out` server-signing RPCs are available only on
dev/test networks with the faucet enabled. Production wallets use local signing
and submit the signed block through `process`; never send private keys to a node.

## Test faucet

On Dev and Test, an enabled faucet with the network's genesis seed can create
one TRST pending for a recipient by burning one BRN from the genesis account.
The recipient accepts it with a signed receive. This does not verify the wallet;
bootstrap verification uses the separate `genesis_endorse` action. The faucet
is unavailable on Live.

## Client integration

For request formats, pagination and signed block submission, see the
[RPC guide](../development/RPC.md). For subscriptions and reconnect handling, see
[WebSocket events](../development/WEBSOCKET.md).

## Peer admission and traffic budgets

Peer limits count both pending connections and established sockets. They use the
observed transport address, not a peer's advertised address or node identity.
IPv4-mapped IPv6 addresses share the IPv4 allowance. Live defaults permit four
connections per IPv4 address or IPv6 /64, and sixteen per IPv4 /24 or IPv6 /48.
Eight of `max_peers` slots are reserved for outbound connections; this reservation
is clamped to leave at least one inbound slot on small configurations. Permits
remain held until the socket tasks release them, including cancelled handshakes.

Dev/test networks default to allowing a same-address cluster up to `max_peers`,
without an outbound reservation. An explicit `[peer_limits]` table overrides that
policy; omitted fields within the table use the live defaults. For a deployment
with many legitimate nodes behind one NAT, set explicit limits rather than
disabling address accounting:

```toml
[peer_limits]
per_ip = 12
per_subnet = 24
outbound_reserved = 8

[bandwidth_limits]
total_bytes_per_second = 10485760
bulk_bytes_per_second = 8388608
```

Block ingress schedules address buckets fairly, retaining PoW priority within
each bucket. Each network bucket can queue up to 256 blocks; the scheduler reserves roughly one quarter of ingress capacity for other work
(with integer rounding for small capacities). Local submissions and already
retained recovery blocks have separate scheduling lanes. Recovery keeps its
existing shared retention permit; these policies do not bypass block validation.
Ordinary publishes and correlated bootstrap replies use the same network quota.

The P2P egress budget is shared across all peers: by default 10 MiB/s total, of
which bulk traffic can use at most 8 MiB/s. Each connection also has a 5 MiB/s
budget with bulk limited to 4 MiB/s. Buckets allow one second of burst capacity.
Control and bulk messages have separate bounded queues (64 and 256 messages).
Control is sent first between frames; an in-progress frame cannot be interrupted.
Bulk sends that exceed the bandwidth budget are dropped. Control messages wait
for credit within the writer's 30-second deadline. Queue admission can still fail
when its bounded queue is full. These limits cover framed P2P traffic, not RPC,
WebSocket traffic or the small pre-registration handshake exchange.

These are local resource policies, not changes to consensus rules. They reduce
concentration and starvation risks; they do not prevent a distributed attacker
from saturating a service. Public-load and release qualification remain separate.

## Implementation

- [RPC authorization and request bounds](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/security.rs)
- [HTTP admission and deadlines](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/server.rs)
- [Work budget](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/work_budget.rs)
- [Peer admission](https://github.com/BURST-UBI/protocol/blob/main/network/src/admission.rs)
- [Bandwidth accounting](https://github.com/BURST-UBI/protocol/blob/main/network/src/bandwidth.rs)
- [Block ingress scheduling](https://github.com/BURST-UBI/protocol/blob/main/node/src/priority_queue.rs)
- [Socket queues and writes](https://github.com/BURST-UBI/protocol/blob/main/network/src/egress.rs)
- [WebSocket limits](https://github.com/BURST-UBI/protocol/blob/main/websocket/src/server.rs)
