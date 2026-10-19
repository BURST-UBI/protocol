# Node transport

Decision 23.3 selects Protobuf for messages between nodes. Decision 8.2 selects
Bincode for signed transactions. Wire generation 33 preserves that separation.

A TCP frame contains a four-byte big-endian body length followed by one
`burst.wire.v33.Envelope` from [wire.proto](wire.proto). Bodies larger than
65,536 bytes are rejected before allocation by the peer reader and before
decoding by `WireMessage`. The envelope and handshake both carry the wire
generation. Nodes reject older generations; there is no fallback decoder.

Generation 31 widens snapshot offsets and total lengths to `uint64`. Generation
30 used `uint32` for these fields. All participating nodes must be upgraded;
the envelope-version check rejects mixed generations before messages are used.
Signed blocks and the snapshot archive encoding are unchanged. The wider wire
fields do not remove the current local archive admission limits.

Outbound TCP connection and handshake work share a 15-second attempt deadline,
including partial frame bodies and writes. Bootstrap, discovery and cached peers
share one per-endpoint retry schedule. Failed attempts wait 2, 4, 8, 16, 32, then
60 seconds; successful connections reset the delay. The failure table holds at
most 4,096 endpoints and forgets failures after five minutes without an attempt.
When full, it defers new endpoints until entries expire. These are local resource
policies, not peer authority or consensus rules. Inbound per-IP cookie limits
remain independent; outbound retries cannot bypass them.

Generation 33 requires mutual authentication before peer registration or message
dispatch. The listener sends its context and a fresh cookie without a signature.
The dialer replies with its own context, a fresh cookie and a dialer proof. After
consuming its pending cookie and verifying that proof, the listener sends its
original context with no cookie and a listener proof. The dialer checks that the
context did not change and verifies the listener proof. Old one-way handshakes
are not accepted. Both peers must use the same wire and block versions and network;
self-identities and invalid public-key addresses are rejected.

Each Ed25519 proof signs `BURST/peer-handshake/v1\0`, a role byte (1 for dialer,
2 for listener), then the listener and dialer contexts in that order. Each context
contains the decoded 32-byte node public key, 32-byte cookie, one-byte network
(Live=1, Test=2, Dev=3), 32-byte parameter hash, one-byte block version, big-endian
`u16` wire version and big-endian `u16` peering port. Signatures themselves are
excluded. The fixed-width transcript binds both identities, both fresh cookies
and the sender role, independently of Protobuf field ordering. Parameter hashes
are authenticated but need not match, so a lagging node can still recover agreed
governance changes. This does not grant consensus authority or encrypt traffic.

The schema names each message and its fields, including bootstrap requests,
votes, telemetry, checkpoint transfer and governance-content transfer. Network
numbers are 1 for Live, 2 for Test and 3 for Dev. Zero is not a network.
Hashes and cookies have exactly 32 bytes; vote and handshake signatures have
64 bytes. Integer conversions reject overflow rather than truncating it.
Optional fields retain presence, including a missing checkpoint transfer digest.

Checkpoint-sync body tags 3 and 4 carry on-demand snapshot requests and chunks.
They bind an exact checkpoint epoch, byte offset and archive digest. Chunks are
at most 16 KiB. The receiver verifies the completed authority header before
requesting further body chunks, then authenticates the full archive before
publishing a staging file. This does not install state. Update participating
peers together before enabling the optional transfer settings.

Tags 5 and 6 query snapshot availability and return an offer. An offer contains
an epoch, height, total byte count and archive digest. Only a response from the
currently queried peer can start a download. The archive's quorum-authenticated
header must match that offer and the receiver's independent minimum height.
Offers do not change the trust anchor or establish the latest network state.

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
Membership does not replace semantic replay. Older generation-30 peers cannot serve these optional
messages. Receivers retry another connected peer after five seconds without
progress, within the request's two-minute deadline.

A planned recovery can first request a missing hash to discover its account.
The first body's hash must match that request, but the response is only a routing
hint. The receiver then proves membership from the independently authenticated
account head. Hints and ancestry responses share the same byte, block and time
limits across all accounts; an account change does not reset those limits.

Generation 32 adds tag 9 for exact historical heads. A request contains 1–64
distinct, nonzero hashes in ascending byte order, an offset and an optional
transport digest. Tag 8 returns the largest whole-head prefix within the existing
1 MiB raw-body limit. Its `start` is BLAKE2b-256 of
`BURST/witness-heads/v1\0` followed by the concatenated requested hashes.
The receiver authenticates each body against its own trusted cursor before
accepting the prefix, then requests the remaining heads. Cursors may cover
different historical positions of the same account. Chunks remain 16 KiB;
server limits remain 64 chunks and four batch loads per second. Resumed offsets
require the transport digest. Update participating peers together; the handshake
requires the same wire generation.

Recovery reuses proposal-content request/chunk messages for missing signed
proposal bodies. Its temporary pool is separate from live admission. Ready
recovery content is returned to replay and never queues a block or writes the
ledger. During startup, content messages require an active registered service;
ordinary block and vote traffic remains closed. Content counts against the
suffix byte budget before retrieval.

Published blocks and bootstrap block entries contain the existing signed
Bincode state-block bytes. Checkpoint and content chunks contain slices of
their authenticated records. Their signatures, content commitments and replay
checks are unchanged. A valid Protobuf envelope does not authorize its payload.

Unknown Protobuf fields may be skipped within the supported wire generation.
An unknown or absent message body is rejected. Senders emit the known schema;
field numbers must not be reused. Transaction bytes reject trailing data.

The Rust model is checked in at `node/src/wire_message/protobuf/model.rs` so
normal builds do not need a schema compiler. When editing it or the schema,
run the independent compiler check:

```sh
cargo test -p burst-node --test protobuf_schema --locked -- --ignored
```

That check requires `protoc` and is included in CI. It encodes each structured
message through the published schema, decodes it with the node, then compares
the node's re-encoding byte for byte. Node unit tests cover framing, bounds,
fixed-width fields, version rejection, unchanged transaction bytes and random
malformed inputs. The message fuzz target calls the running node's decoder.

The workspace lockfile pins release dependencies. Rebuild all communicating
nodes together for wire generation 30; do not change signed ledger bytes to
upgrade the transport.
