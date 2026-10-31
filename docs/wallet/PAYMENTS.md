# Sending and receiving

When Alice pays Bob, she authorizes the payment and Bob accepts it. BURST records
those as two signed operations because each person controls their own account
history. Alice can offer a payment; she cannot write an acceptance into Bob's
account on his behalf. Bob's wallet can accept automatically under his rules
or leave the payment for review.

For an application developer, this means "sent" and "received" are different
states to display and recover.

The sender either burns BRN to create TRST or sends TRST they already hold. When the node applies the sender's block, the payment is **pending**: its
TRST is reserved for the recipient, who has not yet received it. The stored
pending entry is called a **pending receipt**.

The recipient signs a receive operation to claim it. For a new account, that claim
can also open its account chain. The node checks the receipt, amount, provenance
and account predecessor, and consumes the pending entry once.

An account can also open empty, with zero TRST and zero recorded BRN spending.
Verification may precede its first signed block; opening the chain preserves that
identity state. Neither an empty Open nor a receive grants verification by itself.

For example, burning 5 BRN creates a pending receipt for 5 TRST; it does not
increase the recipient's BRN allowance. Sending 2 of those 5 TRST later leaves 3
with the sender and 2 pending for the next recipient, without new issuance.

An unverified account can receive TRST. Receipt does not grant BRN accrual or the
verified rights required for ordinary spending. The ledger also cannot establish
whether goods or services were delivered in exchange for a payment.

## Integrate a payment

1. Choose whether the payer will burn BRN or transfer existing TRST. These use
   different balance checks and block types.
2. Prepare and sign the sender's block locally. Save its exact bytes and hash
   before submitting it with [`process`](../development/RPC.md#submit-a-signed-block).
3. Check `block_info.confirmed`. A successful submission response alone is not
   confirmation.
4. On the recipient, query `account_pending` and review the receipt, including
   its origin and whether it is a return credit.
5. Sign an `Open` or `Receive` for that receipt, submit it, and check its confirmation.

The wallet library provides `pay_signed` for BRN, `transfer_signed` for existing
TRST, and `receive_signed` for a receipt. Use them with the wallet's
[signing and recovery state](WALLETS.md#building-and-signing-operations).
If a request times out, resolve the saved hash before creating a replacement.

## Acceptance, rejection and expiry

Suppose Alice has sent Bob 50 TRST. Until Bob acts, it is pending: Bob cannot
spend it, and Alice cannot spend it again. There are three possible outcomes.

If Bob accepts, he signs an `Open` or `Receive`. Once that receive is confirmed,
the 50 TRST belongs to him.

If Bob rejects, the money does not immediately become spendable by Alice. A new
pending payment goes back to her, and she must sign to claim it.

If the pending payment expires, the agreed expiry transition returns the holdings
to Alice and records a return credit. Her later receive acknowledges that credit;
it does not deliver another 50 TRST or make expired money young again.

These are effects of operations the node accepts, not merely buttons pressed in
a wallet. If Bob does nothing, the payment stays pending, subject to expiry and
other protocol rules. Silence is not acceptance or rejection.

When Bob rejects it with `RejectReceive`, the node creates a new pending payment
for Alice, identified by Bob's rejection block. Alice must sign to claim that
return. Expiry follows a different path, described below.

Rejection does not delete the original send or reverse a BRN burn. It returns
TRST through another pending receipt. Neither account's signed balance is silently
rewritten by the other's rejection.

Expiry returns work differently: the transition has already delivered the
holdings. The sender's later receive acknowledges the return credit without
delivering them a second time. It does not renew their age. The return goes to the
sender of the pending payment, who may differ from the original BRN burner.

## Expiry timing

A sender cannot treat silence as permission to spend the value again. Expiry is
based on the holding's effective origin time and the governed expiry period,
not an arbitrary interval after sending.

In the certified path, nodes first apply the agreed blocks from the old payment
epoch, then its deadline effects. The checkpoint closes that epoch against late
claims. A receive that has already consumed a receipt and an expiry return cannot
both credit the same payment.

Pending delivery validates provenance. An expired receipt cannot be accepted as
an ordinary live payment, and revocation can affect the value carried by a
receipt. These checks also run against the durable write state to detect changes
between preparation and commit.

## Wallet responsibilities

Display whether the action is a burn, an existing-TRST send, a receive or a
rejection. Keep the exact signed action until its outcome is resolved. An RPC
submission result is not the same as confirmation; a lost response is not proof
that no payment happened. See [offline signing and reconciliation](WALLETS.md#offline-actions).

The wallet's transfer-selection helper requires one origin to cover the entire
amount, though several active holdings from that origin can contribute. Without
an explicit origin, it selects the first sufficient origin in sorted string order,
not the holding nearest expiry. A balance spread across different origins may
therefore need a smaller payment or an explicit compatible merge first.

For amounts, ageing and merge behavior, see [BRN and TRST](../introduction/ECONOMICS.md).

## Implementation

- [Economic admission and delivery](https://github.com/BURST-UBI/protocol/blob/main/node/src/economic_candidate.rs)
- [Pending claim and rejection commit](https://github.com/BURST-UBI/protocol/blob/main/node/src/ledger_updater.rs)
- [Pending provenance and delivery](https://github.com/BURST-UBI/protocol/blob/main/node/src/block_economics.rs)
- [Certified expiry transition](https://github.com/BURST-UBI/protocol/blob/main/node/src/confirmed_replay/deadline.rs)
- [Durable expiry effects](https://github.com/BURST-UBI/protocol/blob/main/node/src/expiry_commit.rs)
