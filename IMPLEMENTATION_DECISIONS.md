# BURST Implementation Decisions

Numbered design decisions and their implementation boundaries. The public reading
path starts in [the documentation](docs/README.md). Requirement IDs are retained
for coverage reconciliation; prose cleanup does not transfer validation sign-offs.

---

## 1. Implementation scope

### 1.1 What is the MVP?

The dependency layers are types and cryptography; BRN, TRST and transactions; storage and the ledger; work and consensus; verification and randomness; governance and the constitution; networking; node orchestration; RPC, wallets and group services. This is a map of implementation concerns, not a release-completion checklist. See [architecture](docs/development/ARCHITECTURE.md).

### 1.2 Should the MVP include networking (P2P), or start as a single-node local prototype?

Isolated engine and single-node tests provide early feedback. They do not replace multi-node validation of consensus, propagation, synchronization and recovery. See [development](docs/development/DEVELOPING.md) and the [validation workflow](spec/VALIDATION_WORKFLOW.md).

### 1.3 Should the MVP include governance, or defer it?

Governance has an implementation and its own network gates. A development sequence that deferred it does not exempt a release from validating the enabled governance paths. See [governance](docs/protocol/GOVERNANCE.md#network-availability).

## 2. Performance and hardware targets

### 2.1 Target confirmation latency

The design target is sub-second confirmation for non-conflicting transactions. Treat this as a target, not an established latency guarantee; a measurement must identify the workload, network, hardware and release artifact.

### 2.2 Target throughput

The design target is at least 100 transactions per second. This document does not establish achieved throughput or equivalence to another network. See [performance](docs/development/PERFORMANCE.md).

### 2.3 Node memory budget

Under 1 GB was the initial memory target, not an established capacity requirement. Further work toward that target is deferred. Size a deployment from measured workload and retained state; do not infer a memory bound from the target.

### 2.4 Minimum hardware requirements

Raspberry Pi 4-class hardware with 4 GB RAM and an SSD is the initial hardware target. It is not a qualification result for a particular node role, ledger size or workload.

### 2.5 Disk requirements

An SSD is recommended. HDD operation was a design allowance, not a throughput guarantee; storage latency affects database and synchronization workloads.

## 3. License and protocol scope

### 3.1 Open source license

The repository uses the [MIT license](LICENSE). The license file contains the governing terms.

### 3.2 Is TRST legally a currency, commodity, security, or utility token?

BURST is designed as a general-purpose protocol, without a built-in sanctions-screening service. The code does not determine the legal classification of TRST. This decision is not a legal conclusion about a deployment.

## 4. Cryptography, keys and addresses

### 4.1 Key generation: from seed or raw?

The wallet supports BIP39 phrase creation and recovery, deterministic seed import and raw private-key import. The mnemonic creation path generates 24 words. See [keys and recovery](docs/wallet/WALLETS.md).

### 4.2 Account key derivation

BURST uses 24 BIP39 words to encode a 32-byte wallet seed. Account keys use Nano's BLAKE2b-256(seed || four-byte big-endian index) construction; the current wallet uses index 0. There is no BIP44 path, registered coin type or BIP39 passphrase expansion. This replaces the former path-string HMAC and changes the account recovered from an old phrase. See [the recovery format](docs/development/CRYPTOGRAPHY.md#recovery-phrases).

### 4.3 Seed entropy

Mnemonic generation uses 256 bits of random entropy to produce 24 BIP39 words. Import delegates phrase validation to the mnemonic library; generation length is not an additional import check.

### 4.4 Key derivation for encrypted keystore

Version-1 keystores use Argon2id with 64 MiB memory, three iterations and one lane, then AES-256-GCM encryption with random salt and nonce. The supported profile is checked when loading. See [wallet encryption](docs/wallet/WALLETS.md#recovery-phrase-and-local-password).

### 4.5 Address encoding

Addresses use the `brst_` prefix, 52 characters encoding a 32-byte public key and eight checksum characters. They use BURST's base32 alphabet, not Base58Check. See [address encoding](docs/development/CRYPTOGRAPHY.md#wallet-addresses).

### 4.6 Address checksum

The checksum is the first five bytes of Blake2b-256 of the public key, encoded in base32 in byte order. Decoding also checks length, alphabet and padding. The checksum detects encoding errors; it does not prove account ownership.

### 4.7 Hash function output size

Hashes use Blake2b configured for a 32-byte output. This differs from truncating a 64-byte Blake2b digest. See [hash formats](docs/development/CRYPTOGRAPHY.md#hashes-and-field-boundaries).

### 4.8 Signature malleability protection

Signature verification uses ed25519-dalek strict verification. Protocol admission must also check the signing bytes and signer authority; a valid signature alone does not authorize an operation.

### 4.9 Hash of which fields constitutes the block hash?

`StateBlock::compute_hash` hashes the canonical block fields and excludes the signature, work nonce and stored hash. The signature covers that hash. Proof of work instead uses the predecessor hash; an opening block uses an account-bound hash with the `BURST/open-work/v2` domain. Work is therefore preparable before the next transaction is chosen. See [the implementation](ledger/src/state_block.rs).
## 5. BRN Engine

### 5.1 Time precision

Accrual uses whole Unix seconds. Rates are integer raw units per second. Agreed protocol deadlines and a wallet's local time estimate serve different purposes; a local clock does not independently certify a state transition.

### 5.2 BRN amount representation

BRN and TRST amounts use `u128` raw units. One BRN or TRST is 10^18 raw; one mBRN or mTRST is 10^15 raw. Floating-point values are not the ledger representation. RPC amount strings must retain this precision.

### 5.3 Initial BRN accrual rate (launch default)

The default is 277,777,777,777,778 raw BRN per second, calculated as `BRN_UNIT / 3600 + 1`. This approximates one BRN per hour, exceeding it by 800 raw per hour because the rate is an integer. Governance can change the rate for subsequent time intervals.

### 5.4 Does BRN accrual start from verification time, or from a global epoch?

Accrual starts at the account's recorded verification time and excludes paused intervals. The shared rate history does not give every account a common accrual start. Resuming eligibility does not backfill a pause.

### 5.5 Can you have outstanding BRN staked AND burn BRN at the same time?

A stake reserves only its amount. The account can burn other spendable BRN: eligible accrual minus burns, locked stakes and returned stakes awaiting acknowledgment. For example, 100 accrued with 20 locked leaves 80 spendable when there are no other deductions.

### 5.6 Is there a maximum BRN balance?

There is no separate policy cap on accumulated BRN. The integer representation still has a finite range, and checked accounting can fail on overflow. No promise of unlimited representable accrual follows from the absence of a cap.

### 5.7 BRN rate change history — how many changes to store per wallet?

Accounts use shared rate history. Each change begins a new interval; it does not retroactively apply the new rate to earlier eligible time. Indexed cumulative accrual avoids scanning every rate segment for ordinary valid-history queries. See [performance](docs/development/PERFORMANCE.md).

### 5.8 BRN staking lock period

A verification or challenge stake remains locked until its outcome is settled. Depending on that outcome it is returned or forfeited. A returned amount can remain unavailable for a new spend until the account acknowledges its release evidence; settlement and spendability are distinct.

### 5.9 Can staked BRN be used for other stakes simultaneously?

Multiple stakes can coexist. Each new stake must fit the current spendable amount after existing locks and unacknowledged releases. The same BRN cannot fund two simultaneous stakes.

### 5.10 Should BRN balance be cached or always computed on-the-fly?

The BRN engine computes balance from eligibility, shared rate history and deductions when queried; no periodic payout transaction is needed. Wallets may estimate displays locally, but their cached inputs do not authorize spending. Nodes also compute balances for admission and RPC responses. See [wallet estimates](docs/wallet/WALLETS.md#balance-displays-and-local-estimates).

### 5.11 BRN balance in RPC responses

Account RPC responses include a computed BRN balance as well as accounting fields. This supersedes the components-only choice. Treat the response as a node report; cached fields alone may omit pause history or pending-release effects. See [account handlers](rpc/src/handlers/accounts.rs) and [balance helpers](rpc/src/handlers/helpers.rs).

### 5.12 BRN overflow protection

Checked BRN accounting rejects arithmetic overflow and invalid deductions. Burn and stake admission use the checked spendable calculation. This is not a claim that every amount API is checked: the convenience available-balance method returns zero on calculation failure, while denomination constructors and ordinary amount operators use plain `u128` arithmetic. Callers must choose the checked API where failure must be explicit.

## 6. TRST Lifecycle & Merger Graph

### 6.1 TRST amount representation

TRST uses `u128` raw units with the same scale as BRN: one TRST is 10^18 raw and one mTRST is 10^15 raw.

### 6.2 Default TRST expiry period (launch default)

The default expiry period is `u64::MAX` seconds, providing no effective expiry on practical timescales. It is an integer parameter, not mathematical infinity. Governance can change it; the age-based reference-value helper treats this default as 100%.

### 6.3 TRST token ID generation

Holdings use `TxHash` identities associated with their creation or derived transitions. A token ID is not always an on-chain transaction hash: change and revocation splits can have derived identities. Keep token identity, provenance origin and account-block hash distinct. See [TrstToken](trst/src/token.rs).

### 6.4 Merger graph storage

Merger-graph records and indexes persist in LMDB. The node also maintains an in-memory graph for economic processing and stages its durable effects with the ledger update. On-disk persistence does not imply graph operations have constant memory or time costs. See [graph persistence](node/src/graph_storage.rs).

### 6.5 When TRST is split, do we create "change" automatically?

Node send processing debits holdings for the chosen origin, retaining any unspent amount, and records consumed provenance in a pending receipt. For a 100-TRST holding and a 30-TRST send, 70 remains with the sender and 30 awaits receipt. The standalone `TrstEngine::transfer` helper instead constructs a recipient token and optional change token using caller-supplied IDs; it does not itself commit a ledger payment. Neither path renews age or creates additional value. See [candidate payment effects](node/src/economic_candidate.rs) and [the TRST engine](trst/src/engine.rs).

### 6.6 Can you send TRST to an unverified wallet?

Receiving TRST does not require verified spending rights. The candidate admission predicate requires those rights for sends, burns and merges, while receive/open paths validate their pending claim separately. `Verified` and `Challenged` accounts retain spending rights; an unresolved challenge alone does not suspend them. Receipt does not grant verification or BRN accrual. See [candidate admission](node/src/economic_candidate.rs) and [wallet states](types/src/state.rs).

### 6.7 Self-sends

A wallet can send TRST to itself. The transfer must still satisfy amount,
ownership, expiry and spending rules; sending to the same address does not
create value or reset provenance and age. A partial transfer creates change
under the ordinary split rules.

Merging has its own operation. A self-send is permitted, but is not a required
substitute for a merge. On the ledger, a send creates a pending payment that
still needs a signed receive.

### 6.8 Can expired TRST be merged or split?

Expired holdings may be consolidated with other expired holdings. A merge rejects revoked or pending inputs, duplicate IDs, different owners and mixtures of live and expired value. Ordinary transfers require live holdings; consolidation does not renew their age.

### 6.9 When governance changes the TRST expiry period, what happens to existing tokens?

Expiry uses effective origin time and the current governed period. A longer period can make an expired holding young enough to transfer again; a shorter period can expire existing value. This does not remove revocation. See [expiry and reference value](docs/introduction/ECONOMICS.md#expiry-and-reference-value).

### 6.10 Minimum transfer amount?

The amount minimum is one raw unit. Zero transfers are rejected. This does not bypass eligibility, holdings, provenance or work checks.

### 6.11 Minimum burn amount?

The amount minimum is one raw BRN. Zero burns are rejected. Spendable-balance and other admission checks still apply.

### 6.12 Maximum tokens per merge operation?

A merge consumes between two and 256 distinct inputs owned by the same wallet. The engine checks the amount sum for overflow; block format also bounds input count and encoded size.

### 6.13 TRST value demurrage curve

The reference value falls linearly with remaining lifetime, expressed in integer basis points. Face amount does not decay. Zero expiry or revoked state yields zero reference value; the practical no-expiry default yields 100%. This is a display calculation, not a market price or promise of purchasing power.

### 6.14 What happens to TRST held by a wallet that gets revoked (the holder, not the originator)?

Account eligibility and origin legitimacy are separate. Revoking a holder removes that account's verified spending rights; it does not make every unrelated origin in its portfolio fraudulent or transfer those holdings to another person. Re-verification operates on the account and its recorded provenance. There is no automatic claimant or ownership reassignment based on an off-chain assertion.

### 6.15 Can revoked TRST be "un-revoked" if the originator wallet is later re-verified?

When a previously revoked account is verified again, the commit path restores contributions associated with that originator in holdings and pending payments. It does not mint replacement copies, remove unrelated revocations or reset expiry. See [restoration](docs/protocol/VERIFICATION.md#re-verification-and-appeals).

### 6.16 What if TRST expires between a send block and the corresponding receive block?

The agreed expiry transition returns pending holdings to the payment sender and creates a return credit. The sender's receive acknowledges that credit without delivering the holdings again. This is distinct from explicit rejection, which creates a new pending receipt to claim. Neither path reverses the BRN burn or renews TRST age. See [pending receipts](docs/wallet/PAYMENTS.md).

### 6.17 TRST origin tracking for merged tokens

Merged holdings reference graph records for their immediate inputs rather than copying every ancestral burn into each token. Input amounts and composition bases preserve the distinctions needed for proportional revocation, including restored chunks and clean remainders. Traversal, indexes and composition records still cost storage and work; a pointer is not an O(1) guarantee for every provenance query.

### 6.18 Proportional split rounding

Proportional revocation rounds the affected amount upward at raw-unit boundaries. Integer allocation and composition records track affected contributions through subsequent splits and merges. Examples that divide exactly do not demonstrate rounding behavior; see the arithmetic in [the merger graph](trst/src/merger_graph.rs).

## 7. TRST Revocation

### 7.1 Revocation scope — which TRST is revoked?

An upheld fraud outcome revokes value originating from the affected account wherever it is currently held, including its contributions in merged holdings and pending payments. Revocation follows origin, not merely current ownership. A protocol judgment is not mathematical proof of a person's identity or misconduct.

### 7.2 Revocation of merged TRST

Revocation separates an affected contribution from the clean remainder. For an exactly divisible 100-TRST holding with 60 from the revoked origin, 60 becomes revoked and 40 remains clean. Fractional raw-unit cuts round upward. The remainder remains subject to its existing age and any other restrictions; this is not deletion of historical payments.

### 7.3 Revocation notification to holders

The desktop wallet displays a revoked-total notice and expandable affected holdings from its node-reported portfolio. The notice polls every ten seconds while mounted and labels retained data when refresh fails. It is an in-app report, not a guaranteed push notification or independent proof of the outcome. See the [notice component](https://github.com/BURST-UBI/wallet/blob/main/src/components/revocation-notice.tsx).

### 7.4 Revocation finality

Revocation is not irreversible in every path. Re-verifying a revoked account restores that originator's affected holdings and pending contributions. Appeals separately target a recorded challenge outcome and have their own settlement and release gates; they are unavailable on Live. Neither path rewrites completed real-world exchanges. See [verification and restoration](docs/protocol/VERIFICATION.md).

## 8. Transaction Architecture

### 8.1 Transaction model: UTXO-like or account-balance?

Accounts have signed chains with post-operation TRST balances and a BRN spending field. BRN availability is computed separately. TRST holdings, provenance and pending receipts also require economic state outside the block. Calling this an account-balance model does not mean an account head alone reconstructs all economic state.

### 8.2 Transaction encoding (on-disk and on-wire)

Stored state blocks use bincode. Peer messages use the wire-message envelope and its protobuf encoding, with published blocks carried as bincode payloads; the legacy codec has its own framing. RPC uses JSON. Signing hashes use explicit canonical bytes rather than serialization of the whole struct. See [wire encoding](messages/src/wire_message/protobuf.rs) and [block hashing](ledger/src/state_block.rs).

### 8.3 Send/Receive: one-phase or two-phase?

A send creates pending TRST; the recipient claims it with a signed receive. Rejection creates another pending receipt for the sender. Agreed expiry returns holdings with a separate acknowledgment credit. See [payments](docs/wallet/PAYMENTS.md).

### 8.4 Burn transaction: does the receiver need to accept?

A BRN burn also creates pending TRST for its recipient. The recipient must claim it; newly minted value does not bypass receiving or provenance checks.

### 8.5 Should transactions support an optional memo/message field?

State blocks have no general payment memo field. Keep application messages outside the payment record.

### 8.6 If memos are supported, are they encrypted?

There is no on-chain payment-memo encryption format because there is no payment memo field. Any separate messaging service needs its own delivery, privacy and authentication design; this decision does not establish that such a service is implemented.

### 8.7 Should blocks have a "subtype" field for richer categorization?

State blocks identify protocol operations with `block_type`. There is no separate application-level subtype for labels such as donation or refund.

## 9. Block Format & Versioning

### 9.1 State block format

`StateBlock` carries account-chain fields, balances, operation references, timestamp, format version, signing grant, payment epoch, parameter hash, merge inputs, release sources and scoped delegation payload, plus signature, work and stored hash. Field interpretation depends on operation type. In particular, `link` is a destination key for Send/Burn and a source hash for Receive; it is not always a preceding token ID. The Rust `brn_spent` field serializes as `brn_balance`. See [the authoritative layout](ledger/src/state_block.rs).

### 9.2 Block version field

Each block has a `u8` format version included in its hash. `validate_format` requires `CURRENT_BLOCK_VERSION`, currently 9. Peer protocol compatibility has a separate version; one is not a substitute for the other.

### 9.3 Transaction extensibility

There is no unrestricted extensions field. Variable fields such as merge sources, release sources and delegation payload have specific meanings, bounds and operation checks. A format change must update its encoding, hashing and validation together.

### 9.4 Signature scheme agility

Block signatures use Ed25519 with no per-block algorithm selector. A new signature scheme would require an explicit compatible protocol design and implementation, not merely a new parameter value.

### 9.5 Maximum block size in bytes?

The base encoded-block allowance is 1,024 bytes. Merge blocks add room for 256 input hashes (8,192 bytes). The format check then adds 32 bytes per release source and the delegation payload length, with independent limits on those fields. P2P fit is checked separately with 40 bytes reserved for a block-carrying envelope. A universal 1 KB maximum would misdescribe accepted blocks.

### 9.6 Maximum pending receives per account

The payment pending-entry path does not impose a fixed per-recipient receipt-count quota. This is not unlimited capacity or a proof that work prevents spam: receipt data and indexes consume storage, and node admission and resource limits still apply. RPC page limits bound a response, not the number of receipts an account may accumulate. See [pending query costs](docs/development/PERFORMANCE.md#pending-payment-queries).

### 9.7 Pending receive timeout

There is no independent refund timer measured from send time. A pending receipt remains subject to claim, rejection and the holding's effective origin age under the governed expiry period. The agreed expiry transition returns holdings and creates acknowledgment credit; it does not reverse a burn. See [payments](docs/wallet/PAYMENTS.md).

### 9.8 Maximum account chain depth

There is no separate policy limit such as one million blocks per account. Account height is a finite integer: ledger updates use checked increment and reject height overflow. Disk capacity, retained economic state and recovery requirements also constrain operation. Pruning does not make chain growth unbounded or establish that all old records may be discarded.

### 9.9 Timestamp representation

Block timestamps are unsigned 64-bit Unix seconds. The type does not represent sub-second time or dates before the Unix epoch.

### 9.10 Endianness for binary serialization

Do not assume one byte order covers every encoding. Bincode storage and payloads use their configured serialization format; protobuf has its own field encoding. Canonical block hashing explicitly writes integer fields in big-endian order. Reconstruct signing bytes from `StateBlock::compute_hash`, not host-native integers or a JSON/bincode dump.

## 10. DAG Ledger

### 10.1 Genesis block

The running node derives its genesis anchor deterministically from the configured creator address and parameter hash at Unix epoch zero. It is an Open block with zero balances, work and signature, trusted as the configured anchor rather than elected by ORV. Initial account state marks the creator verified. Historical fixture genesis helpers have separate encodings and must not be substituted for the configured launch anchor. See [genesis construction](ledger/src/genesis.rs) and [genesis state](node/src/genesis_state.rs).

### 10.2 Ledger pruning: when to prune?

Expiry alone does not authorize deletion of history: provenance, restoration and authority recovery may still depend on it. Physical block-body retirement has configuration and implementation paths, but is disabled by default and rejected by current non-test startup when enabled. Do not describe operator configuration as production availability. See [pruning and retirement](docs/node/NODES.md#pruning-and-retirement).

### 10.3 Epoch blocks for protocol upgrades?

An Epoch marker is a signed operation on an existing account chain, not executable code or an automatic software upgrade. The current authentication path selects the configured genesis signer; structural admission requires the current predecessor, and candidate validation preserves balances, representative and signing-grant continuity. The block must use the supported format and installed payment epoch. A payment-epoch field is distinct from an Epoch operation. See [block admission](node/src/block_processor.rs) and [signing-grant validation](ledger/src/state_block.rs).

### 10.4 Can pruned history be recovered from peers?

Peers can serve retained historical block bodies through ascending pulls. The serving node reads its account-height index and block store; it cannot reconstruct a missing body merely from an index or hash. Responses are capped by block count and message size. Receiving nodes check request/chain binding and pass blocks through admission before advancing from persisted state. Recovery therefore depends on a reachable source retaining the required data; it is not a guarantee that arbitrary pruned history remains available. See [bootstrap](node/src/bootstrap.rs) and [node recovery](docs/node/NODES.md).

### 10.5 Maximum mempool/unchecked blocks size

The running node uses 65,536 shared retained-block slots across queueing, authentication and dependency recovery. Its unchecked map is limited to 61,440 entries, reserving 4,096 slots for ingress. The standalone block processor starts with a 65,536-entry unchecked limit before shared retention is installed. These are entry limits, not a total process-memory bound. Missing-dependency entries have a 15-minute age limit and periodic cleanup. See [retention slots](node/src/block_retention.rs) and [unchecked entries](node/src/unchecked.rs).

### 10.6 Account chain fork handling

Validated competing blocks use one election root for the same account and predecessor, including competing Open blocks. The node attempts to register the fork and its incumbent candidate with ORV; registration is bounded by election capacity and a 16-candidate limit per root. First arrival is not finality. Installing a winning alternative requires transactional branch replacement and dependency/confirmation checks; recognizing a fork does not itself authorize rollback. See [elections](consensus/src/active_elections.rs) and [fork installation](node/src/fork_resolution.rs).

### 10.7 Open block (first block in an account chain)

An account chain starts with an Open block and a zero predecessor. It may claim a pending burn/send, in which case its TRST balance must match the receipt, or open empty with zero TRST. Its BRN spending field is zero. Verification state can exist before this first signed block; opening preserves it rather than restarting eligibility. See [open validation](node/src/block_processor.rs) and [account update](node/src/ledger_updater.rs).

## 11. Snapshots & Fast Sync

### 11.1 Should the node support ledger snapshots for fast syncing new nodes?

Nodes support authenticated state snapshots at a named certified epoch. Export
reconstructs that epoch from retained history or an authenticated installed base
and replayed successors. The epoch height identifies a checkpoint; account chains
have their own heights. Recovery can continue from the snapshot using successors.
A snapshot does not replace private keys or local signing journals.

### 11.2 Snapshot trust model

The receiver starts with independently trusted network and genesis parameters.
Each authority transition must be certified by the preceding trusted electorate.
The resulting epoch's representatives attest the snapshot manifest using its
certified human weights and quorum. The manifest binds the network, genesis,
epoch, roster, quorum, state format, byte length and state hash.

The receiver authenticates the authority chain and certificate, then validates
the body. This is quorum-backed state authentication; it does not independently
replay every earlier transaction or prove that the snapshot is the latest one.
The verification command accepts an independently obtained minimum checkpoint
height and optional expected epoch hash. Installation separately reconciles local
state, history requirements and signing commitments.

### 11.3 Snapshot frequency

Snapshot creation is operator-triggered. Operators choose an epoch, produce
attestations from their local reconstructed state, then collect matching
attestations for export. There is no fixed block-count or daily export schedule.
The offline verification command checks an archive without installing it or
starting networking.

---

## 12. ORV Consensus & Elections

### 12.1 Voting weight for ORV

Each verified human contributes one equal unit, delegated to a representative.
Token balances, expired-TRST reputation and creator status add no weight.
The representative cache stores 10,000 fixed-point units per human; this scaling
does not give anyone additional voting power. Verification supplies the identity
basis, while the cache only accounts for the supplied verified identities.

### 12.2 Confirmation threshold

The certified payment electorate and quorum come from the installed checkpoint.
The election threshold is the electorate's total weight multiplied by the quorum
in basis points, rounded up. The election helper defaults to 6700 basis points
(67%); certified authority accepts 6667 through 10000 basis points.

Checkpoint certificates use the preceding roster and quorum to authorize a
successor. A proposed quorum change cannot lower its own approval threshold.
Governance participation thresholds are separate parameters.

### 12.3 Default representative

The wallet's signing client defaults a new, unopened account to representing
itself. Existing accounts retain their current representative unless the caller
requests a change. The representative is an explicit signed block field; nodes
do not silently assign a representative to a submitted block.

### 12.4 Election lifecycle states

An election starts Passive and becomes Active when it accepts a vote. It ends
Confirmed after reaching the final-vote threshold, or Expired after timeout.
Cementation is subsequent durable ledger processing, not an ElectionState variant.
Soft votes may change; an accepted final vote cannot be replaced in that election.
A soft-vote margin can trigger final voting, but does not itself confirm a block.

### 12.5 Cementing policy

Final-vote quorum confirms the election. The node then hands the target to a
durable pending-confirmation queue and cements its dependencies before advancing
the account's confirmation height. There is no five-minute stability delay.
Missing dependencies or unresolved independent quorum can defer cementation.
Sub-second finality is a performance objective, not a timing guarantee of this
state machine. Rollback rejects blocks at or below confirmation height.

### 12.6 Vote-by-hash optimization

Wire votes reference block hashes rather than carrying complete blocks. The
receiver checks the representative's signature over the canonical vote payload
and accepts one to eight hashes per vote. Candidate blocks and their dependencies
still need separate validation; receiving a vote does not establish their validity.

The standalone VoteByHash helper is not the node's wire encoding. Its signing_data
method has a different layout from the canonical vote_signing_payload used by the
node signer and receiver.

### 12.7 Representative weight recalculation frequency

Certified payment weights come from the installed checkpoint roster. A local
account change cannot immediately rewrite that authority. Each election retains
its electorate; installation of a new certified epoch retires old elections and
candidate routes before using the new weights.

### 12.8 Minimum representative weight to vote

A representative with one delegated human can contribute to certified consensus.
There is no minimum percentage of the electorate for that vote to count. Zero
weight does not count, and confirmation still requires the full quorum.

### 12.9 What counts as "online weight" for the quorum denominator?

For certified payments, the denominator is the installed electorate, including
humans delegated to offline representatives. Peer connections and recent voting
activity do not shrink it. The election component retains an online-weight setter
for use without installed authority; that setter cannot override a certified
electorate.

### 12.10 Confirmation height tracking

Each account stores a confirmation height. Cementation advances it contiguously,
checking that the next block is exactly one height above the confirmed prefix.
The transaction also promotes confirmed authority and applicable governance state
and prunes cemented undo records. A target leaves the durable pending queue only
when its own height has been cemented.

### 12.11 Confirmation height processor

The node runs a background cementation task with blocking database work isolated
from async runtime threads. It selects at most 256 pending targets per group,
cements at most 4096 blocks in a transaction and bounds dependency traversal at
131,072 examined entries. Large valid cuts can require several transactions.

Predecessors and cross-account dependencies are processed before their dependents.
An implicit dependency that still needs its own quorum defers the group and aborts
its staged writes. Failed groups can be isolated using the single-target path.
The standalone ConfirmationProcessor walks one account chain; the running node
uses causal_cementation for the shared dependency graph.

### 12.12 Election timeout

The node's repair pass expires unresolved elections after 300,000 milliseconds
(five minutes). The election also rejects incoming votes more than 300 seconds
after creation. Timeout ends that election; it does not confirm a candidate or
release durable final-vote commitments.

### 12.13 Vote replay protection

A wire signature covers the BURST vote domain, voter, final-vote flag, timestamp,
sequence and candidate hashes. The candidate hash commits to its block contents,
including the payment epoch. There is no separate block-hash-pair election ID in
the signed vote payload.

Within an election, zero sequences are ignored and a soft vote can replace an
earlier soft vote only with a higher sequence. A final vote can supersede a soft
vote regardless of sequence, but an accepted final vote cannot be replaced.
Local signing reserves the sequence and account-position commitment in durable
storage before generating the signature. Restart must preserve that record.

### 12.14 Equivocation detection

The election rejects replacement of an accepted final vote. Representatives
also reserve local final-vote commitments durably before signing, so restarting
must not permit a conflicting signature at that account position.

The standalone EquivocationDetector records conflicting hashes and local penalty
expiry times. The node voting path does not use that helper. It neither retains
signed evidence nor distinguishes soft from final votes, so its observations
cannot serve as transferable equivocation proofs. Automatic proof broadcast and
a timed network-wide weight penalty are not implemented by this helper.

### 12.15 Network partition recovery

Certified consensus keeps the installed electorate during a partition. Reachable
voters do not become a smaller replacement electorate. A partition without the
required quorum cannot advance certified state.

After reconnection, ordinary fork resolution can replace a provisional branch
subject to its confirmed-election, dependency and rollback checks. It cannot
roll back cemented blocks or discard certified authority simply because another
partition has more currently online weight.

### 12.16 Minimum online weight for consensus to function

Certified consensus requires a nonempty authenticated electorate and enough
final-vote weight to meet its quorum. It has no 60-million token-weight floor and
cannot reduce the denominator to whichever representatives are reachable.
An empty electorate cannot confirm a block.

### 12.17 What if there's only one representative?

A single representative can reach quorum if it holds enough delegated human
weight. There is no minimum of three representatives. At genesis, the creator
has one human unit and represents that unit; it has no voting bonus.
A partition without the required certified weight cannot advance checkpoints.
Reconnection does not authorize rollback of a certified checkpoint.

---

## 13. Representatives

### 13.1 Can any wallet be a representative, or must they opt in?

Accounts name their representative in signed blocks. There is no separate
representative-registration operation. The certified roster assigns each verified
human's unit to that representative. Receiving delegation does not run a node or
make it sign: the operator needs the representative key and voting enabled.

### 13.2 Representative change cooldown

Changing representative has no dedicated cooldown. The signed operation must
still satisfy normal admission, chain and balance-preservation checks. A local
change does not rewrite the installed certified electorate: the new delegation
must be included in a certified roster and installed before it changes certified
payment weight.

### 13.3 What if your representative goes offline?

An offline representative cannot contribute signatures, but its delegated weight
remains in the certified quorum denominator. There is no automatic fallback to
another representative. Changing delegation requires the normal certified roster
transition; it cannot bypass unavailable quorum. Enough offline weight can
therefore stop progress.

### 13.4 Principal representative threshold

The node does not require 0.1% principal-representative status to generate votes.
Voting is enabled by node configuration, while counted weight comes from the
applicable electorate. A representative with one delegated human can count.

The online-weight sampler has a principal-classification helper, and the
standalone VoteRebroadcaster accepts an absolute minimum relay weight. Neither
helper establishes a principal-only voting rule in the running node. Relay
selection and eligibility for consensus weight are separate concerns.

---

## 14. Proof of Work & Anti-Spam

### 14.1 PoW algorithm

Work uses Blake2b with an eight-byte digest over nonce_le || work_root. The
digest is interpreted as a little-endian u64 and must be greater than or equal
to the configured threshold. This is an integer threshold comparison, not a
leading-zero test. An eight-byte Blake2b digest differs from truncating a longer
digest.

### 14.2 PoW difficulty

The default threshold is 0xfffffff800000000. Test and Dev defaults use
0xfffff00000000000. The governed min_work_difficulty parameter supplies the
current threshold; block admission installs updated parameters. These values
are BURST's configured defaults, not a claim about another network's current
configuration.

### 14.3 PoW priority

The running node schedules admission fairly across transport origins. Local
submissions and retained recovery have separate origins; peer traffic uses
observed IP buckets. Within each origin, higher work values go first, with
arrival order breaking ties. Local and recovery origins receive scheduling
weight four; peer origins receive weight one.

Higher work does not buy global precedence over all origins, bypass capacity
limits or guarantee earlier finality. Election scheduling is a separate stage.

### 14.4 Pre-computed PoW

For a successor block, the work root is the previous block hash. An opening
block uses Blake2b-256 over the domain BURST/open-work/v2 followed by a zero byte
and the account address string. Each account therefore has its own opening root.

The root is known before choosing the rest of the next transaction, so work can
be prepared ahead of time. Cached work must still satisfy the threshold when the
block is admitted; a parameter change can make it insufficient.

### 14.5 Rate limiting per account

There is no daily transaction-count quota. Work is only one admission control;
the node also bounds queues, retained blocks and network work.

The new-wallet spending cap is per transaction, not a count or daily aggregate.
Send and Burn admission apply it to their respective spend amounts. The current
checker uses new_wallet_rate_limit_duration_secs as the age window measured from
verified_at. A zero new_wallet_spending_limit disables that cap. These names do
not imply a transaction-rate counter.

### 14.6 Different difficulty for different block types?

All block kinds use the same configured threshold. WorkThresholds accepts a
block-kind classification but returns its base value for every kind. Receive,
Open and Epoch have no separate work discount.

### 14.7 Work version

StateBlock has a block-format version and a u64 work nonce. There is no separate
work_version field. Work verification requires the current block version and
uses the Blake2b work algorithm described above.

### 14.8 Work server protocol

The JSON RPC action work_generate takes hash as the work root, with optional
difficulty and block_type. It raises a lower requested difficulty to the current
required threshold and applies the RPC work budget before generation. The caller
must supply the correct root; this action does not construct or validate a block.

The response includes the hexadecimal work nonce, difficulty, hash and a
multiplier field. That field is calculated as difficulty divided by the base
threshold; it is not an estimate of relative nonce-search effort.

### 14.9 Account creation spam

An account can open with zero TRST, or receive a matching pending transfer as
part of opening. There is no minimum opening balance. A normal opening block
must satisfy signature, format and work checks; its work root is account-specific.

Work makes repeated openings require computation, but does not prove that account
spam is economically infeasible or that storage growth is bounded. Node admission
budgets bound retained work rather than the total possible account population.

### 14.10 Dust attack protection

Send admission rejects zero amounts and requires sufficient transferable balance
for the named origin. It does not impose a larger dust minimum. Small sends still
require valid work and normal transaction checks, and create pending state until
resolved. Work imposes a computation requirement; it is not a guarantee against
pending-state growth or dust attacks.

### 14.11 PoW stockpiling

A work nonce has no creation timestamp or independent expiry. Previously computed
work remains usable for its root if it satisfies the threshold at admission. The
submitted block must separately satisfy the current block-validation rules.

For successive account blocks, each next root depends on its predecessor hash.
Prepared work cannot be reused across unrelated roots. Bursts still face origin
scheduling and capacity limits; higher work only changes order within an origin.

---

## 15. Verification System

### 15.1 Initial parameter defaults (before governance exists)

ProtocolParams::burst_defaults supplies these initial values. Governance can
change the applicable parameters; Test and Dev have separate defaults.

| Parameter | Initial value |
| --- | --- |
| Required endorsements | 3 |
| Burn per endorsement | 336 BRN |
| Selected verifiers | 7 |
| Legitimate threshold | 9000 basis points (90%) |
| Stake per substantive verifier vote | 500 BRN |
| Maximum revotes | 3 |
| Challenge stake | 1000 BRN |
| Challenge reward | 1% of revoked TRST, capped at 2000 TRST |
| Verification timeout | 7 days |

Six of seven votes is below 90%; a seven-person completed panel needs all seven
Legitimate votes to meet this default. Neither votes remain in the denominator.

### 15.2 Genesis set

Genesis initializes only the configured creator account as verified, with
verified_at at the epoch and no verifier opt-in. Restart preserves existing
identity state rather than re-verifying an adjudicated creator.

Creator bootstrap authority is a persisted phase, ending at the configured
threshold capped at 50 verified humans. Once complete, a later population drop
or threshold increase cannot reopen it. Ordinary endorsement and committee
selection are separate from the creator's bootstrap path.

### 15.3 "Neither" vote penalty

The default penalty threshold is strictly more than 50% Neither in the retained
window of up to 100 assignments. Signed votes and certified timeout defaults both
enter this window. It is a count-based window, not a calendar period, and there
is no minimum sample size: a first assignment recorded as Neither exceeds 50%.
Exactly half does not exceed the default threshold.

Applying the penalty clears the retained history immediately and starts the
configured selection cooldown, seven days by default. Changing the window size
trims older entries when shrinking; enlarging it does not reconstruct discarded
history or undo past penalties.

### 15.4 How does a wallet opt in to be a verifier?

VerifierOptIn and VerifierOptOut are signed on-chain operations. The ledger
records the opt-in timestamp or clears it on opt-out. Confirmed replay determines
whether the account belongs in an eligible pool. The age requirement measures
time since verification, not time since opting in. Opt-in alone does not select
a verifier or waive the other eligibility requirements.

### 15.5 Minimum requirements to be a verifier

Replay eligibility requires verified rights, an on-chain verifier opt-in,
sufficient time since verified_at, the configured minimum available BRN balance,
and completed external TRST activity with a counterparty that still has verified
rights at the ledger cutoff. Defaults require 30 days and 50 BRN. The 500 BRN
stake for a substantive vote is a separate requirement.

The eligible pool is only an input to selection. Request-specific exclusions,
previous panels and penalty cooldowns can remove otherwise eligible candidates.

### 15.6 Minimum requirements to be an endorser

Ordinary endorsement admission requires verified spending rights and enough
available BRN for the endorsement burn. There is no separate minimum verified
age for endorsing. The orchestrator checks the required burn and prevents a
duplicate endorsement by the same account for the same attempt. The standalone
endorsement helper does not itself check verified status or debit BRN; node
admission and economic processing supply those checks.

### 15.7 MVP verification evidence — what's the concrete first method?

Humanity assessment takes place outside protocol code. Applications can arrange
interviews or other evidence review; the protocol records authorized endorsements,
selected-verifier votes and agreed outcomes. VerificationMethod is an application
interface returning a confidence score and evidence text. Its return value does
not itself authorize a protocol state change or prove human uniqueness.

### 15.8 Verification timeout

The default verification window is seven days. A certified committee activation
binds its start, deadline and activation parameters. Deadline validation requires
start plus that activation's verification_timeout_secs to equal the recorded
deadline; changing parameters later does not silently move it.

A local clock reading cannot settle the panel. The node must certify the deadline
completion and install its effects. Existing signed votes must belong to the
activation context and precede its deadline.

### 15.9 Duration of verified status

Verification does not expire on a recurring calendar schedule. Verified rights
continue unless a challenge outcome changes the account's state. Fraud and
inactivity challenges have different consequences; inactivity is not inferred
merely from time since the last verification.

Verification voting deadlines and verifier penalty cooldowns are separate timers.
They do not require every verified wallet to enroll again periodically.

### 15.10 Can you endorse multiple wallets simultaneously?

Current admission has no per-endorser cap on simultaneous target applications.
Each ordinary endorsement requires its own burn and the target's attempt rejects
duplicate endorsers. A frozen selection also prevents further endorsements for
that attempt. These checks do not implement the originally intended limit on
active endorsements across different targets; that limit remains an implementation
gap, with no configured count in ProtocolParams.

### 15.11 Can you be a verifier while your own wallet is being challenged?

A challenged account retains verified rights while its challenge is pending.
The replay eligibility predicate therefore does not exclude it solely for being
challenged. It must still meet opt-in, age, balance and activity requirements,
and selection applies the exclusions for the particular request. This does not
permit a target to decide its own case.

### 15.12 What happens to endorsers if the wallet they endorsed is later revoked?

Endorsement burns are permanent and receive no reward. Current outcome settlement
has no additional endorser stake to slash when the endorsed wallet fails or is
later revoked. Verifier stake settlement is separate. Endorsing does not make
the endorser immune from an independent challenge.

### 15.13 Can a verifier see who endorsed the wallet they're verifying?

Endorsements are public ledger operations. The block identifies the endorser
through its account and the target through its link; verification state retains
the endorser address, burn amount and timestamp. The protocol does not hide these
identities from selected verifiers. An application can omit them from a screen,
but that is not cryptographic anonymity.

### 15.14 What if a verification vote exactly ties?

For an ordinary verification, the completed panel passes only if the Legitimate
share reaches the configured threshold. A 3 Legitimate, 3 Illegitimate, 1 Neither
panel does not pass the 90% default. If revotes remain, the orchestrator retains
prior votes for settlement, excludes the previous panel, clears the current votes
and selection, and increments the attempt count. A new selection is required.
Once revotes are exhausted, a completed panel below threshold fails.

### 15.15 What if ALL verifiers vote Neither?

An all-Neither panel has zero Legitimate support. Ordinary verification requests
another attempt while revotes remain, then fails if the final panel still does
not meet the threshold. Neither carries no stake, but contributes to the rolling
Neither history used for selection penalties.

### 15.16 What if a selected verifier never votes (goes offline)?

At the authenticated deadline, each absent selected verifier receives a synthetic
zero-stake Neither vote. The completed tally therefore includes absent members;
the denominator does not shrink to the people who responded. These defaults use
the normal tally and Neither-penalty logic, but do not fabricate signed vote
bindings or stake receipts.

Deadline completion is prepared on an isolated copy. Its effects become
publishable only through the caller's certified settlement and installation.

### 15.17 What if an endorser's wallet is revoked during an ongoing verification they endorsed?

The standalone endorsement engine can remove an endorser from a verification
state, but the node revocation path does not call that helper. Revocation updates
identity, BRN activity and affected TRST; it does not automatically remove the
account's endorsements from other pending applications or pause those applications
below threshold.

Automatic invalidation was an intended rule, but it is not enforced by the
current integration. It must not be advertised as an active protection.

### 15.18 Circular endorsement prevention

The endorsement path does not search the endorsement graph for cycles. Ordinary
endorsers need verified spending rights, and an already verified target cannot
receive a new application endorsement. Two unverified wallets therefore cannot
bootstrap each other through ordinary endorsements. These admission rules are
not a general proof against collusion or cycles involving later re-verification.
Panel selection excludes the target and its recorded endorsers.

---

## 16. Endorsements

### 16.1 Do endorsements expire?

Endorsement records have no scheduled expiry. The BRN burn remains spent,
but an endorsement is associated with a particular verification attempt rather
than a reusable lifetime credential. Restarting verification from an Unverified
or Deactivated phase clears the old attempt and requires fresh endorsements.
Pending-application invalidation after endorser revocation is not integrated;
see decision 15.17.

### 16.2 Can an endorsement be revoked by the endorser?

There is no endorser-initiated withdrawal operation. The endorsement burn is
not refundable. An eligible account can use the separate challenge procedure
when it disputes a verified identity; a challenge does not undo an endorsement
transaction or refund its burn.

### 16.3 Endorsement ordering

Ordinary verification reaches the endorsement threshold before preparing a
selection. The selection request derives its trigger, eligible pool, exclusions
and parameters from a confirmed ledger cut, then commits to a future beacon round.
A frozen selection rejects further endorsements for that attempt. Merely seeing
enough provisional endorsements does not authorize a node to choose a panel.

### 16.4 Double-endorsement prevention

The attempt's endorser set rejects a second endorsement from the same account.
This is scoped to retained attempt state. A permitted re-verification restart
clears that state, so a previous endorser can endorse the new attempt with another
burn. It is not a lifetime ban on endorsing the same wallet again.

### 16.5 Self-endorsement

Ordinary self-endorsement cannot pass admission: an endorser must have verified
spending rights, while an already verified target is rejected. An unverified
account cannot supply its own qualifying endorsement. The standalone endorsement
helper only checks duplicate membership; it relies on node admission for these
identity constraints.

### 16.6 Endorser-verifier conflict of interest

Selection excludes the target and every endorser recorded for its attempt.
It also excludes previous panels and verifiers in a penalty cooldown. These
filters apply to the pool before the selection request commits to it; they are
not discretionary choices made after seeing the beacon output.

---

## 17. Challenges

### 17.1 Challenge evidence submission

The challenge transaction records the target, stake, reason and optional appealed
outcome reference. It has no evidence-text, attachment or evidence-URL field.
Applications can arrange evidence review separately, but the current transaction
format does not attach that material to the challenge. A challenge's stake and
signature authorize the request; they do not prove the allegation.

### 17.2 Challenge outcome notification

Challenge state and outcomes are available through the node's ledger and
verification query paths. Settlement authority depends on the applicable votes,
certified cut and installed state, not on receiving a notification.

The WebSocket library defines a verification topic and publish_verification
method, but the current node has no caller publishing lifecycle outcomes through
that method. Automatic challenge-result push delivery is therefore not an
implemented integration. The generic broadcast stream can also drop events for
lagging clients; it is not a durable receipt stream.

### 17.3 What happens to pending transactions during a challenge?

A pending challenge preserves the target's verified spending and accrual rights.
It does not freeze Send or Receive solely because the account is challenged.
Normal balance, provenance, signature, work and operation-specific checks still
apply. A certified upheld outcome can later revoke or deactivate the identity and
apply its corresponding economic effects.

### 17.4 Challenge resolution verifiers — same pool or different?

Challenges use the normal eligible verifier pool. Starting a new challenge clears
the prior selected panel and prior exclusion set, so original verification panel
members are not categorically excluded. Selection still excludes the target,
recorded endorsers, current attempt's previous panels and penalized candidates.
There is no separate experienced-verifier pool for ordinary challenges.

### 17.5 Revocation propagation speed

An upheld fraud outcome invalidates affected TRST as part of its authorized
settlement and local state installation. Other nodes must receive and authenticate
the relevant history and certified state before applying the same result.

There is no fixed propagation deadline or special message that makes revocation
instantaneous across the network. Disconnected nodes can remain behind. Local
revocation uses the provenance graph and updates held and pending effects; its
cost depends on affected state rather than a universal constant-time bound.

### 17.6 Challenge spam prevention

Ordinary challenge admission requires verified challenger status and at least the
configured challenge stake, initially 1000 BRN. Economic admission must reserve
the stake. A failed challenge can forfeit it under outcome settlement. This is an
economic deterrent rather than a proof that challenge spam is impossible; work
and ordinary node admission limits also apply.

### 17.7 Concurrent challenges on the same wallet

Only one ordinary challenge can be active for a target. Admission requires the
target's verification phase to be Verified, then changes it to Challenged.
Another ordinary challenge cannot enter while that phase remains active. Appeals
reviewing an earlier judgment use their own receipt-bound path.

### 17.8 Can you challenge the genesis wallet(s)?

Ordinary challenge admission checks the target's verified state without a
creator-address exemption. The creator can be challenged under the same stake,
selection and settlement requirements. Genesis initialization preserves later
identity adjudications on restart; it does not restore immunity. This is separate
from the temporary enrollment authority in the bootstrap phase.

---

## 18. Verifier Reputation

### 18.1 Should verifier accuracy be tracked?

The protocol does not use a cumulative verifier-accuracy score for selection.
It does retain identified votes, attempt contexts and settlement outcomes, and
tracks rolling Neither history for penalties. Verifiers are therefore not
anonymous or history-free. Agreement with a settled outcome determines stake
settlement; it is not independent proof that the judgment was correct.

### 18.2 Does verifier reputation affect future selection?

Selection ranks eligible addresses by their deterministic hash score from the
certified beacon output. It does not weight those scores by reputation, token
balance or past agreement with outcomes. Eligibility still depends on verified
age, BRN availability and external TRST activity, and Neither penalties can
exclude a candidate. Uniform scoring within the committed pool does not remove
those eligibility filters.

### 18.3 Verifier history storage

A signed VerificationVote is an operation on the verifier's account chain. Node
admission validates its selection context, activation and stake before recording
it. Verification state is also persisted in database records for admission and
recovery; the implementation is not restricted to querying raw blocks each time.

Certified deadline completion can create synthetic Neither records for absent
members. Those records have no corresponding verifier-signed block and must stay
distinguishable from signed votes.

---

## 19. VRF & Randomness

### 19.1 Phase 1 drand configuration

The default relay is https://api.drand.sh. Certified selection pins chain hash
8990e7a9aaed2ffed73dbd7092123d6f289930540d7651336225dc172e51b2ce,
the chained BLS scheme, its public key, a 30-second period and genesis timestamp
1595431050. Local replay verifies against these immutable chain rules without
consulting an HTTP relay or the current clock.

Quicknet support in the drand client does not change the chain accepted by
certified selection. A different relay can deliver the pinned chain's output;
it cannot choose a different beacon policy.

### 19.2 Verifier selection algorithm

The committed request fixes the eligible pool and committee size. Selection first
validates its checkpoint certificate and the exact pinned-chain beacon round.
For each address it computes Blake2b-256(random_output || address_string), sorts
scores ascending, and breaks equal-score ties by address string. It takes the
first requested number of addresses.

No reputation weight enters the score. The scoring and sorting step costs
O(P log P) for a pool of P addresses, in addition to request, certificate and
beacon verification. A node cannot substitute a different pool after seeing
the random output.

### 19.3 What if the drand network goes down?

A certified selection waits for its exact committed drand round. Fetch or proof
failure leaves it pending. The node cannot substitute local randomness, the last
available output or a later convenient round. Relay redundancy can improve
availability without changing the requested chain, round or pool.

### 19.4 Drand round mapping

The request uses the first pinned-chain round strictly after its replayed ledger
cutoff. That cut includes the qualifying endorsement state. The request commits
the cutoff, pool, committee size and round before selection uses the output.
Fresh signatures must meet the pre-beacon deadline under the signer's conservative
clock bound; missing clock authority prevents fresh selection signatures.

An expired proposal without a prepared value may be replaced in a later consensus
round by a newly replayed cut, subject to durable round rules. This refreshes the
proposal cache, not votes or locks. Known prepared evidence retains its checkpoint,
request, pool and beacon round and takes precedence over fresh construction.
Timeout alone does not prove that no prepare certificate exists elsewhere.

A certified request remains bound to its original round. Recovery neither creates
another paid endorsement nor counts as a verification revote. Eligibility for a
fresh unagreed proposal comes from its new cut. These recovery rules do not prove
that adversarial message scheduling cannot bias which proposal reaches agreement.

---

## 20. Governance

### 20.1 Initial governance parameters

Initial live-network defaults are defined in ProtocolParams::burst_defaults.
Ordinary governance has one voting window, followed by cooldown and activation.

| Parameter | Initial value |
| --- | --- |
| Paid proposal endorsement | 100 BRN |
| Required proposal endorsements | 10 |
| Proposal duration | 7 days |
| Exploration vote | 14 days |
| Cooldown | 7 days |
| Propagation buffer before counting | 1 hour |
| Ordinary supermajority | 80% |
| Constitutional supermajority | 90% |
| Ordinary and constitutional quorum | 30% |
| Emergency voting duration | 24 hours |
| Emergency quorum and supermajority | 95% each |

Quorum counts Yea, Nay and Abstain participation against eligible voters.
Supermajority counts Yea against Yea plus Nay; abstentions do not enter that
approval denominator. Threshold selection depends on proposal content.

### 20.2 Which parameters are governable at launch?

The GovernableParam registry defines the supported parameter changes, including
economic rules, verification rules, work difficulty and governance thresholds.
It currently contains 40 named fields. A proposal can change only a registered
parameter within its accepted constraints. This does not make every constant,
algorithm, data format or local node setting governable.

### 20.3 Can proposals be withdrawn by the proposer?

Only the proposer can withdraw a proposal, and only while its phase is Proposal.
The engine sets it to Withdrawn. The caller must authenticate the operation and
persist the resulting state; mutating the engine's in-memory map alone is not a
durable write. Withdrawal does not refund paid endorsement burns.

### 20.4 Can you change your vote after casting?

A recorded direct ballot cannot be replaced for the same proposal. Duplicate
secondary-key ballots are rejected per proposal, owner and grant. Delegated
ballots are rechecked against effective authority at counting, so recording one
does not permanently fix its contribution.

Each eligible owner contributes at most one effective vote. Its direct ballot
takes precedence, followed by its key-delegation projection, then ordinary
transitive delegation. There is no second promotion ballot in the active lifecycle.

### 20.5 What happens if quorum isn't met?

A counted voting phase that misses quorum rejects the proposal. Failure to meet
the applicable supermajority also rejects it. The engine preserves the vote
record and does not extend the window, waive quorum or automatically reset the
proposal for another round. Resubmission requires a new proposal and endorsements.

### 20.6 Can multiple proposals be active simultaneously?

Multiple proposals can coexist and progress through their own schedules. The
engine stores them by proposal hash rather than enforcing one global active
proposal. Certified counting and activation establish the order of shared-state
changes. Concurrent constitutional edits can conflict with the document state;
coexistence does not guarantee that every approved edit remains applicable.

### 20.7 Can a delegate re-delegate to someone else?

Ordinary governance delegation is transitive. Resolution applies scope precedence
at every hop and rejects cycles, excessive depth or ineligible intermediates.
Removing a delegation changes the graph used by a later counting cut; it cannot
rewrite an already certified result. A direct ballot by the owner takes precedence
over delegated voting. Key-based delegation has separate grant and revocation
rules and is projected into the same one-vote-per-owner tally.

### 20.8 Delegation scope

Ordinary delegation supports proposal, named parameter, parameter category and
global scopes. Resolution checks them in that order at every hop. Category names
come from the protocol registry and are policy identifiers, not arbitrary UI
labels. Removing a more specific ordinary delegation can expose a broader fallback;
explicit key-delegation revocation has its own fallback-blocking semantics.

### 20.9 Can the same parameter be changed by multiple proposals in quick succession?

A numeric parameter proposal carries the parameter and its new value, without
an expected-old-value precondition. A later valid activation can overwrite an
earlier value. At a shared activation cut, replay sorts due proposals by proposal
hash and applies them in that order against the evolving parameters. Packet
arrival order does not choose the winner. Each activation must still satisfy
content validation and certified activation requirements.

### 20.10 Can governance change the governance rules themselves?

Registered governance parameters can be changed through governance. There is no
universal additional meta-governance threshold. Ordinary changes use the ordinary
rules, while changing the constitutional supermajority requires the current
constitutional supermajority. Changes to either constitutional quorum or
supermajority use the constitutional quorum.

Emergency changes use emergency thresholds. An emergency change to constitutional
supermajority also requires the higher of emergency and constitutional approval;
changes to either constitutional threshold use the higher participation requirement.
A proposed lower threshold does not supply its own approval rule.

### 20.11 Emergency governance

Emergency parameter proposals enter voting directly and skip ordinary proposal
support and cooldown. Defaults are a 24-hour voting window with 95% quorum and
95% approval, subject to the constitutional-threshold combinations in 20.10.

They still wait for the propagation buffer, certified counting and scheduled
activation. They do not authorize an immediate local parameter edit or install
new executable code. A software bug outside governable behavior still requires
a software change.

### 20.12 Governance participation incentives

Governance voting does not pay BRN or TRST, increase accrual, or impose an
abstention penalty. The vote transition checks eligibility and records the
ballot. Proposal costs and verification-vote stakes are separate operations;
they are not rewards or charges for casting a governance vote.

---

## 21. Constitution (Consti)

### 21.1 Maximum amendment text length?

Amendment text is limited to 50,000 Unicode scalar values, counted with Rust's
chars(), rather than 50,000 UTF-8 bytes or visual glyphs. For an operation-based
amendment, the limit covers the sum of added and replacement article text.
Titles are outside that character count but inside the separate 512 KiB encoded
content limit. Content-transfer support remains subject to its release gate;
a valid size does not establish that every network accepts the full-content path.

### 21.2 What is the initial constitution?

Node initialization uses a three-article bootstrap constitution at version zero:
legitimacy and fraud, evidence standards, and participant rights and responsibilities.
The empty ConstiDocument::genesis constructor is a lower-level helper, not the
node's chosen initial document. Article text is stored policy content, not
executable validation or proof that every stated principle is enforced in code.

### 21.3 Amendment format

An amendment carries ordered AddArticle, ModifyArticle and RepealArticle
operations. ModifyArticle replaces the named article's entire text; this is not
a line-oriented patch format. AddArticle supplies a title and text. Repeal marks
the article inactive while retaining its record. The legacy text-only form adds
one article.

Application works on a candidate document. If any operation fails, the original
document remains unchanged. A successful operation list advances the document
version once, not once per operation.

### 21.4 Can amendments reference or modify other amendments?

Operations address article numbers rather than amendment hashes. A later amendment
can replace an existing active article's text or repeal it, including an article
introduced by an earlier amendment. It does not edit the earlier proposal record.
Missing or already repealed targets can cause an activation conflict; approval
does not guarantee that an edit remains applicable after intervening amendments.

### 21.5 Constitution text encoding

Titles and article text use UTF-8 Rust strings. The protocol does not require
Markdown syntax or interpret prose as executable rules. Renderers must handle
that text according to their own presentation and escaping rules.

### 21.6 What about emoji in constitution amendments?

Unicode text, including emoji, is allowed within the content limits. One displayed
emoji can contain several Unicode scalar values and therefore consume more than
one unit of the text-length limit. There is no emoji-specific allowlist.

### 21.7 Amendment language

There is no language restriction or required English translation in amendment
validation. Voters must assess meaning themselves; the content validator checks
supported structure and bounds, not linguistic equivalence or interpretation.

### 21.8 Constitution versioning

The bootstrap document starts at version zero. Each successfully applied amendment
increments a checked u64 version and appends a version-history entry with its
title. All article operations in that amendment share the new version. Overflow
rejects application. Signed content commitments and proposal hashes are separate
from this sequential document version.

---

## 22. Group Trust Layer

### 22.1 Group registration — where does the directory live?

Groups publish off-chain directories listing their membership-service endpoints.
The client imports versioned JSON over HTTP(S); IPFS content needs an HTTP gateway.
There is no on-chain registration operation or configured default directory.
Import validates the listing but does not automatically trust its groups or query
their services. Users choose which groups become inputs to wallet policy.

### 22.2 Group query protocol

The membership client sends GET to the configured endpoint's verify path with
the inspected wallet address as an encoded path segment. It parses valid, score
and optional since fields; the score must be finite and between zero and one.
The since value becomes response metadata.

This is a service assertion, not a consensus certificate. Queries disclose the
inspected address to the group. Group scores do not add voting weight or override
base-protocol validation. Payment inspection checks original creators rather
than only the immediate sender.

### 22.3 Group response caching

The group client queries membership live rather than retaining a membership
response cache. Its HTTP requests specify no-cache and no-store. This asks HTTP
intermediaries not to reuse responses; it cannot prove that the service's own
membership data is current. Saved directory entries and trust preferences are
separate from membership-response caching.

### 22.4 What if a trusted group goes offline?

An explicit protocol-only fallback can handle a service reported as unreachable,
including connection failures and HTTP server errors. It does not turn a negative
membership response, malformed data or an unknown group into approval. Overall
inspection timeouts require review.

Under an any-group policy, a positive response can satisfy the policy despite
another unavailable group. Under an all-groups policy, a negative response rejects
the payment. Users can choose review for outages instead of protocol-only fallback;
protocol validity is still required in either case.

### 22.5 Group trust policy defaults

The desktop wallet's GroupPolicy defaults to protocol-only mode, no selected
groups, protocol-only outage fallback and zero trust-graph hops. Users can change
these preferences. Any-group or all-groups mode requires at least one directly
trusted group; importing a directory alone does not enable group enforcement.
The service registry and preference validation are wallet policy, not consensus.

---

## 23. P2P Networking

### 23.1 P2P library

The node uses Tokio TCP connections with its own BURST framing, handshake and
message dispatch. The outbound connector shares this flow between bootstrap,
peer-cache connection and reachout. It does not use libp2p gossipsub, Kademlia or
QUIC merely because those appeared among the original design options.

### 23.2 Default ports

NetworkId defines P2P ports 7076 for Live, 17076 for Test and 27076 for Dev.
The base NodeConfig defaults to Dev's P2P port; deployment configuration must
select the intended network and port. RPC defaults to 7077 on 127.0.0.1.
WebSocket defaults to port 7078 on 127.0.0.1 and is disabled by default.
These are configurable service ports, not the earlier suggested Nano port set.

### 23.3 Message format (on-wire between nodes)

Peer messages use typed Protobuf envelopes encoded with prost. Embedded blocks
and some signed records retain their own serialization; the block payload is
bincode, not a field-by-field Protobuf StateBlock. TCP frames prefix the encoded
message with a four-byte big-endian length. Protobuf transport encoding is
separate from the canonical bytes used for block and vote signatures.

### 23.4 Maximum peer connections?

NodeConfig defaults max_peers to 50 and allows configuration overrides.
Additional peer-admission limits constrain origins such as IPs and subnets.
The global connection count is therefore not the only admission bound, and
setting it does not guarantee that every connection attempt will be accepted.

### 23.5 Peer scoring and banning

PeerManager provides score penalties and timed bans, but the running node does
not call its penalize method. Automatic score-based punishment for invalid blocks,
invalid votes or spam is therefore not an active node protection.

The helper assigns penalties of 100 points for an invalid block, 50 for an invalid
vote, 25 for spam and 10 for timeout. A score at or below −500 triggers a one-hour
ban; ProtocolViolation triggers the ban immediately. Ban expiry resets the score
to zero. The transport currently awards one point when a block enters the
validation queue, before ledger validation succeeds. That score is not evidence
that a peer supplied valid ledger data.

### 23.6 Bootstrap nodes

Bootstrap peers come from configuration or CLI arguments; the base configuration
has an empty list. The node also reconnects retained peers and can dial addresses
learned through keepalive exchange when reachout is enabled. It does not use a
Kademlia DHT. A configured seed supplies discovery, not trusted ledger authority.

### 23.7 Clock synchronization tolerance

Fresh admission accepts timestamps no more than 60 seconds ahead of or behind
the node's admission time. Older blocks need the historical-admission or finality
exception recognized by the processor; merely labeling traffic as bootstrap does
not supply that authority. These freshness checks are separate from checkpoint
signing's trusted-clock and pre-beacon deadline requirements.

### 23.8 NAT traversal

The node supports UPnP TCP port mapping where a compatible gateway is available.
Automatic mapping is skipped on Dev and when an explicit advertised address is
configured. Operators can instead supply an advertised address and arrange
public reachability or manual port forwarding. UPnP success depends on the
network and does not imply universal NAT traversal. The node does not use libp2p
hole-punching.

### 23.9 Transaction propagation strategy

Accepted blocks and candidate forks are broadcast to a random subset of connected,
non-banned peers. The fanout is the greater of four and the rounded-up square root
of the eligible peer count, capped by available peers. Local rebroadcasts use the
same selection. This is a custom TCP broadcast path, not libp2p Gossipsub.

Authenticated received votes use a separate relay path: acknowledgments are
normalized to vote gossip, deduplicated, then sent to connected peers other than
the source. Queueing is best effort; a successful enqueue is not proof of delivery.

### 23.10 Block propagation priority

Outgoing sockets have separate bounded FIFO queues: 64 control messages and 256
bulk messages. When both are ready, the writer selects control first. It does not
reorder outgoing blocks by proof-of-work difficulty or account balance. Each
socket has its own writer, so a stalled peer does not block every peer's writes.

Block admission has a separate scheduler that balances origins and uses work
priority within an origin. Admission priority must not be described as outgoing
propagation priority.

### 23.11 Handshake protocol

Peers exchange network and version context plus fresh cookie challenges, then
prove control of their node keys with role-specific signatures. The transcript
binds both identities, cookies, network identifiers, parameter hashes, block and
wire versions, and advertised peering ports. The dialer rejects a listener that
changes this context between challenge and proof.

Handshake validation rejects incompatible networks or versions and self-connections.
Proof of a node key does not establish human verification, representative weight
or an authenticated ledger state. A signed advertised port is still the peer's
claim; the signature does not prove reachability.

### 23.12 Keepalive interval

The running node configures a 15-second keepalive interval and polls on a
15-second timer. The standalone peer-manager default is 60 seconds, but the node
overrides it. The first round is immediately eligible. The task also removes
peers after five minutes of inactivity and expires recorded bans.

### 23.13 Peer exchange

Keepalive construction requests up to eight known peer addresses, including the
node's advertised address when available. The node's two message variants both
use random_peers_with_self; they are separate samples, not distinct self-only and
peer-only formats. Received addresses feed peer discovery and optional reachout;
a listing does not authenticate an endpoint or guarantee connectivity.

### 23.14 Message deduplication

The node shares a bounded cache of 65,536 BLAKE2b-256 message digests. Incoming
block gossip is suppressed for one second after admission to this cache;
duplicates do not extend that window. Identical blocks can then retry after
missing dependencies or authority arrive. Blocks already in storage are also
filtered before queueing; this is a storage lookup, not an authentication cache.

Authenticated vote relay normalizes acknowledgments to the same vote encoding
and suppresses repeat relays while the digest remains in the cache. A separate
512-entry cache per connection remembers successful signature checks on exact
signed vote bytes. A cache hit does not skip authority, routing or historical
recovery checks.

Requests, keepalives and telemetry are not globally deduplicated: identical
messages from different peers can be legitimate. Cache eviction and retry
windows mean deduplication is not a promise of exactly-once processing.

### 23.15 Maximum message size on wire

An encoded peer message is limited to 64 KiB (65,536 bytes), excluding its
four-byte TCP length prefix. Handshake framing rejects zero-length and oversized
messages before allocating the payload. Protobuf encoding and decoding also
apply the message bound. Block payloads must leave room for their envelope;
large supported content uses its separately bounded transfer protocol.

### 23.16 Bandwidth throttling per peer

Each socket has an outbound token budget of 5 MiB/s total, with bulk traffic
limited to 4 MiB/s. All sockets also share the node's configurable budget, which
defaults to 10 MiB/s total and 8 MiB/s bulk. Budgets count the four-byte frame
prefix and allow up to one second of accumulated credit.

Bulk frames are dropped when either budget cannot admit them. Control frames
wait for both budgets within the writer's 30-second deadline; timeout or write
failure ends the writer. Both queues reject new messages when full. These are
egress controls, not an assertion that incoming traffic has the same byte limits.

### 23.17 Connection encryption

The P2P transport is plain TCP. The mutual cookie handshake authenticates key
control and connection context; it does not establish TLS or Noise encryption.
Block and vote signatures protect their signed contents, but do not hide traffic,
peer addresses or access patterns. Other HTTP-based services have separate
transport policies.

---

## 24. Storage, LMDB & Indexing

### 24.1 LMDB database layout

The primary LMDB environment opens 32 named databases through heed. The schema
is defined in `store_lmdb/src/environment.rs`; a logical subsystem can use several
tables, and additional records use prefixes inside `meta`.

| Purpose | Named databases |
| --- | --- |
| Accounts and block history | `accounts`, `blocks`, `frontiers`, `block_heights`, `block_height_reverse` |
| Transaction bytes and account lookup | `transactions`, `account_txs` |
| Pending receives and lookup indexes | `pending`, `pending_senders`, `pending_amounts` |
| Merger records and edge ordering | `merger_origins`, `merger_downstream`, `merger_nodes`, `graph_positions` |
| TRST origin and expiry lookup | `trst_origins`, `trst_expiry`, `trst_reverse` |
| BRN state | `brn_wallets`, `brn_meta` |
| Verification records and membership | `endorsements`, `verification_votes`, `challenges`, `verified_accounts` |
| Governance and constitution | `proposals`, `votes`, `delegations`, `constitution` |
| Representative and peer records | `rep_weights`, `online_weights`, `peers` |
| Checkpoint heads and metadata | `checkpoint_heads`, `meta` |

Table names describe storage, not independent sources of authority. Schema checks
run when opening the environment. Recovery input scratch storage uses a separate
environment; it is not part of this 32-table count.

### 24.2 LMDB map size (max database size)

The default database map limit is 1 GiB. Operators can set
`database_map_size_bytes` in node TOML; for example, `4294967296` permits a 4 GiB
map. The value must be a positive whole number of MiB and fit the platform's
address space. Stop the node before changing it and use the same configuration
for offline snapshot operations.

A map limit bounds the database mapping, not resident RAM, scratch storage or a
proven network population. Raising it does not establish that the host can handle
the intended workload.

### 24.3 Should the node maintain secondary indexes?

Secondary indexes support account block history, pending-receive queries, TRST
origin and expiry lookup, graph edge ordering and verified-account membership.

Account block history uses `(account, height)` keys, with big-endian heights for
ordered range scans and a reverse block-hash lookup. The separate `account_txs`
index uses `(account, transaction hash)` keys; its order is not chronological.
Pending records are keyed by destination and source hash, with additional sender
and amount indexes.

Indexes avoid unrelated-table scans but do not make collecting an entire account
history or origin's matches constant time. Query cost still grows with the rows
returned; callers need bounded queries for bounded work.

### 24.4 TRST-by-origin index

The `trst_origins` index is keyed by origin burn hash and token hash, not by
wallet address. Ordinary node writes add entries when a burn mints TRST;
checkpoint installation reconstructs the mint inventory, including mints whose
tokens have since been sent or merged.

This index is not a complete list of current holdings or every descendant of an
origin. The node's provenance and merger state supply those economic relationships.
The storage helper can collect entries under an origin prefix, but that alone
does not implement wallet-wide revocation or authenticate the indexed data.

### 24.5 TRST-by-expiry index

The `trst_expiry` index orders mint lookup rows by big-endian expiry timestamp
and token hash. A reverse mapping locates a token's origin and expiry entries for
deletion. The storage query for entries strictly before a cutoff collects all
matches; it is not a bounded page or a complete inventory of current holdings.

The node's 60-second expiry task separately updates economic state and removes
up to 1,024 expired lookup rows. Lookup cleanup does not delete portfolios, origin
rows or ledger bodies. Held-token expiry uses portfolio deadlines; in certified
mode, the periodic task leaves ownership-changing pending returns to an agreed
boundary. An index cleanup timestamp is not authority to transfer ownership.

### 24.6 Block-by-timestamp range queries

There is no dedicated block-by-timestamp index in the primary LMDB schema.
Blocks are addressed by hash, and account history is indexed by height. A query
for all blocks in a wall-clock interval would require filtering available block
data or maintaining a separate analytics index. Account height is not a global
clock, and body retention limits what historical data remains available.

---

## 25. Node Operations

### 25.1 Graceful shutdown behavior

Shutdown signals workers, removes the UPnP mapping, closes tracked connections
and waits up to five seconds for background tasks. Unfinished tasks are aborted
and awaited; a timeout is reported as an error. After joining connection tasks,
the node forces LMDB synchronization and refreshes metrics.

Accepted transitions persist economic state in their transaction. Shutdown does
not make caches authoritative or guarantee that every queued block finishes.

### 25.2 Node modes

The node does not expose a full/light/archive mode selector. It validates ledger
state locally. Representative signing is a separate configuration choice through
`enable_representative`; voting still requires the appropriate signing identity
and electorate weight.

Historical body retention, snapshot serving and snapshot installation have their
own policies and release gates. Retaining fewer historical bodies does not turn
the process into a light client that trusts another node to validate its account.
The body-retirement policy is disabled by default.

### 25.3 RPC authentication

RPC binds to loopback by default. HTTP admin authentication applies specifically
to `genesis_endorse`, including when it appears in a batch. It requires a matching
Bearer token configured through `BURST_RPC_ADMIN_TOKEN` (64 hexadecimal characters);
without a configured token the action is unauthorized. A node holding the genesis
seed must bind RPC to a numeric loopback address.

The token is not a blanket access gate for other RPC actions. Ordinary calls still
have their own validation and signing requirements. Request admission separately
limits body size to 2 MiB, batches to 32 actions, concurrent HTTP requests to 32,
and response handling to 30 seconds. These limits do not provide authentication.

### 25.4 Telemetry / metrics

When RPC and metrics are enabled, `/metrics` on the RPC listener exposes the
node's Prometheus registry in text format. Metrics are disabled by default; the
route returns 404 when no registry is configured. There is no separate metrics
listener selected by this flag.

The registry contains counters, gauges and histograms for node activity. A
registered metric is not proof that every execution path updates it, and a
healthy endpoint does not establish synchronization or consensus readiness.

### 25.5 Thread pool sizing

The daemon uses Tokio's multithreaded runtime without specifying a worker count.
This is separate from application-level concurrency: block authentication uses
a capped CPU-derived worker budget (25.7), dashboard reads use a
one-slot executor per RPC state, and work generation has its own admission budget.

NodeConfig stores `work_threads` with a default of one, but the running node does
not read it to size a worker pool. It is not an effective tuning control, and the
implementation does not use it to create per-account processing workers.

### 25.6 Async runtime

The daemon uses Tokio for asynchronous work. Its normal entry point uses
`#[tokio::main]`; the Windows service explicitly constructs a multithreaded Tokio
runtime. Runtime concurrency does not imply parallel ledger commits or independent
economic dependencies between accounts.

### 25.7 Block processing concurrency

Authentication and payment preparation run ahead of serialized ledger commits.
The validation queue sizes its blocking-worker budget from available parallelism:
`min(cores, clamp(floor(cores / 2), 2, 8))`, with one core assumed if detection fails.
Account preparation slots and a bounded preparation window limit speculative work.

The admission worker can group eligible, already-ready candidates into one durable
transaction. It still stages changes against shared economic state and serializes
commit; this is not unrestricted parallel mutation of different account chains.
Different accounts can share payment, provenance or governance dependencies.

### 25.8 Maximum concurrent elections

The node constructs ActiveElections and its priority scheduler with a limit of
5,000 active elections. This is a node constant, not a governance parameter or a
NodeConfig setting. New elections beyond capacity are rejected. The bound does
not cover other queues or establish a throughput guarantee.

### 25.9 Logging level default

Tracing is initialized before CLI and file configuration parsing. Its filter
comes from `RUST_LOG`, then `BURST_LOG_LEVEL`, with `info` as the fallback in
both development and release builds.

The stored `log_level` field and an explicit `--log-level` argument do not
reconfigure the installed subscriber. Set the environment filter before startup.

### 25.10 Structured logging format

The tracing subscriber emits human-readable formatted output. Events can carry
structured fields, but the daemon does not select JSON output. The stored
`log_format` field is not read by this initializer; setting it to `json` does
not switch the output format.

### 25.11 Should the node expose a debug RPC?

The RPC dispatcher has no separate debug-build-only surface. Diagnostic actions
such as `node_info`, `peers_detailed`, `bootstrap_status`, `merger_graph_stats` and
`trace_origin` are part of the normal dispatcher. There is no general debug RPC
switch or `debug_assertions` gate protecting them.

Their availability follows the enabled RPC service and each handler's prerequisites.
Do not rely on a release build to hide diagnostic methods or protect an exposed
RPC listener.

### 25.12 Block processing trace

The node emits tracing events and some targeted spans, including vote-receiver
instrumentation. The span constructors in `tracing_spans.rs` have no current
callers; their existence does not instrument the full receive–validate–commit–
broadcast path. The daemon installs a formatting subscriber, not an OpenTelemetry
exporter. Logs can help follow a block hash, but complete distributed pipeline
tracing is not provided by this setup.

---

## 26. Error Handling & Recovery

### 26.1 What happens if LMDB gets corrupted?

LMDB open errors, incompatible schema or network identity, and failed required
recovery checks prevent normal startup. Recovery authenticates the state it needs
before enabling normal admission; a readable database alone does not establish
valid ledger contents.

There is no universal startup scan that proves every byte or cross-table relation
uncorrupted, and no general automatic LMDB repair. The standalone database-check
helper is not called by node startup. Its partial table list and entry counts do
not authenticate ledger state, and missing named tables are ignored by that helper.
Preserve a failing data directory for diagnosis; restoring or rebuilding state
must also respect the signing identity's durable commitments.

### 26.2 What if a node receives a block it can't validate yet (missing predecessor)?

Missing-dependency blocks can wait in the bounded unchecked buffer, indexed by
predecessor or source. Retention is limited to 15 minutes, with removal on a
periodic sweep rather than an exact per-entry timer. Retrying a block still runs
admission against current state; retention is not acceptance or confirmation.

Certified-mode source gaps also have a historical-dependency path. The admission
worker hands them to that recovery service rather than treating every missing
source as an ordinary unchecked entry. Neither path guarantees that a peer still
has the required historical body.

### 26.3 What if the node falls far behind the network (e.g., offline for weeks)?

An offline node can request account frontiers and retained block bodies through
ascending bootstrap and pass the results through admission. This requires peers
that still hold the needed history; an empty response does not prove that the
local ledger is complete.

Authenticated snapshot bootstrap is an explicit startup option, not an automatic
switch based on how long the node was offline. It requires the certified-mode
gate. A matching configured fetch can obtain the archive before admission;
installation authenticates its chain and certificate and reconciles required
history and local commitments. Downloading or verifying an archive alone does not
install it or establish that it is the latest network state.

### 26.4 What if PoW computation fails or takes too long?

RPC work generation uses a process-wide two-slot budget and a 15-second computation
deadline. Busy requests fail immediately. Native workers check cancellation and
the deadline between batches of 4,096 nonce attempts, so stopping is cooperative,
not an exact wall-clock interrupt. The worker retains its slot until it exits.

Dropping the asynchronous RPC work future signals cancellation. Blocking authoring
helpers share the slots and deadline, but use no caller cancellation flag. The
low-level native `generate` method has no deadline; callers needing bounded work
must use `generate_until`. These computation limits are separate from the HTTP
server's 30-second response deadline.

### 26.5 Double-receive prevention

Known block hashes are filtered through stored-block checks and the processor's
recent-block cache; the processor can report Duplicate. This avoids reapplying a
known block, but is not the entire double-receive defense.

Receive claims re-read the destination/source pending entry inside the ledger
write transaction, validate the amount, and delete the entry in that transaction.
A missing pending entry fails the claim. A different block hash cannot receive
the same payment again merely by bypassing hash deduplication. Rollback and replay
must preserve the pending entry and economic changes together.

---

## 27. Bootstrapping & Network Launch

### 27.1 Bootstrapping the very first verified wallets

The creator account begins verified. While the persisted bootstrap phase is
active, an accepted endorsement from that account takes the direct verification
path for its target rather than requiring a selected panel. Ordinary endorsements
from other accounts still follow the normal endorsement and selection rules.

This is not an enrollment veto window. The creator's separate GovernanceVeto
operation concerns approved governance proposals and requires the verified creator
identity and active bootstrap authority. Bootstrap completion disables the special
creator paths; it does not make the creator's ordinary signed blocks invalid.

### 27.2 Bootstrap duration / exit criteria

Creator bootstrap authority ends when the verified count reaches the smaller
of the configured bootstrap threshold and 50. The phase is persisted. Once
complete, falling membership or a later threshold increase cannot reopen it.
Genesis initializes the phase with one verified account; a threshold at or below
that count completes it immediately.

### 27.3 Testnet vs mainnet genesis

Live, Test and Dev use separate configured genesis identities. The running node
constructs its anchor from the selected creator address and genesis parameters;
the block commits the parameter hash at Unix epoch zero. Historical fixture
genesis helpers are not the running node's launch anchors.

Disposable Dev/Test builds have published fixture seeds. A private-testnet build
pins a separate Test public key at build time; it is a different ledger, not an
upgrade of the published-seed network. Live uses its configured public identity
without distributing the creator's private signing seed.

### 27.4 Network identifier

The handshake carries a network identifier, encoded in Protobuf as Live = 1,
Test = 2 and Dev = 3. Connection setup rejects a peer with a different network
identifier, and the authenticated transcript binds that context along with
protocol versions and parameter hashes. This is not a network-ID byte prepended
to every application message.

Wallet addresses share the `brst_` format across networks. An address alone does
not identify its network; clients must select and verify the appropriate node
and genesis context.

### 27.5 Testnet TRST/BRN — accelerated parameters?

Test defaults retain the Live BRN accrual rate and unlimited TRST lifetime;
they do not apply a 100× accrual multiplier or one-hour expiry. They reduce work
difficulty and verifier minimum age, and shorten governance timings: 30-second
proposal, 60-second voting, 15-second cooldown and five-second propagation buffer.
Proposal endorsement count and cost are zero, quorum is one basis point, and
ordinary approval is 51%.

Dev defaults retain Live economic and governance rules with cheaper work. The
daemon selects these Test/Dev parameter sets after loading configuration, for
both startup and offline snapshot commands. Legacy serialized timing fields
remain in the parameter object; their presence does not enable an extra voting
phase.

---

## 28. RPC API

### 28.1 RPC style

Clients POST JSON to `/`, specifying an `action` and its parameters in the same
object. The server returns a `result` value or an `error` string. This is an
action-based JSON interface, not a JSON-RPC 2.0 envelope: it does not require
`jsonrpc`, dispatch on `method`, or echo a request `id`. Health and metrics use
separate HTTP GET routes.

### 28.2 Pagination for list endpoints

List endpoints use endpoint-specific cursors with a common page-size default of
100, clamped to 1–1,000 where PaginationParams is used. Account history decodes a
base64 decimal offset and reads ascending account heights; it is not a hash-based
continuation token. Missing or invalid offset cursors restart at zero.

Governance proposal pages emit a versioned hash cursor. Pending pages emit a
versioned amount/hash cursor, ordered by descending amount then ascending hash.
These handlers still accept legacy offset cursors. Malformed recognized versioned
payloads are rejected; other undecodable inputs can fall back to offset zero.

Clients should return a cursor unchanged to the same endpoint with the same query
filters. Cursors do not pin a ledger snapshot across requests. Concurrent state
changes can affect later pages, and an encoded offset does not by itself make
storage traversal constant time.

### 28.3 Should RPC support batch requests?

The server accepts arrays of one to 32 actions and returns responses in the same
order. Actions execute sequentially. A batch is not an atomic ledger transaction:
earlier actions may complete even if a later action fails. Authorization and
request-budget checks run before dispatch, including checking for a protected
action anywhere in the array.

### 28.4 RPC rate limiting

The node configures a limit of 100 RPC actions per client IP per fixed one-second
window. Each batch consumes one unit per action. The limiter keys clients by the
TCP peer IP, not forwarded headers, and retains at most 4,096 client buckets; new
clients are rejected at capacity until cleanup frees entries. Old buckets are
cleaned periodically. This is not a token bucket or a configurable NodeConfig
rate setting. HTTP concurrency, body-size and handler-specific work budgets apply
separately.

### 28.5 Which RPC methods are "dangerous" and should be restricted?

The dispatcher has no blanket safe/unsafe switch for state-changing methods.
`genesis_endorse` requires the admin Bearer token, and a node holding the genesis
seed must bind RPC to numeric loopback. Other methods enforce their individual
validation, signing and feature requirements; the admin token does not protect
the entire RPC surface. Keep these action-specific rules separate from the
transport bind address and resource limits.

---

## 29. WebSocket API

### 29.1 Subscription model

Clients subscribe at `/ws` to `confirmation`, `account_update`, `governance` or
`verification`. There are no `new_block` or `vote` topics. Subscribe/unsubscribe
acknowledgments use a `type` field; forwarded events contain `topic`, `data` and
`timestamp` directly.

The running node publishes account updates after acceptance and confirmation
events during cementation. Account updates are provisional, not finality. The
confirmation publisher currently receives the block's TRST balance as `amount`;
clients must not interpret it as the transferred amount. Governance and
verification publishers exist but have no node call sites, so subscribing does
not currently deliver those lifecycle events.

### 29.2 Should WebSocket support filtered subscriptions?

The only implemented filter is an optional list of accounts. It matches the
event's `data.account` field exactly; it does not search every party involved in
a transaction. There are no block-type or minimum-amount filters. An omitted
account list matches all events on the topic, while an empty list matches none.
Subscribing again to the same topic replaces its previous filter and forwarder.

### 29.3 WebSocket reconnection

Events are transient broadcast notifications. Reconnecting clients must subscribe
again and query RPC for current state; the server has no replay cursor or durable
event history. Even a connected client can miss events if it falls behind the
bounded broadcast channel. The forwarder logs lag and continues, without sending
a separate gap notification. Event timestamps are not replay sequence numbers.

---

## 30. Block Explorer & Tooling

### 30.1 Is a block explorer in scope?

The node desktop application includes an Explorer page for block-hash and account
lookups, with block details, account history and verification information fetched
through RPC. It is an application view, not a public explorer website served by
the daemon or an independent historical indexing service.

### 30.2 Should the RPC expose enough data for a block explorer?

RPC exposes account information and history, individual and batched block details,
pending receives, representatives, verification status, peers and telemetry.
Provenance endpoints add origin and merger inspection. The node desktop Explorer
uses these interfaces rather than reading LMDB directly.

These queries do not constitute a complete global explorer index. Results depend
on retained or recoverable history, endpoint pagination and the node's current
state. A public explorer needs its own access policy and any additional search
or analytics indexes; WebSocket notifications are not a durable indexing feed.

### 30.3 GraphQL API

The node exposes action-based HTTP JSON RPC and WebSocket subscriptions. It has
no GraphQL schema or GraphQL endpoint. A separate client-facing GraphQL service
would be an additional integration, not a configuration option of the node.

---

## 31. Wallet

### 31.1 Key storage

The desktop wallet can save and load a password-protected JSON keystore containing
its 32-byte Ed25519 secret key. Version 1 derives the encryption key with Argon2id
and encrypts with AES-256-GCM using random salt and nonce. Decryption checks the
version, algorithm names and supported KDF profile.

Loading a keystore stops wallet automation and clears the unlocked wallet before
unlock. Saving it encrypts the current primary private key; it is not a backup
of transaction history, local settings or every device's signing state. Browser
storage and recovery phrases have separate lifecycles; see the wallet guides.

### 31.2 Wallet auto-merge behavior

The desktop wallet exposes a 0–100 merge setting, defaulting to 50. Zero disables
automatic merging; 100 groups all eligible inputs subject to the input bound.
The shared AutoMergeConfig helper separately defaults to 10, so its default is
not the desktop setting.

Grouping sorts active holdings by remaining lifetime and compares each candidate
with the earliest lifetime in its group. The allowed difference is the configured
percentage of the candidate's longer lifetime. Merging takes the earliest expiry;
the setting limits lifetime loss, not transaction fees. Automation submits at most
the first computed group per maintenance call and requires spending eligibility.
The helper suggests groups; normal signed-block validation still applies.

### 31.3 Multiple accounts per wallet?

The current core Wallet contains one primary key pair and one derived account
address. It does not expose a hierarchy of account indices derived from one seed.
Separate delegation or device keys are not automatically separate verified humans.

One-account wallet storage is not itself Sybil prevention: anyone can create more
keys. Verification and duplicate-identity rules determine human status, separately
from how many local wallets or keys a user creates.

### 31.4 Transaction history storage (client side)

The desktop Activity page requests account history from its configured node in
pages of 25. It does not maintain a complete local transaction-history database
or implement a fixed last-N pruning policy for that network history. Available
historical bodies depend on the node and its recovery sources.

Local signed-action journals and completed receipts serve a different purpose:
they preserve submission/recovery information. They are not a substitute for
querying the account chain or a promise to retain every historical transaction.

### 31.5 Pending receive behavior

Desktop automatic acceptance defaults to enabled, with an empty sender allow-list.
Automation runs only with an unlocked signing wallet and serializes its actions
with manual signing and wallet changes. Each maintenance call examines at most
eight pending entries, retaining pagination progress when none are received.

A nonempty allow-list restricts senders. Optional group policy adds provenance
checks and its configured outage behavior. A payment that fails these rules is
held for review, not automatically rejected or returned. Accepting or explicitly
rejecting it still requires a signed protocol operation. Account/session checks
prevent continuing automation for a wallet that has been switched or stopped.

### 31.6 Wallet backup format

Mnemonic creation generates a 24-word BIP39 phrase. BURST's fixed derivation
recreates the account key, not the full wallet state. Balances and eligibility
come from the network; saved signed actions, device bindings and preferences
require their own local state. Raw-key imports do not automatically recover an
original phrase.

The desktop also supports encrypted keystore files. A keystore password protects
that file, not the independently usable recovery phrase. Check the restored public
address before using either recovery method.

### 31.7 Can the same wallet be open on multiple devices simultaneously?

Several devices can read an account, but the supported signing workflow requires
a local binding to its active signing grant. ChangeSigner replaces that grant;
blocks must refer to the expected predecessor grant. The desktop also checks
profile signing access and unresolved saved actions before allowing another
signature.

These mechanisms coordinate wallet use rather than providing remote exclusion
against someone who copied the root key and local state. A copied key is not
a read-only credential. Devices must use the explicit signing handoff and resolve
existing submissions instead of independently extending the same frontier.

### 31.8 Wallet state synchronization

Wallet views query the configured node for account state, balances, history and
verification information. Another device learns confirmed changes through those
network queries; there is no wallet-to-wallet synchronization channel here.

Refreshing ledger state does not synchronize local signed-action journals or
transfer the signing-device binding. A device must separately establish its
signing role and resolve pending local actions before creating conflicting work.

### 31.9 Can transactions be created offline?

The wallet supports prepared offline actions: fetch public account and operation
context, prepare work, review and sign offline, save the signed action, then submit
when connected. The context is node-supplied data, not an independent proof of
balance or authority.

Knowing a frontier alone is insufficient. Signing also binds operation data,
parameters, payment epoch and signing grant. Prepared work must match the block
root and required difficulty. State can change before submission, so offline
signing does not reserve funds or guarantee later acceptance.

### 31.10 Work delegation

The wallet can request work from its configured node through `work_generate`.
It sends the work root, not a private key or necessarily the final block hash.
For an existing account this root is the previous block hash; opening uses the
account-derived root.

PreparedWork checks root equality and verifies the nonce against the required
threshold before applying it. Work computation can therefore be delegated without
delegating account signing. A work response is not evidence that the eventual
transaction is valid or confirmed.

### 31.11 Multi-signature support

State blocks carry one signature; the implemented operation set has no native
M-of-N account authorization policy or multisignature block type. Device grants
and governance delegation do not implement multisignature spending. This describes
the current format, not a commitment to a future extension.

### 31.12 Time-locked transactions

The current payment format has no recipient claim lock such as `lock_until`.
Block timestamps, freshness checks and TRST expiry do not create a programmable
not-before claim condition. A sender cannot obtain a time-locked transfer merely
by choosing a future timestamp.

### 31.13 Conditional transactions (escrow)

There is no native escrow block type, programmable release predicate or hash-lock
payment operation. A pending send awaiting a signed receive is the ordinary
two-phase payment flow, not a general escrow contract. External arrangements do
not gain protocol enforcement from the pending state alone.

---

## 32. Display & UX

### 32.1 Decimal places for display

One BRN or TRST equals 10^18 raw units, stored as unsigned 128-bit amounts.
The desktop wallet formatter displays six decimal places by default, with caller-
selected precision from zero to 18. It truncates fractional digits rather than
rounding; a positive value below display precision is shown with a less-than
indicator. It uses string arithmetic so raw amounts do not lose precision through
JavaScript Number. Display precision does not change ledger precision.

### 32.2 Display units — should there be named sub-units?

The types crate defines BRN and TRST units at 10^18 raw, plus mBRN and mTRST
constants at 10^15 raw. The wallet interface ordinarily labels BRN and TRST; it
does not automatically switch to a family of named sub-units. Raw is the smallest
integer accounting unit, not a separate asset.

### 32.3 Negative balances — how to display "you can't afford this"

The desktop payment form parses the entered amount and compares it with the
available BRN estimate before opening review. An unavailable balance blocks that
flow, and an excessive amount displays an insufficient-BRN error. This is an
early usability check, not spending authorization: refreshed state and node
validation can still reject a previously reviewed amount. Unsigned balances do
not become negative to represent the rejected payment.

### 32.4 TRST age/freshness indicator

The desktop portfolio shows status badges and time remaining, not configurable
75%/25% lifetime bands. Revoked holdings are marked Revoked; server-classified
expired holdings are labeled Virtue Points. Other holdings with less than 30 days
remaining show Expiring Soon, and those with no expiry show No expiry.

The detailed view includes an expiry date and a linear remaining-lifetime pricing
reference. That reference neither reduces token amounts nor sets a market price.
Local clock-based labels can differ temporarily from refreshed node state.

### 32.5 QR code format for addresses

The desktop Receive page renders a QR code containing the plain wallet address.
It does not embed an amount or label. The QR is a convenient encoding of an
address, not proof of its owner, network or verification status.

### 32.6 URI scheme for payment requests

The current desktop wallet and shared wallet core do not implement a `burst:`
payment-request parser or registered payment-link handler. The Receive QR uses
a plain address. The previously proposed URI syntax is not an interoperable
implemented API; clients must not assume amount or label parameters are handled.

### 32.7 On-chain aliases/naming system?

The implemented block types contain no alias-registration operation or on-chain
name registry. Accounts are identified by their cryptographic addresses. A local
label or third-party name, if supplied by another application, does not establish
protocol ownership of a name or replace address verification.

### 32.8 Address book

The desktop wallet does not currently implement a persistent nickname/address
book or contact-management page. Its trusted-sender list is an automation filter,
not a contact directory. The earlier local address-book choice describes an
unimplemented feature rather than current wallet behavior.

---

## 33. Economics, Incentives & Privacy

### 33.1 What if BRN rate is voted to 0?

A zero BRN rate is representable by the governance parameter registry. Activating
a rate change closes the previous rate-history segment and starts a new one; it
does not recompute earlier accrual at the new rate. A zero segment adds no new
BRN. Previously accrued BRN remains subject to the normal spending, stake and
eligibility rules rather than being frozen solely because the rate became zero.

### 33.2 What if TRST expiry is voted to 0?

Zero is representable for `trst_expiry_secs`; the parameter registry does not
impose a one-day minimum. Expiry checks use `now >= effective_origin + duration`
with saturating addition, so duration zero makes value expired at its effective
origin timestamp. Transferability checks then exclude it from active spending.

This does not make the original receiver specially exempt or mean every token
constructor immediately stores an Expired enum value: time-derived checks and
portfolio refresh apply the current expiry rule. Pending ownership changes remain
separate durable transitions.

### 33.3 What if TRST expiry is voted to infinity?

`u64::MAX` represents unlimited TRST lifetime for ordinary timestamps and is the
launch default. Deadline arithmetic saturates at that value; the generic expiry
predicate would still report expiry if `now` itself reached `u64::MAX`.

Unlimited expiry does not disable BRN accrual, revoke identity rules, or prevent
fraud revocation. It changes the time-based expiry rule, not the rest of the
economic model. The token pricing-reference helper treats this duration as full
remaining value, independently of market prices.

### 33.4 Pending TRST and expiry

Outgoing pending TRST remains attributed to its sender but is excluded from
spendable holdings. The portfolio response reports it separately as total_pending;
it is not already part of the recipient's accepted holdings. Its provenance and
effective origin time continue to determine expiry.

Receiving consumes the pending claim. Rejection and expiry returns follow their
own durable transitions; the sender cannot spend a pending transfer again merely
because the recipient has not accepted it. In certified mode, ownership-changing
expiry returns wait for an agreed boundary.

### 33.5 Why would someone run a representative node?

Representative vote generation does not mint a reward or pay a per-vote fee.
Representative weight comes from the applicable verified electorate and delegation,
not from an earned payout balance. Operators may run representatives to support
confirmation and independently check network behavior, but the protocol does not
guarantee compensation for that service. Verification-panel settlement is a
separate role and mechanism.

### 33.6 Why would someone run a full node (non-representative)?

A non-representative node validates its own ledger view, serves configured local
clients and exchanges data with peers. The current protocol has no storage,
uptime or relay payout for running it. Its practical benefits depend on the
operator's need for independent validation and service availability; motivation
is not an enforced protocol property.

### 33.7 Verifier compensation

Verification settlement returns stakes to voters who agree with the settled
outcome and forfeits dissenting stakes. Winning voters with positive stakes
receive equal TRST shares of the forfeited verifier-stake pool, using integer
division. A zero-stake voter receives no share; if no stakes are forfeited, there
is no such reward pool. Integer remainder is not redistributed by this helper.

Governable stake amounts and voting thresholds affect settlement, but this is
not a fixed reward for each review or an arbitrary configurable reward schedule.
Challenger settlement and revocation-backed challenger rewards are separate.
Agreement with the recorded outcome does not prove the voter was factually right.

### 33.8 Endorser motivation

An endorsement permanently burns BRN and creates the recorded support for a
verification attempt. Endorsers receive no protocol reward, referral payment or
extra TRST weight when the target is verified. The implementation establishes
that cost and effect; it cannot guarantee that users will find endorsing others
worthwhile.

### 33.9 Are account balances public?

Account addresses, signed balances and ledger operations are public protocol
data. RPC can expose BRN estimates, TRST balances and portfolio breakdowns without
a signature from the queried account. They are not encrypted account secrets.

A displayed balance still needs its accounting context: active holdings, expired
or revoked value, outgoing pending value and locked BRN are distinct quantities.
An RPC operator's access restrictions do not make replicated ledger data private.

### 33.10 Is the endorsement/verification graph public?

Endorsements and verification votes use identified signed accounts and reference
their targets. The ledger therefore exposes who endorsed or voted on which
identity; it does not use anonymous verifier credentials. Off-chain identity
evidence is separate and need not be embedded in those blocks.

A derived missing-vote Neither outcome is not a signed statement by an absent
verifier. Public outcome records must not be confused with signed vote receipts.

### 33.11 Can a wallet's full transaction history be enumerated by anyone?

Account chains and their links are public, and account-history RPC exposes
ordered blocks when the node can supply them. Public does not mean every node
always retains every body. A missing historical page can require recovery from
another source or return an unavailable-history error. A frontier hash alone
cannot reconstruct missing bodies. There is no account-chain obfuscation layer.

### 33.12 IP address privacy

P2P connections use ordinary TCP and disclose endpoint addresses to peers. The
node does not implement Tor routing or Dandelion-style origin hiding. Signatures
authenticate contents without encrypting the transport.

External network routing can change the address observed by peers, but it does
not make account actions anonymous or remove disclosures to RPC, time, randomness
and group-service endpoints. See the privacy guide for those separate connections.

### 33.13 Automated identity judgments

Human verification and challenges determine identity status through the
endorsement, panel-vote and outcome rules. The node does not classify accounts
as Sybils from shared IP addresses, similar transaction timing or payment patterns.

Network admission can group traffic by observed IP to protect resources. That is
a transport limit, not evidence that the accounts behind an address are the same
person, and it does not itself revoke their verified status.

### 33.14 Circular payments

Payments can return to an earlier holder, including through a sequence such as
A to B to C to A. Admission applies the ordinary ownership, amount, expiry and
pending-receive rules; it does not reject a payment merely because the recipient
held the value earlier. Transfers preserve the relevant origin history.

A circular sequence of holders is different from a cycle in the provenance
graph. Provenance references describe earlier inputs; graph validation rejects
cyclic dependencies. Receiving value again creates a later ledger operation,
not a reference that makes an earlier input depend on its own descendant.

---

## 34. Build, Test & Deploy

### 34.1 Config file format

Node configuration is TOML, loaded into NodeConfig. The daemon combines file
settings with its CLI/environment handling and applies network-specific defaults.
RPC requests and responses use JSON; they are not interchangeable with TOML
configuration. Some stored settings are currently inactive, as noted in the
operation sections, so serialization support alone does not prove a setting works.

### 34.2 Feature flags

Cargo features are defined per crate. The daemon defaults to `node-runtime`,
which enables its optional node dependency and is required by the burst-daemon
binary. `experimental-testnet` forwards the node feature; `private-testnet`
includes that feature and the private Test identity configuration.

The daemon enables the RPC and WebSocket crates' `server` features. Listener
enablement remains a runtime setting. The earlier example flags `groups`,
`metrics` and `debug-rpc` are not a workspace-wide feature interface.

### 34.3 WASM compilation target

Wallet core has target-specific native and wasm32 paths. Native HTTP uses
reqwest; wasm32 uses the browser transport crate. Filesystem journals and native
runtime dependencies are conditional rather than universally available in browser
builds. The browser-check example is a cdylib behind `browser-test`.

This describes source support, not proof that every feature or current release
artifact has passed browser validation. Browser execution and native execution
have different storage, transport and signing-workflow constraints.

### 34.4 CLI interface for the daemon

The Cargo binary is `burst-daemon`, using clap. Run a node with
`burst-daemon [options] node run`. Snapshot commands are `snapshot attest`,
`snapshot export` and `snapshot verify`, each with their own arguments.
`burst-daemon genesis-keygen live --seed-file PATH` generates a creator identity
without opening a ledger or starting networking. The parser does not provide
the previously suggested `start`, `status` or
`wallet create` subcommands. Normal application queries use RPC.

### 34.5 Docker support

The repository contains a Dockerfile for the network NAT test fixture. It does
not currently provide the proposed general-purpose multistage daemon image or
a node-plus-explorer Compose deployment. A test container recipe is not a
production packaging or deployment guarantee.

### 34.6 Systemd integration

`deploy/burst-node.service` is a sample systemd unit for the daemon. It uses
Type=simple, runs under the burst user/group and restarts on failure. Installation
must supply the executable, configuration, user and writable directories.
It does not implement systemd readiness notification: process startup alone is
not evidence that ledger recovery and synchronization are complete.

### 34.7 Test strategy

Tests include crate-local unit tests, integration targets, node network scenarios
and tool tests. The node runner groups library tests into fast, slow and network
sets and supports an exact focused test. Test doubles and in-memory components
are used where appropriate; some scenarios use actual stores, sockets or processes.

Choose validation from changed dependencies and required evidence, not merely
from a test count. Follow `spec/VALIDATION_WORKFLOW.md` for resource limits,
artifact reuse and release qualification. Source tests existing is not evidence
that the current candidate passed them.

### 34.8 CI/CD

GitHub Actions defines scoped CI jobs for tooling, crate tests, node validation,
network scenarios, WebAssembly checks, linting, formatting and platform checks.
Node tests use separate compilation and execution stages with the validation
profile; the general workspace test job excludes the node crate. Release work
has a separate workflow.

Job selection depends on changed inputs. A workflow definition describes intended
checks, not their current result or full release acceptance. Preserve the actual
job artifacts and requirement-specific evidence when reporting a pass.

### 34.9 Multi-node testing

The dev-cluster.sh script launches separate local daemon processes with distinct
ports and data directories, then runs RPC smoke checks. It defaults to three
nodes and a release binary; --no-build reuses that binary. Its build path must
not bypass the host's validation limits or release-build restrictions.

Rust network integration tests also launch node instances with RPC and peer
services inside a test process. These fixtures test specific scenarios; their
presence does not establish a passing test network. Record the executed scenario
and artifact when reporting results.

### 34.10 Testnet faucet

The faucet requires a Dev or Test network, an enabled faucet and the network's
genesis signing seed. It authors a signed Burn block using one BRN from the
genesis account to create one TRST pending for the requested recipient. Normal
admission still checks the block and available funds.

The recipient must sign a receive to accept the payment. The faucet does not
verify the recipient; bootstrap verification uses the separate genesis_endorse
action. The faucet is unavailable on Live.

### 34.11 Property-based testing

Property tests use proptest in BRN accounting, shared types and proof-of-work
validation, including generated rate/pause/release cases. Other subsystems also
have deterministic invariant and regression tests. The repository does not
establish blanket generated-test coverage of every merge, split or serialization
rule merely by depending on proptest. Coverage and passing results must be tied
to the individual property and executable tested.

### 34.12 Fuzzing

The separate cargo-fuzz workspace defines five targets: block deserialization,
message parsing, codec decoding, proof-of-work validation and BRN computation.
The workspace and seed tooling are source infrastructure; they do not imply that
a sustained fuzz campaign has run or that every decoder is covered. Record the
target, inputs, duration and findings when describing fuzz evidence.

### 34.13 Benchmark suite

Criterion suites exist for cryptographic operations, BRN computation and work
generation/validation. The work-generation cases use very low thresholds that
accept almost every hash; they measure overhead rather than realistic search
time at the protocol threshold.

This is not the originally proposed complete Criterion suite for LMDB and block
processing. End-to-end capacity and performance questions need their own scenario
measurements with recorded inputs, profile and host limits. A microbenchmark is
not a network throughput or production-readiness result.

### 34.14 Database compatibility

LMDB stores its schema version in metadata. Opening a database accepts versions
3 through 8 and checks additional layout markers. Supported index rebuilds and
their version markers commit in the same write transaction. Version acceptance
alone does not guarantee that every stored layout is supported.

Unsupported schemas, missing required indexes and incompatible pending-record
layouts cause an error. A nonempty unversioned ledger is not initialized as a
new database. Opening never falls back to deleting the ledger and synchronizing
again; layouts that require a fresh directory must be handled explicitly.

### 34.15 Peer protocol compatibility

The authenticated handshake carries the network, block version and wire version.
A peer must use the same network and exactly match CURRENT_BLOCK_VERSION and
PROTOCOL_VERSION. There is no negotiated range of compatible minor versions.
An incompatible peer is rejected before registration.

### 34.16 Incompatible protocol upgrades

Governance changes the parameters and constitutional content understood by the
running software. It does not install binaries or supply a new implementation
of consensus rules. Epoch markers likewise do not install code.

An incompatible format or rule change requires updated software and an explicit
deployment plan. The current exact-version handshake rejects incompatible peers;
there is no general governance-controlled fork scheduler or representative
readiness threshold that makes arbitrary code changes activate automatically.

---

## 35. Catastrophic Scenarios

### 35.1 Signature algorithm failure

Signing and verification use Ed25519. The current signature representation and
verification path do not negotiate another algorithm. Governance parameters and
epoch markers cannot replace the signature scheme.

A break of Ed25519 would require new software and an explicit recovery design,
including how to establish legitimate ownership when existing signatures can be
forged. The repository does not implement that recovery. Changing the algorithm
alone would not distinguish an owner's prior authorization from a forgery.

### 35.2 Hash algorithm failure

Blake2b is used for protocol hashes and proof of work. These uses are implemented
in code, not selected by a governable hash-algorithm parameter. A block version
identifies a supported format; it does not implement a replacement hash.

Recovery would require updated software and rules for existing identifiers,
signed history and work. Its scope depends on which hash property failed.
There is no automatic migration that guarantees old blocks remain trustworthy
after such a failure.

### 35.3 Compromised human electorate

Verification and governance depend on verified accounts representing distinct
people. A Sybil majority defeats that assumption; ordinary voting cannot be
assumed to repair an electorate controlled by the attacker. Representative
consensus has separate weight and quorum rules, so this is not a claim that half
the accounts automatically control every consensus decision.

The protocol has individual challenges and re-verification, but no automatic
majority-compromise reset. Recovery through a fork would require coordination
outside the compromised decision process and explicit rules for trusted state.
It is not an implemented recovery operation.

### 35.4 Compromised creator key

During bootstrap, a compromised creator key can exercise the creator's remaining
permissions, subject to their admission rules. These include bootstrap
verification, governance vetoes and epoch markers. This is more serious than
losing an ordinary spending key.

Creator authority ends permanently at the configured verified-count threshold,
capped at 50. Epoch admission checks that phase, and account-authored governance
activation is rejected in favor of the certified boundary. After exit the key
has ordinary account permissions; the public genesis identity remains the
network trust anchor. See [creator authority](docs/protocol/BOOTSTRAP.md).

---

## 36. Account recovery and interoperability

### 36.1 BRN after loss of verification

Revocation and deactivation stop future BRN accrual at the outcome timestamp.
They preserve the existing accounting history, including prior burns and locked
stakes. They do not reset earned BRN to zero. Available BRN still reflects those
deductions and any applicable stake settlement.

The account loses verified spending rights. Receiving a pending TRST payment
has separate admission rules, so describing the entire account as locked is
misleading. A wallet that has never been verified has no verified accrual period.
An unresolved challenge retains verified rights until its outcome is applied.

Successful re-verification resumes accrual from the retained history and excludes
the inactive interval. Fraud revocation also invalidates originated TRST;
inactivity deactivation does not.

### 36.2 Re-verification

Unverified and deactivated verification states can restart enrollment with a
new endorsement. An unsettled challenge prevents restart, and the endorsement
cannot predate the prior outcome. Restart clears the prior panel, votes and
attempt state; fresh endorsements and verifier selection are required.

Successful re-verification resumes the retained BRN accrual history without
credit for the inactive interval. The node's verification commit path also
handles restoration of revoked origins. Re-verification is not a new account
with its old ledger history erased. See [verification](docs/protocol/VERIFICATION.md).

### 36.3 Cross-chain interoperability

The current protocol has no native bridge, wrapped-asset operation or external
chain proof verifier. BRN accrual and TRST issuance follow BURST's own ledger
rules. Transferring value through an external service would depend on that
service's custody and verification rules; a BURST payment alone does not prove
or enforce a payment on another chain.

---

## 37. Comprehensive Parameter Defaults

### 37.1 Parameter and service defaults

[Protocol parameters](docs/protocol/PARAMETERS.md) lists the current Live defaults, units,
inactive fields and Test/Dev differences. The source authority is
ProtocolParams::burst_defaults(); a running network may have governed values
that differ from those defaults.

Operational limits are documented separately under [node operation](docs/node/NODES.md)
and [RPC and peer services](docs/node/SERVICES.md). Queue capacities, message limits
and connection policies are not all governable protocol parameters. Consult the
relevant implementation section here for the behavior each limit controls.

---


## Creator authority

Bootstrap authority and its permanent cutoff are described in
[creator authority during bootstrap](docs/protocol/BOOTSTRAP.md). Approved governance
changes use certified activation without a separate creator approval. Before
bootstrap ends, an explicit valid veto can reject an approved proposal during
its veto window. Silence does not veto it.

A veto finalized before exit remains a historical decision; it does not extend
the creator's authority. Later revocation, parameter changes or recovery cannot
reopen completed bootstrap. Source implementation and release qualification are
separate: deployment still requires the relevant validation evidence.
