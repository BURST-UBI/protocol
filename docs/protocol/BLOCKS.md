# Accounts and blocks

Each BURST account has its own ordered chain of signed blocks. A block names its
predecessor and describes one operation. Different accounts can advance
independently, but a receive depends on the payment it claims, and shared rules
such as verification and governance require agreement across the network.

The latest block hash is the account's **head**, also called its frontier.
Two blocks with the same predecessor compete for the same account position.
[Representatives](CONSENSUS.md) vote to resolve that conflict.

## Opening an account

Generating keys creates an address locally. It does not create a ledger block.
The first block is an `Open` with a zero predecessor. It can claim a pending
payment or open the chain with no TRST. Opening and verification are separate:
receiving money does not grant BRN accrual or verified voting rights.

An account may already have verification state before its first signed block.
Opening its chain preserves that state.

## Operations

These are the current `BlockType` names. Their presence in the format does not
mean every operation is enabled on every network.

| Block type | Purpose |
| --- | --- |
| `Open` | Start an account chain, optionally claiming a pending payment. |
| `Burn` | Spend BRN to create an equal amount of pending TRST for a recipient. |
| `Send` | Transfer existing TRST into a pending payment. |
| `Receive` | Claim a payment or acknowledge a return credit. |
| `RejectReceive` | Refuse a pending payment and create a return receipt for its sender. |
| `Merge` | Combine holdings without renewing their age. |
| `ChangeRepresentative` | Choose the representative receiving the account's consensus weight. |
| `ChangeSigner` | Replace the account's active signing-device grant. |
| `Endorse` | Support another account's verification application. |
| `VerifierOptIn`, `VerifierOptOut` | Volunteer for or leave the verifier pool. Opt-in alone does not establish selection eligibility. |
| `VerificationVote` | Record a selected verifier's judgment. |
| `Challenge` | Challenge an account, or name a prior outcome for an appeal where enabled. |
| `GovernanceProposal` | Submit a proposed parameter, constitution or representative-status change. |
| `GovernanceEndorse` | Burn BRN to support a proposal entering voting. |
| `GovernanceVote` | Vote on a proposal. |
| `GovernanceWithdraw` | Withdraw the signer's proposal during its proposal phase. |
| `Delegate`, `RevokeDelegation` | Update governance voting delegation. These do not select a consensus representative. |
| `GovernanceVeto` | Exercise the creator's temporary bootstrap veto before activation. |
| `Epoch` | Add a bootstrap-authorized marker to an existing account chain. It installs no software. |
| `GovernanceActivation` | Retained encoding; current nodes reject account-authored activation blocks. Activation occurs through a certified boundary. |

For payment rules, see [sending and receiving](../wallet/PAYMENTS.md).
Verification, governance and bootstrap operations have their own eligibility,
stake, timing and [availability](../introduction/LIMITATIONS.md#feature-availability)
requirements.

## Fields to understand before integrating

Use the wallet library's builders rather than guessing fields from another block.
The current block version is 9; `params_hash` RPC reports the node's version and
current signing context.

| Field | Meaning |
| --- | --- |
| `account`, `previous` | Account and predecessor hash; `previous` is zero for `Open`. |
| `representative` | The account's selected consensus representative. |
| `brn_balance` | Serialized name for cumulative BRN committed by burns and stakes. It is **not available BRN**. Rust calls this field `brn_spent`. |
| `trst_balance` | Transferable TRST balance after this block, not the amount of the operation. |
| `link` | Operation-specific reference: recipient public key for a payment, source block for a receive, proposal hash for a governance vote. |
| `origin`, `transaction`, `target_head` | Operation-specific provenance, vote or dependency fields. They are not interchangeable transaction IDs. |
| `merge_sources` | Holding IDs to consume in a merge, up to 256. An empty merge list requests deterministic selection. |
| `release_sources` | Sorted, distinct vote-set hashes acknowledging stake releases, up to 1,024. |
| `signing_grant` | Active signing-device grant before this operation; zero means unenrolled. |
| `payment_epoch` | The agreed economic boundary used when preparing the operation; initially the configured genesis hash. |
| `params_hash` | Commitment to the parameter values used by the operation. |
| `timestamp`, `work`, `signature`, `hash` | Signed timing, anti-spam work and authentication fields. Use the library's canonical hash and signing functions. |
| `delegation_payload` | Scoped governance-key operation or secondary ballot, where enabled. It does not replace the account signature or authorize spending. |

Blocks do not contain the whole account state. Holdings, provenance, pending
receipts, eligibility and shared protocol state are stored separately. Saving
only account heads is insufficient to validate or restore a node.

## Submit and confirm

A valid signature proves authorization, not sufficient balance or confirmation.
The node also checks dependencies, work, the current protocol context and the
operation's rules. A submission may wait for missing dependencies or compete
with another block at the same position.

Keep the signed bytes and hash, submit with [`process`](../development/RPC.md#submit-a-signed-block),
then query confirmation. If the request times out, check that hash before
constructing another payment. See [wallet recovery](../wallet/WALLETS.md#a-lost-response-is-not-a-failed-payment).

Source: [block format and validation](https://github.com/BURST-UBI/protocol/blob/main/ledger/src/state_block.rs),
[block processing](https://github.com/BURST-UBI/protocol/blob/main/node/src/block_processor.rs),
[wallet signing](https://github.com/BURST-UBI/protocol/blob/main/wallet_core/src/signed_client.rs).
