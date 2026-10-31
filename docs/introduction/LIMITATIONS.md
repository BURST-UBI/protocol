# Limits and assumptions

## Feature availability

The source includes features that are not enabled on Live. Check these before
building an integration around a protocol operation:

| Feature | Default build | Dev/Test with `experimental-testnet` | Live |
| --- | --- | --- | --- |
| Certified verifier selection and activation | Disabled | Enabled | Disabled |
| Governance operations requiring certified activation | Disabled | Enabled | Disabled |
| Full governance content | Disabled | Enabled | Disabled |
| Ledger-backed governance-key delegation | Disabled | Enabled | Disabled |
| Appeals of challenge outcomes | Disabled | Enabled | Disabled |

The experimental flag permits these paths; accounts must still meet each
operation's eligibility and state requirements. Bootstrap verification has
[separate rules](../protocol/BOOTSTRAP.md).

The switches are in
[verification](https://github.com/BURST-UBI/protocol/blob/main/verification/src/lib.rs)
and the [node release policy](https://github.com/BURST-UBI/protocol/blob/main/node/src/release_policy.rs).
Physical removal of historical block bodies also remains disabled by its release
gate; see [node storage](../node/NODES.md#pruning-and-retirement).

## Protocol assumptions

Correct accounting does not by itself establish economic usefulness or honest
human judgments. These risks remain relevant even when software follows its rules.

| Risk | Consequence |
| --- | --- |
| False identities pass verification | Extra accounts can gain BRN and human-based voting weight. Endorsements and challenges do not prove perfect Sybil prevention. |
| Honest accounts are wrongly revoked | Recipients can lose affected value. Restoration can correct accounting, but not the intervening harm; appeals are currently gated on Live. |
| Representatives become concentrated | Equal account weights can still be delegated to a small number of operators. |
| Required quorum becomes unavailable | Finalization can stop. Each partition cannot safely redefine authority around its reachable peers. |
| A sufficiently powerful electorate colludes | Signatures establish who authorized an action, not that their decision is benevolent. |
| Merchants refuse TRST | The ledger can function while the currency has little purchasing power. Acceptance is voluntary. |
| Fraudulent value buys real goods | Revocation cannot retrieve the goods or insure the recipient. |
| Public provenance is linked to people | Addresses and transfers can expose financial relationships. This is not a private-payment system. |
| Recovery secrets are lost | The wallet cannot recreate an unknown signing key. Local guardian records are not a general social-recovery service. |

Tests and benchmarks can establish specific behavior under recorded conditions.
They do not prove a safe fraudulent-account percentage, incentive equilibrium,
unlimited capacity or correctness under every failure mode. Public claims must
match the evidence's scope.

Read [verification](../protocol/VERIFICATION.md), [privacy](../release/PRIVACY.md),
[wallet recovery](../wallet/WALLETS.md) and [performance](../development/PERFORMANCE.md) for the mechanisms
and their practical boundaries.
