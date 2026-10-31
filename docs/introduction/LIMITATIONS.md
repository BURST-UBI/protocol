# Limits and assumptions

Understanding the rules also means understanding what they depend on. This page
separates features not yet available on Live from risks that remain even when
the software follows its rules.

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

Verification can be wrong in either direction. A false identity that passes can
gain extra income and voting weight. A real person wrongly judged fraudulent can
lose rights, and honest recipients can lose affected payments. Restoration can
correct the accounting but cannot undo an intervening loss of goods or opportunity.
Appeals are currently disabled on Live.

Equal account weights do not guarantee widely distributed control. People can
delegate them to a small number of representatives. If too much required weight
goes offline, agreement can stop; each disconnected part of the network cannot
safely redefine the voting group around the nodes it can reach. If enough voters
collude, their signatures can authorize a harmful decision. Signatures establish
who agreed, not whether the decision was good.

A functioning ledger does not guarantee purchasing power. Merchants may refuse
TRST, and a payment may buy goods before its fraudulent origin is discovered.
Revocation cannot retrieve those goods or insure the recipient.

The ledger is also public. Linking payment history to real people can reveal
financial relationships. Losing the secrets that control an account presents a
different risk: the wallet cannot recreate an unknown key. Its local guardian
records do not provide a general social-recovery service.

Tests and benchmarks can establish specific behavior under recorded conditions.
They do not prove a safe fraudulent-account percentage, incentive equilibrium,
unlimited capacity or correctness under every failure mode. Public claims must
match the evidence's scope.

Read [verification](../protocol/VERIFICATION.md), [privacy](../release/PRIVACY.md),
[wallet recovery](../wallet/WALLETS.md) and [performance](../development/PERFORMANCE.md) for the mechanisms
and their practical boundaries.
