# BURST

BURST connects a computed spending allowance, BRN, with transferable currency,
TRST. Verified people accrue BRN while eligible. Burning BRN for a payment creates
TRST for a recipient; that TRST can then change hands until it expires or is
revoked.

![Burning BRN creates TRST for a recipient to accept.](assets/conversion.png)

## Start here

Read the [two-minute introduction](introduction/QUICK_TOUR.md) for the basic flow, then
[BRN and TRST](introduction/ECONOMICS.md) for balances, transfers and revocation. The
[whitepaper](introduction/WHITEPAPER.md) develops the design, assumptions and open questions.

## Choose a path

| You want to… | Start with |
| --- | --- |
| Use a wallet or recover an account | [Keys and recovery](wallet/WALLETS.md), then [payments](wallet/PAYMENTS.md) |
| Understand enrollment and challenges | [Verification](protocol/VERIFICATION.md) |
| Understand how rules change | [Governance](protocol/GOVERNANCE.md) and [parameters](protocol/PARAMETERS.md) |
| Run a node | [Node operation](node/NODES.md) and [installation](node/INSTALLATION.md) |
| Launch a network | [Launch preparation](node/NETWORK_LAUNCH.md) and [creator authority](protocol/BOOTSTRAP.md) |
| Host a group service | [Group services](node/GROUP_SERVICES.md) |
| Integrate a browser wallet | [Browser integration](development/BROWSER_WALLET.md) |
| Work on the implementation | [Build and test](development/DEVELOPING.md), then [architecture](development/ARCHITECTURE.md) |
| Evaluate a release | [Limits and assumptions](introduction/LIMITATIONS.md) and [build qualification](release/DOWNLOADS.md) |

These pages describe the implementation and its boundaries. Network settings and
release gates matter: consult the relevant operation's restrictions before using
it on a live network.
