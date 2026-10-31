# Terms and roles

| Term | Meaning |
| --- | --- |
| Person | The human whom verification aims to distinguish from other participants. An address alone does not establish uniqueness. |
| Account | An address and its signed chain. It can exist before verification. |
| Wallet | Software that manages keys, reviews and signs operations, and reads network state. It is not the ledger. |
| Node | Software that stores and validates ledger state and communicates with peers. Running additional nodes does not create more voting weight. |
| Representative | A transaction-consensus voter receiving equal-weight units delegated by verified accounts. |
| Verifier | An eligible participant selected to assess an enrollment or challenge. This is separate from the representative role. |
| Governance delegate | An authorized participant or key used to resolve governance ballots within a scope. |
| BRN | An account's non-transferable allowance accrued over eligible time, reduced by burns and unavailable stakes or releases. |
| TRST holding | An amount with ownership, origin, effective age and lifecycle state. Equal amounts can have different expiry and revocation exposure. |
| Pending receipt | A sent payment awaiting a recipient claim or another valid settlement. |
| Provenance | The origins and composition relationships needed to track value through transfers, splits and merges. |
| Burn origin | The BRN burn from which a contribution of TRST was created. A holding can instead refer to a merge whose graph leads back to burns. |
| Merger graph | Immediate input relationships between burns and merges, indexed so revocation can find downstream contributions. |
| Composition basis | Which part of an origin a holding represents: its whole mixture, one separated origin, or a recorded remainder. |
| Composition revision | A shared record of an origin's exclusion or restoration and the remaining amount at that step. Later merges retain the revision of the inputs they consumed. |
| Expired holding | A holding too old to spend under the current expiry period. A longer governed period can make it active again. |
| Revoked holding | Value invalidated through its provenance. Restoration and expiry checks determine whether it can become spendable again. |
| Signing grant | The account's protocol context for its active signing device; distinct from governance delegation. |
| Checkpoint | An agreed boundary or decision with the certificates and context required by its protocol path. |

Start with [the introduction](QUICK_TOUR.md), then follow
[payments](../wallet/PAYMENTS.md), [verification](../protocol/VERIFICATION.md) or
[node operation](../node/NODES.md).
