# Payment acceptance policies

A payment can follow every network rule and still carry a risk you do not want
to accept automatically. If the person whose BRN created it is later found to
have enrolled fraudulently, the affected value can become invalid, even after
it reaches you. Knowing the sender is not necessarily enough.

Wallet acceptance policies let you decide which payments to accept automatically
and which to review yourself. You might trust particular creators, or rely on
membership in a group you know. Those are your preferences; they do not change
what the network considers a valid payment. They also cannot guarantee that a
trusted person will never be found fraudulent.

## Sender or creator?

Suppose Alice burns BRN to pay Bob, and Bob sends that TRST to you. Bob is the
**sender**, but Alice is the **creator** of that value. A sender allow-list checks
Bob. A creator policy checks Alice. If Bob sends a merged holding, the policy
checks every creator whose contribution remains in it.

| Policy | What the wallet checks |
| --- | --- |
| Protocol only | Normal protocol validity without an additional creator or group restriction. |
| Allowed senders | The immediate sender is on your list. An empty list adds no sender restriction. |
| Trusted creators only | Every surviving original creator is on your personal list. |
| Any selected group | Each creator belongs to at least one of your selected groups. |
| Every selected group | Each creator belongs to all your selected groups. |

The sender filter can apply in addition to a creator or group policy. A rejected
policy check is not a signed `RejectReceive`: holding a payment for review does
not automatically return it to the sender.

## Personal creator trust

The wallet's **Trusted creators only** mode checks every distinct original BRN
creator whose contribution survives in a payment. Each must appear in your saved
address list, even when someone else sends the payment. A merged payment can have
several creators; trusting its sender alone does not satisfy this policy.

This mode uses the same node-reported, complete and confirmed provenance as group
checks, without contacting group services. Missing history, invalid addresses or
an unlisted creator leave the payment pending for review. The list must contain
1–256 addresses. Revoked contributions already removed by accounting are excluded;
the clean remainder is checked. Manual receiving remains available.

**Allowed senders** is a separate, additional filter on the immediate sender.
An empty sender list imposes no sender restriction. Creator trust does not change
protocol validity, revocation rules or signing authority.

## Group trust

Groups operate outside consensus. A registered group supplies a membership
endpoint; its membership judgment is an additional signal for a wallet, not proof
of protocol eligibility or authority to mint TRST.

Wallet policies can require membership in at least one selected group, require
all selected groups, or explicitly permit bounded paths through other registered
groups. Custom rules can also inspect verification age and the share of unknown
origins when those facts are available.

Payment group checks inspect original creators behind the payment's provenance,
not just its immediate sender. They require complete, confirmed provenance and
check surviving, non-revoked contributions. A revoked historical contribution
must not automatically disqualify the clean value left after protocol accounting.

The wallet obtains these identities from its configured node. It checks report
structure, matching roots, valid addresses and unique, ordered burn hashes; it does
not independently prove the node's confirmation or completeness claims. The report
lists origins without contribution amounts, so counting listed origins cannot
establish the proportion of unknown value. That custom rule needs separate amount
and total facts.

The source inspector follows at most 64 payment/return blocks and rejects cycles.
An origin report can contain at most 8,192 burns. These inspection limits are
separate from the smaller group-query limits below; a structurally valid origin
report can still be too large for automatic group checking.

The current payment check limits inspection to 256 originators and 32 groups;
group paths are limited to eight hops. Creator checks share an eight-request
network budget. Overlapping paths reuse membership and endorsement responses
only within that payment evaluation. Incomplete inputs, exceeded limits and
timeouts require review instead of silently accepting the payment.

### Decisions and unavailable services

The policy returns **Accept** when it permits automatic acceptance, **Reject**
when the payment fails a configured rule, or **Warn** when the wallet needs review
or an explicitly configured fallback. These are policy results. Accept does not
skip protocol validation, and Reject does not itself send the money back.

The explicit protocol-only fallback applies when the group client reports service
unavailability, including connection failures and HTTP server errors such as 503.
It does not turn a negative response, an unknown group ID or malformed data into
approval. An overall inspection timeout requires review. Users can also choose
review for service outages.

For an “any group” policy, one positive membership response is enough even if
another group cannot be reached. For an “all groups” policy, one negative response
is enough to reject. Custom rules require every check to pass: a rejection takes
precedence over missing facts, which produce a warning. An empty custom rule list
adds no restriction; an empty required-group list asks for review instead.

For directory formats, endpoint contracts and hosting instructions, see
[running a group service](../node/GROUP_SERVICES.md).

Queries disclose the inspected address to the group service and may reveal payment
interests. See [privacy](../release/PRIVACY.md) before configuring external endpoints.

## Implementation

- [Wallet trust policy](https://github.com/BURST-UBI/protocol/blob/main/wallet_core/src/trust_policy.rs)
- [Group queries](https://github.com/BURST-UBI/protocol/blob/main/groups/src/client.rs)
- [Group-to-group traversal](https://github.com/BURST-UBI/protocol/blob/main/groups/src/trust_graph.rs)
