# BURST in two minutes

BURST starts with a question: what if access to a spending allowance depended on
being a person, rather than owning money already?

## Two kinds of balance

A verified person accrues **BRN** while eligible. It is a personal allowance:
you cannot transfer it directly to someone else.

To pay someone, you can burn BRN. That creates **TRST**, which the recipient
accepts with a signed receive operation. They can then pay another person using
that TRST. Sending existing TRST does not create more money. Signing a payment
authorizes a request; the network must accept it before it changes ledger state.

## Currency with a history

TRST retains its origin and age as it changes hands. Splitting a holding preserves
that history. Merging holdings combines their amounts and uses the oldest input's
origin time. Neither operation resets the clock.

Once a holding reaches the governed expiry period, it cannot be spent. Its
age-based reference value is a separate calculation, not a guaranteed price.

If an origin is revoked for fraud, the protocol follows its contributions through
the merger graph. Clean value is separated from affected value. This can affect
later recipients, and it cannot undo goods or services already exchanged.
A later merge of the clean remainder keeps that distinction, even if the revoked
origin is subsequently restored. See [the worked example](ECONOMICS.md#merging-a-clean-remainder).

## People make the judgments

Endorsements, selected verifier panels and challenges determine account
eligibility. They provide a process for judgment, not a mathematical proof of
human identity. Mistakes and collusion remain risks.

Verified-account voting governs supported parameters, including accrual and
expiry. Representatives help the network agree on transactions; owning more
currency gives no extra representative weight.

The rules define how balances work. People still decide whether to accept TRST
and what to exchange for it.

Continue with [BRN and TRST](ECONOMICS.md), or read the
[whitepaper](WHITEPAPER.md) for the wider protocol.
