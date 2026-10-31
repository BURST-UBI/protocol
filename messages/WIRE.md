# Node transport

Nodes exchange Protobuf envelopes. Signed blocks inside those envelopes retain
their Bincode encoding. The accepted wire version is `PROTOCOL_VERSION` in
[`types/src/lib.rs`](../types/src/lib.rs), currently 39.

## Framing and versions

A TCP frame contains a four-byte big-endian body length followed by one
`burst.wire.v33.Envelope` from [wire.proto](wire.proto). Bodies larger than
65,536 bytes are rejected before allocation by the peer reader and before
decoding by `WireMessage`. The envelope and handshake both carry the wire
generation. Nodes require an exact version match; there is no fallback decoder.
The `v33` Protobuf package name is a schema namespace, not the accepted wire version.

Snapshot offsets and total lengths use `uint64`. Local archive admission limits
still apply.

## Connection attempts

Outbound TCP connection and handshake work share a 15-second attempt deadline,
including DNS resolution, address fallbacks, partial frame bodies and writes.
Registry lock waits before and after the attempt are outside that deadline.
Each TCP connection attempt has a five-second cap within that shared deadline.
Bootstrap, discovery and cached peers share a retry schedule keyed by the
supplied address string. Failed attempts wait 2, 4, 8, 16, 32, then
60 seconds; successful connections reset the delay. The failure table holds at
most 4,096 address strings and expires each entry five minutes after its last
recorded failure.
When full, it defers new endpoints until expiry or a successful connection frees
an entry. These are local resource
policies, not peer authority or consensus rules. Inbound per-IP cookie limits
remain independent; outbound retries cannot bypass them.

## Peer authentication

Peers require mutual authentication before peer registration or message
dispatch. The listener sends its context and a fresh cookie without a signature.
The dialer replies with its own context, a fresh cookie and a dialer proof. After
consuming its pending cookie and verifying that proof, the listener sends its
original context with no cookie and a listener proof. The dialer checks that the
context did not change and verifies the listener proof. Both peers must use the
same wire and block versions and network; self-identities and invalid public-key addresses are rejected.

Each Ed25519 proof signs `BURST/peer-handshake/v1\0`, a role byte (1 for dialer,
2 for listener), then the listener and dialer contexts in that order. Each context
contains the decoded 32-byte node public key, 32-byte cookie, one-byte network
(Live=1, Test=2, Dev=3), 32-byte parameter hash, one-byte block version, big-endian
`u16` wire version and big-endian `u16` peering port. Signatures themselves are
excluded. The fixed-width transcript binds both identities, both fresh cookies
and the sender role, independently of Protobuf field ordering. Parameter hashes
are authenticated but need not match, so a lagging node can still recover agreed
governance changes. This does not grant consensus authority or encrypt traffic.

## Payload decoding

The schema names each message and its fields, including bootstrap requests,
votes, telemetry, checkpoint transfer and governance-content transfer. Network
numbers are 1 for Live, 2 for Test and 3 for Dev. Zero is not a network.
Hash and cookie fields have exactly 32 bytes; consensus-vote and handshake
signatures have 64 bytes. The logged-only legacy verification-vote payload
retains an arbitrary-length signature byte field. Integer conversions reject
overflow rather than truncating it. Address decoding
checks the `brst_` prefix and a 128-byte length limit; it does not verify the
address checksum or prove ownership.
Optional fields retain presence, including a missing checkpoint transfer digest.

## Transfer limits

Transfer limits apply at different layers:

| Transfer | Encoded chunk | Assembled size limit |
| --- | --- | --- |
| Checkpoint record and bundled history | Up to 16 KiB of data | 64 MiB |
| Agreement payload | Up to 16 KiB of data | 16 MiB |
| Historical witnesses | Up to 16 KiB of data | 1 MiB of block bytes plus framing |
| Snapshot archive | Up to 16 KiB of data | Receiver-configured archive budget |

The Protobuf decoder checks the envelope size, field widths and integer
conversions. It does not call the transfer bounds checkers; receiving services
apply them before using transfer data.

The message bounds checker does not cap total snapshot size. The snapshot
receiver applies its archive budget before accepting the transfer. Transfer
digests identify assembled bytes; they do not establish checkpoint authority.

## Snapshot transfer

Checkpoint-sync body tags 3 and 4 carry on-demand snapshot requests and chunks.
They bind an exact checkpoint epoch, byte offset and archive digest. Chunks are
at most 16 KiB. As soon as enough header bytes are available, the receiver
authenticates the header before continuing the download. It authenticates the
complete archive before publishing the output file without overwriting an
existing destination. This does not install state.

Tags 5 and 6 query snapshot availability and return an offer. An offer contains
an epoch, height, total byte count and archive digest. Only a response from the
currently queried peer can start a download. The archive's quorum-authenticated
header must match the offered epoch and height and meet the receiver's independent
minimum height. The peer-advertised archive digest is checked against the complete
download; header authentication alone does not authenticate that digest.
Offers do not change the trust anchor or establish the latest network state.

## Historical witnesses

Tags 7 and 8 request historical witnesses by starting block hash and return
16 KiB chunks bound to that hash, offset, total length and transport digest.
Each batch contains at most 128 newest-to-oldest blocks and 1 MiB of block bytes.
Its framing is a big-endian `u32` count followed by a big-endian `u32` byte length
and the original Bincode bytes for each block. The count and lengths are checked
before allocating bodies; trailing bytes are rejected. A transport digest is
not membership proof. An account-head cursor must be bound to an authenticated
snapshot or to a checkpoint certificate verified against its trusted parent.
Certificate-bound suffix retrieval also checks the canonical head commitment
against the signed ledger root and stops at each parent account's height.
Membership does not replace semantic replay. Receivers retry another connected
peer after five seconds without progress, within the request's two-minute deadline.

A planned recovery can first request a missing hash to discover its account.
The first body's hash must match that request, but the response is only a routing
hint. The receiver then proves membership from the independently authenticated
account head. Hints and ancestry responses share the same byte, block and time
limits across all accounts; an account change does not reset those limits.

Tag 9 requests exact historical heads. A request contains 1–64
distinct, nonzero hashes in ascending byte order, an offset and an optional
transport digest. Tag 8 returns the largest whole-head prefix within the existing
1 MiB raw-body limit. Its `start` is BLAKE2b-256 of
`BURST/witness-heads/v1\0` followed by the concatenated requested hashes.
The receiver authenticates each body against its own trusted cursor before
accepting the prefix, then requests the remaining heads. Cursors may cover
different historical positions of the same account. Chunks remain 16 KiB;
the witness service allows at most 64 response attempts and four batch-load
attempts per one-second window, shared across peers. A rejected request can
consume an attempt. Resumed offsets require the transport digest.

## Proposal-content recovery

Recovery reuses proposal-content request/chunk messages for missing signed
proposal bodies. Its temporary pool is separate from live admission. Ready
recovery content is returned to replay and never queues a block or writes the
ledger. During startup, content messages require an active registered service;
ordinary block and vote traffic remains closed. Content counts against the
suffix byte budget before retrieval.

Published blocks and bootstrap block entries contain signed Bincode state-block
bytes. Checkpoint and content chunks carry record bytes for the receiver to
authenticate. A valid Protobuf envelope does not authorize its payload.

## Legacy envelopes

The legacy verification-request, verification-vote, governance-proposal and
governance-vote envelopes are decoded and logged by the node. They do not admit
endorsements, verifier votes, proposals or ballots. Governance-content transfer
has a separate registered service; it must not be confused with those envelopes.

An envelope with no recognized body is rejected. Unknown fields alongside a
recognized body may be skipped within the supported wire generation. Field
numbers must not be reused. Published state-block decoding rejects trailing
Bincode data. Bootstrap block entries remain opaque bytes until the bootstrap
receiver processes them.

## Implementation reference

- [Codec](src/wire_message/protobuf.rs) and [message bounds](src/checkpoint_sync.rs).
- [Outbound connection](../node/src/transport/peer_connector.rs),
  [retry state](../node/src/transport/connection_registry/dial_retry.rs), and
  [handshake transcript](../node/src/transport/peer_handshake.rs).
- [Snapshot receiver](../node/src/checkpoints/sync/snapshot.rs).
- [Witness receiver](../node/src/checkpoints/sync/witness.rs),
  [exact-head transfer](../node/src/checkpoints/sync/witness/exact.rs), and
  [authenticated cursors](../node/src/confirmed_replay/snapshot/history.rs).
- [Proposal-content transfer](../node/src/governance_content/transfer.rs).

## Maintaining the schema

The Rust model is checked in at `messages/src/wire_message/protobuf/model.rs` so
normal builds do not need a schema compiler. When editing it or the schema,
run the independent compiler check:

```sh
cargo test -p burst-messages --test protobuf_schema --locked -- --ignored
```

That check requires `protoc` and is included in CI. It encodes structured-message
fixtures through the published schema, decodes them with the shared message
codec, then compares re-encoded bytes. It checks those fixtures, not every possible
field combination. Messages unit tests cover framing, bounds,
fixed-width fields, version rejection, unchanged transaction bytes and random
malformed inputs. The message fuzz target calls the same decoder used by the node; it does not
start a node or exercise live peer connections.

The workspace lockfile pins dependencies. Communicating nodes must use matching
wire and block versions.
