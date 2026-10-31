# What is BURST?

BURST gives verified people a personal income and lets them use it to pay one
another. Its name combines **BRN**, from *burn*, and **TRST**, from *trust*.

## Start with a payment

Imagine you have accumulated 50 BRN. You buy something from a shop for 50.
Your wallet burns those 50 BRN, creating 50 TRST for the shop to receive.

![Burning 50 BRN creates 50 TRST for a recipient.](../assets/conversion.png)

The shop can spend its TRST with someone else. That person can spend it again.
The same 50 TRST moves between people; each transfer does not create another 50.

Meanwhile, your BRN continues to accrue while you are eligible. At the current
default rate, you gain approximately one BRN per hour, just like other verified
people.

## Why two kinds of balance?

BRN is personal. You can save it, but spending it burns it. TRST is transferable:
it is what someone receives when you pay them, and they can pass it on.

The community can set a lifetime for TRST. If that lifetime is finite, TRST
becomes unspendable when it reaches that age, even if it has changed hands.
Unused BRN does not expire. The current defaults give TRST an effectively
unlimited lifetime.

## What keeps it fair?

Verification aims to give each person one eligible account. Existing participants
endorse applicants, and selected verifiers judge them. Challenges provide a way
to review fraud or inactivity later.

TRST keeps a record of its origin. If an account obtained verification
fraudulently, revocation can follow the value it created through later transfers
and merges. That can affect recipients too, which makes the quality of
verification important.

## Who chooses the rules?

Verified accounts vote on supported settings, including the income rate and TRST
lifetime. Money does not buy extra voting weight. Separate representatives vote
to confirm transactions and help nodes agree on ledger state.

Read the [whitepaper](WHITEPAPER.md) next. It develops these ideas with the original
payment, splitting and merging examples. The
[implementation status](WHITEPAPER.md#implementation-status) identifies features
that are currently restricted to experimental networks.
