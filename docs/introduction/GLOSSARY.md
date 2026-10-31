# Glossary

| Term | Meaning |
| --- | --- |
| Person | The human behind an account. Verification aims to prevent one person from enrolling more than once; an address alone cannot prove this. |
| Account | An address and its chain of signed blocks. An account can exist before verification. |
| Wallet | Software that manages keys, signs operations and reads account state from a node. |
| Node | Software that validates blocks, stores the ledger and communicates with other nodes. Running more nodes does not give an account more votes. |
| Representative | An account that votes to confirm blocks. Each verified account can delegate one unit of voting weight to a representative. |
| Verifier | A participant selected to judge an account's verification request or a challenge against it. This is separate from confirming transactions. |
| Governance delegate | An account or key authorized to cast governance votes for another account within a defined scope. |
| BRN | A personal spending allowance earned while an account is eligible. It cannot be transferred. Burns spend it; locked stakes and unacknowledged stake returns reduce what is available. |
| TRST holding | An amount of transferable currency owned by an account, with a recorded origin and age. Holdings can differ in expiry and exposure to revocation. |
| Pending receipt | A payment sent but not yet claimed by the recipient, rejected or otherwise settled under the protocol rules. |
| Provenance | The history of where TRST came from, including the inputs to splits and merges. |
| Burn origin | The BRN burn that created a contribution of TRST. A merged holding can contain contributions from several burns. |
| Merger graph | A graph recording the inputs to each merge. It lets the node trace which later holdings contain value from a revoked origin. |
| Composition basis | The part of an origin represented by a holding: the original mixture, a separated contribution or the remainder after separation. |
| Composition revision | A record of which contributions were excluded or restored at a particular step. Later merges keep the revision of the inputs they actually consumed. |
| Expired holding | TRST too old to spend under the current expiry setting. Increasing that setting can make it spendable again. |
| Revoked holding | TRST invalidated because of its origin. Restoration can reverse that invalidation, but expiry and other invalidation reasons still apply. |
| Signing grant | The protocol record that identifies the account's active signing device. It does not grant governance voting authority. |
| Checkpoint | An agreed boundary in account history and the shared state calculated from it. Nodes check both the agreement signatures and the resulting state. |
| Head / frontier | The latest block hash in an account's chain. |
| Quorum | The voting weight required for agreement. In the certified path, it comes from the installed electorate, not the number of currently reachable peers. |
| Cementation | Recording confirmed blocks and their dependencies durably, advancing account confirmation heights. |
| Payment epoch | The agreed economic and authority context named by an ordinary signed block. Initially it is the configured genesis hash. |
| Ledger cut | The agreed prefixes of account chains used to calculate a checkpoint's shared state. |
| Return credit | A pending record acknowledging expired payment value already returned to its sender. Receiving it does not deliver the value again. |

For examples, see [sending and receiving](../wallet/PAYMENTS.md),
[account verification](../protocol/VERIFICATION.md) and
[checkpoints](../protocol/CHECKPOINTS.md).
