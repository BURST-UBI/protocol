# Architecture

BURST separates economic rules, persistent state, peer communication and application
interfaces. The node connects these layers. Wallets construct and sign operations;
the node checks those operations against its ledger and protocol rules.

## Follow a payment through the code

A wallet prepares a block from the account's current head, signs it locally, and
submits it through RPC. The RPC handler hands it to the node's processing path;
it does not hold the user's keys or decide the outcome itself.

The node checks the block's format, signature, work and dependencies, then checks
its economic effect against ledger state. Accepted candidates enter representative
elections. Confirmation is recorded together with the required account-chain and
cross-account dependencies. The WebSocket layer reports updates; RPC lets a
client query state again if an event was missed.

A receive follows the same path as a send. It references the pending payment,
so its account chain depends on the sender's block even though the chains are
stored separately. See [accounts and blocks](../protocol/BLOCKS.md) for the format
and [payments](../wallet/PAYMENTS.md) for the client workflow.

## Find the code you need

Paths below are relative to the protocol repository.

| Responsibility | Source |
| --- | --- |
| Hashes, addresses, timestamps and protocol parameters | `types/` |
| Signing, signature verification and key derivation | `crypto/` |
| BRN accrual and staking | `brn/` |
| TRST holdings, provenance, merging and revocation | `trst/` |
| Humanity verification and challenges | `verification/` |
| Proposals, ballots and delegation | `governance/` |
| Constitutional documents and amendments | `consti/` |
| Signed state blocks and ledger records | `ledger/` |
| Storage interfaces and shared record types | `store/` |
| LMDB databases, transactions and consistent read views | `store_lmdb/` |
| Sockets, peer address admission and bandwidth limits | `network/` |
| Live peer messages, Protobuf encoding and message bounds | `messages/` |
| Block admission, economic transitions, confirmation and recovery | `node/` |
| JSON RPC request and response types | `rpc/messages/` |
| RPC handlers and node-facing service interfaces | `rpc/` |
| Event subscriptions | `websocket/` |
| CLI, process lifecycle and public listener startup | `daemon/` |
| Wallet keys, local signing, node client and offline operations | `wallet_core/` |
| Native HTTP response limits and navigation policy | `http_client/` |
| Browser HTTP and timers | `web_runtime/` |
| Shared collections, clocks, logging and shutdown utilities | `utils/` |
| Test doubles | `nullables/` |

The wallet and node dashboard are separate application repositories. Their native
Rust code lives in `src-tauri/`; their frontend code lives in `src/`.

## Network messages, blocks and RPC are different contracts

[`messages/wire.proto`](https://github.com/BURST-UBI/protocol/blob/main/messages/wire.proto) describes live peer messages.
The message codec checks framing and stateless bounds. Successfully decoding a
message does not establish that its signature, ledger dependencies or proposed
state transition are valid. Those checks belong to node admission and replay.

Signed blocks are ledger objects with their own hashing and encoding rules.
Changing a field name in Rust must not silently change the bytes covered by a
signature or stored in a database.

[`rpc/messages`](https://github.com/BURST-UBI/protocol/blob/main/rpc/messages/src/lib.rs) contains types shared by RPC servers
and clients. It depends on domain types rather than the node runtime or HTTP
server. [`rpc/state.rs`](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/state.rs) exposes the operations and read views
that handlers can use; the node supplies their implementation.

## Shared types and validation

A domain type is not necessarily a validated protocol value. `WalletAddress::new`
checks the prefix only, and its derived deserializer accepts unchecked text.
Use the address codec at untrusted boundaries. A checksum-valid address still
does not prove a signature or ownership.

Amount wrappers store raw u128 values. Their denomination constructors and ordinary
arithmetic operators do not return overflow errors; callers must bound inputs or
use checked arithmetic. `Timestamp` stores Unix seconds, while `Timestamp::now`
reads the local wall clock. Neither operation establishes agreed protocol time.

Hash types distinguish identifiers in APIs. Converting a block hash to a
transaction hash copies its bytes; it does not validate a ledger relationship.
See [cryptographic formats](CRYPTOGRAPHY.md) for hashing, encoding and signing
contracts. Admission must still check each operation against its dependencies
and the current protocol rules.

## Inside the node

Start with [`BurstNode`](https://github.com/BURST-UBI/protocol/blob/main/node/src/node.rs) for construction, worker startup and
shutdown. Follow a specific operation into its owner instead of adding protocol
rules to the lifecycle code.

- `block_processor` and `block_validation` handle block admission and authentication.
- `block_economics` applies economic operations; `payment_transition` coordinates
  payment state changes with persistent storage.
- `transport` owns peer connections, handshakes and message dispatch.
- `checkpoints` owns checkpoint agreement, transfer and installation services.
- `confirmed_replay` authenticates and reconstructs confirmed state. Its
  `retirement` module handles removal of eligible stored bodies and the catalogue
  used to classify them.

A storage read view pins a database transaction. Values that must describe the
same ledger state must come from that view. For example,
[`read_governance_snapshot`](https://github.com/BURST-UBI/protocol/blob/main/store_lmdb/src/governance_snapshot.rs) reconstructs
governance from a caller-owned transaction and keys its cache by that view's
generation. A live engine cache is not a substitute for a consistent database read.

## Storage interface defaults

The `store` traits describe individual record operations, not a shared transaction
across every call. Some defaults intentionally provide only a basic fallback:

| Method | Default behavior |
| --- | --- |
| `AccountStore::accounts_page` | Ignores cursor and limit; returns all accounts. |
| `AccountStore::verified_account_count` | Loads verified accounts and counts them. |
| `BlockStore::put_block_with_account` | Stores bytes without maintaining an account index. |
| `MetaStore::payment_epoch` | Reads epoch and closed marker through separate metadata calls. |
| `TransactionStore::delete_transaction_with_account` | Deletes transaction bytes without maintaining the account index. |
| `GovernanceStore::proposal_hash_page` | Loads and sorts all proposal hashes before selecting a page. |
| `PendingStore::get_pending_page` / `get_pending_page_after` | Loads and sorts the account backlog before selecting a page. |
| `PendingSourceSnapshot::totals` | Materializes outgoing entries before aggregation. |

LMDB supplies indexed and transactional paths where needed. Code requiring bounded
pagination, atomic updates or a consistent multi-key read must choose the concrete
backend operation that provides it. A trait method's name is not evidence of those
guarantees. Read the [interfaces](https://github.com/BURST-UBI/protocol/blob/main/store/src/lib.rs)
and the relevant LMDB implementation together.

## Stored records and validation

Reading a record is not the same as validating its ledger history. The LMDB
account readers have different checks:

| Reader | Checks beyond deserialization |
| --- | --- |
| `get_account`, `get_account_txn`, `all_accounts`, `all_accounts_bounded_txn` | None. |
| `accounts_page_txn` | Stored address matches its database key. |
| `account_entries`, `visit_accounts` | Address matches the key; confirmation height does not exceed block count. |
| `all_verified_accounts` | Those account checks plus verified-state membership for each indexed address. |
| `all_verified_addresses` | Index marker and UTF-8 decoding; no account-row cross-check. |

The verified index includes challenged accounts until their status changes.
Its counter is a stored value, not a fresh recount. Writes must keep the account,
index and counter together. See [account storage](https://github.com/BURST-UBI/protocol/blob/main/store_lmdb/src/account.rs).

Block height lookups also differ. `block_at_height_txn` and `height_of_block_txn`
check agreement between forward and reverse indexes within one snapshot. The
ordinary trait lookups read just their respective index. Neither proves that
stored block bytes are signed correctly or belong to a valid chain. See
[block storage](https://github.com/BURST-UBI/protocol/blob/main/store_lmdb/src/block.rs).

## Shared state

`Shared<T>` wraps `Arc<T>` and uses `Arc::make_mut` for mutable access.
`SharedMap` and `SharedSet` use persistent ordered trees, so clones can share
unchanged nodes. Map values detach individually when mutable access requires it.
These wrappers avoid copying unrelated data; they do not isolate interior
mutation or make a shallow `Clone` implementation into a deep snapshot.

The collection diff methods compare membership or values while skipping shared
subtrees. They are not mutation logs. Pointer equality identifies shared storage;
pointer inequality does not prove that values differ.

## TRST portfolios and storage

A wallet portfolio can use a token vector or persistent indexes. The indexed
representation tracks token IDs, canonical order, active times, origins and
revoked chunks. It avoids copying unrelated holdings during point updates.
Reading vector fields materializes the canonical vector: for t holdings this
requires O(t log t) lookup work and O(t) output space. Mutable vector access
then discards the indexes. Prefer the indexed operations or borrowed iterators
when a complete vector is unnecessary.

Local holding rows use the holder and token ID as their keys. A header and
next-token links preserve canonical order. Inserting a holding therefore does
not require renumbering every later storage key. Compatible indexed portfolios
diff changed tree paths; the vector fallback still scans both portfolios.
Delta emission order differs between these paths and is not token order.

These layers provide different guarantees:

| Layer | What it establishes |
| --- | --- |
| Row decoder | Version, byte bounds, canonical key and owner consistency. |
| Portfolio restoration | Unique holdings, complete linked order, state tags, balance cache and expiry deadline under the supplied period. |
| Combined TRST validation | Checks holdings against graph composition and registered burn ownership. |
| Node admission and settlement | Authenticates the operation and applies it with the required ledger and storage changes. |

A delta visitor emits records; it does not commit a transaction. An error can
follow earlier callback calls. Buffer or stage writes so an incomplete delta
cannot become durable state.

Cached transferable balances also have a time boundary. A snapshot-only read
does not process newly expired holdings. Timestamped balance queries account for
expiry; expired/revoked counter queries classify the current holdings without
reconstructing past ownership. A changed expiry period can reactivate expired
holdings, so newly expired amounts from successive flushes are not a permanent
cumulative total.

Snapshot encoding also has a cost. `save_wallets` returns bytes; it does not write
them to disk. `write_wallets` streams the encoding but still allocates sorting
collections and may materialize indexed portfolios. Both omit the merger graph.
`load_wallets` rebuilds holder indexes with an empty graph; attach the separately
stored graph and validate the combined state before using incremental updates.
The storage caller commits related records together.

See [portfolio representations](https://github.com/BURST-UBI/protocol/blob/main/trst/src/engine/portfolio.rs),
[indexed operations](https://github.com/BURST-UBI/protocol/blob/main/trst/src/engine/holding_rows/indexed.rs)
and [holding storage](https://github.com/BURST-UBI/protocol/blob/main/trst/src/engine/holding_rows.rs).

## Constitution recovery

Constitution snapshot reconciliation compares before, after and current state.
For an amendment-only change, it restores the affected amendment, vote bucket and
conflict record only if their current values still match the recorded after-state.
Independent amendments survive. A document change requires the complete current
engine to match the after-state before restoration.

That helper reconciles local state; it does not authorize reversing a certified
amendment. The caller must establish the rollback's protocol authority. Retained
record iterators preserve all amendment phases, empty vote buckets and conflicts;
being old or terminal is not itself permission to discard a record.

See [constitution reconciliation](https://github.com/BURST-UBI/protocol/blob/main/consti/src/engine/snapshot_undo.rs)
and [retained records](https://github.com/BURST-UBI/protocol/blob/main/consti/src/engine/retention.rs).

## Embed a node

Use `BurstNode::start()` when RPC and WebSocket listeners are disabled. It rejects
configurations that request those listeners.

For a node with public listeners, call
`node.start_with_services(burst_daemon::start_public_services)`. The application
factory creates the configured listeners and returns their task handles to the
node. Listener failures are reported through the shared shutdown controller.

Both start methods run until shutdown is requested; they are not readiness
notifications. Call `node.stop().await` when the start future finishes, including
on an error, as the daemon does. Shutdown closes peer admission, drains workers
and connections, and flushes committed storage. `ShutdownController::shutdown()`
only requests this work; it does not wait for cleanup. The controller preserves
the first fatal worker failure for the caller to report after cleanup.

The `burst-daemon` library can be used with default features disabled when only
listener composition is needed. Its CLI binary requires the `node-runtime` feature.
See [`daemon/src/lib.rs`](https://github.com/BURST-UBI/protocol/blob/main/daemon/src/lib.rs) for listener startup and
[`daemon/src/main.rs`](https://github.com/BURST-UBI/protocol/blob/main/daemon/src/main.rs) for the process lifecycle.

## Test boundaries

`node/tests` separates component checks (`components/`), running-node scenarios
(`network/`), restart and recovery gates (`recovery/`), and serialization properties
(`serialization/`). Shared helpers and recorded inputs stay in `support/` and
`fixtures/`. Cargo declares the existing test target names explicitly; moving
files does not drop targets. Component tests now have topic module prefixes, so
exact test filters must include the module (for example, `storage::`).

Node integration fixtures share lifecycle helpers in
`node/tests/support/running_node` and port checks in `node/tests/support/ports`.
Each scenario defines its own readiness and confirmation conditions. A successful
port probe does not reserve that port for a later bind.

Tests for private recovery invariants live beside the implementation. Run the
smallest relevant check; a storage unit test, a signed-block regression and a
multi-node recovery scenario establish different things. Follow the repository's
[validation workflow](https://github.com/BURST-UBI/protocol/blob/main/spec/VALIDATION_WORKFLOW.md) for resource limits and reuse
of existing evidence.

## Mint lookup indexes

The LMDB origin and expiry indexes record mint identities. An origin key is a burn
hash, not a wallet address; sending or merging the minted value does not turn
these rows into an inventory of current holders. Checkpoint installation rebuilds
them from authenticated mint history, including mints no longer held directly.
Current holdings and their ancestry are tracked separately in economic state.

The expiry task removes at most 1,024 old expiry lookup rows per tick. That cleanup
does not revoke value, move pending payments or erase ledger bodies. Portfolio
expiry and pending returns have their own durable state transitions; certified
mode requires an agreed boundary for ownership-changing returns.
