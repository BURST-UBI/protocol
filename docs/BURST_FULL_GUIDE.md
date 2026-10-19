# BURST: architecture, complete flows, and complexity

Written 2026-10-19 from the explanation accompanying the Linux public-testnet handoff. Code baseline: protocol `f6603d0`, wallet `0054a6d`, node-gui `a07323d`.

This guide separates the economic idea, the rules people participate in, and the machinery that makes nodes agree. It explains the implementation and its limits; it is not a replacement specification, a new performance benchmark, or a security audit. The owner's explicit decisions and the specification's recorded resolutions govern where older documents conflict. Release evidence is scoped to the selected Linux public testnet, not mainnet or indefinite scale.

The economic idea is fairly small. Most of the implementation complexity comes from making it survive disagreement, changing rules, unavailable nodes, lost responses, and restarts without inventing or duplicating money.

## Contents

1. [The whole system in one picture](#1-the-whole-system-in-one-picture)
2. [What the two economic parameters actually control](#2-what-the-two-economic-parameters-actually-control)
3. [The main objects you need to distinguish](#3-the-main-objects-you-need-to-distinguish)
4. [Creating a wallet and recovering it](#4-creating-a-wallet-and-recovering-it)
5. [Becoming verified](#5-becoming-verified)
6. [Genesis and the first people](#6-genesis-and-the-first-people)
7. [BRN accrual, spending, and staking](#7-brn-accrual-spending-and-staking)
8. [Paying somebody with BRN](#8-paying-somebody-with-brn)
9. [Paying with existing TRST](#9-paying-with-existing-trst)
10. [Pending payments, rejection, and expiry](#10-pending-payments-rejection-and-expiry)
11. [Splitting and merging TRST](#11-splitting-and-merging-trst)
12. [Expiry and reputation](#12-expiry-and-reputation)
13. [Challenging a verified person](#13-challenging-a-verified-person)
14. [How proportional revocation works](#14-how-proportional-revocation-works)
15. [Appeals, re-verification, and restoration](#15-appeals-re-verification-and-restoration)
16. [The block-lattice and transaction consensus](#16-the-block-lattice-and-transaction-consensus)
17. [Why BURST also needs checkpoints](#17-why-burst-also-needs-checkpoints)
18. [Randomness and drand](#18-randomness-and-drand)
19. [Governance and the constitution](#19-governance-and-the-constitution)
20. [Delegation, group trust, and family use](#20-delegation-group-trust-and-family-use)
21. [Offline actions and uncertain submissions](#21-offline-actions-and-uncertain-submissions)
22. [Node networking, NAT, and synchronization](#22-node-networking-nat-and-synchronization)
23. [Snapshots, restart, pruning, and backups](#23-snapshots-restart-pruning-and-backups)
24. [Time complexity: what the notation means](#24-time-complexity-what-the-notation-means)
25. [Where the current implementation is heavier than the ideal model](#25-where-the-current-implementation-is-heavier-than-the-ideal-model)
26. [The actual defaults versus testing shortcuts](#26-the-actual-defaults-versus-testing-shortcuts)
27. [What can go wrong even if the code follows the rules?](#27-what-can-go-wrong-even-if-the-code-follows-the-rules)
28. [What the current evidence does and does not establish](#28-what-the-current-evidence-does-and-does-not-establish)
29. [The simplest accurate mental model](#29-the-simplest-accurate-mental-model)

## 1. The whole system in one picture

BURST connects four things:

```text
Human verification
        │
        ├── determines who accrues BRN
        ├── determines who has a governance vote
        └── determines consensus voting weight
                         │
                         ▼
                  Agreed network rules
                         │
        ┌────────────────┴────────────────┐
        ▼                                 ▼
BRN computed over time           TRST expiry and validity
        │                                 ▲
        └── burn BRN → create TRST ────────┘
                                          │
                                transfer / split / merge
                                          │
                                traceable origin history
                                          │
                       fraud judgment → proportional revocation
```

The central thesis is:

> Every verified human has equal production potential. Spending that potential creates transferable money. The community controls how quickly potential accumulates and how long transferable money stays usable.

There are two different balances:

| Balance | What it represents | Can you transfer it? | How it appears |
|---|---|---|---|
| BRN | Your accumulated ability to create TRST | No | Computed from time and your recorded activity |
| TRST | Transferable value created by burning BRN | Yes, while valid | Recorded holdings with provenance |

A verified person also supplies one unit of democratic authority. Owning more BRN or TRST does not create more people or more votes.

The important limitation is that the protocol cannot directly observe "this is a unique human." People assess evidence, and the protocol enforces the resulting decisions.

Sources: [whitepaper](../BURST_WHITEPAPER.md), [decision resolutions](../spec/DECISION_RESOLUTIONS.md).

## 2. What the two economic parameters actually control

The main parameters are:

- `r`: the BRN accrual rate.
- `e`: the TRST expiry period.

The owner's selected initial configuration is:

```text
r = approximately 1 BRN per hour
e = no expiry
```

"Approximately" here only concerns integer representation. The implementation uses integer raw units per second, explained below.

The initial rate is the owner's explicit later decision. It supersedes the whitepaper roadmap's rate-zero launch wording.

### When the rate changes

Changing `r` affects new accrual after activation.

It does not recalculate everyone's past income using the new rate.

Example:

```text
First 10 hours at 1 BRN/hour:   10 BRN
Next 10 hours at 2 BRN/hour:    20 BRN
Total earned:                 30 BRN
```

The result is not 40 BRN.

### When expiry changes

Changing `e` affects the validity of existing TRST relative to its original effective age.

Suppose some TRST is 100 days old:

```text
Expiry period = 90 days:    expired
Expiry period = 120 days:   transferable again
Expiry period = 60 days:    expired
```

That revival is intentional. It is one of the owner's explicit requirements.

Extending expiry does not override fraud revocation. A token can be young enough to spend but still invalid because its origin was revoked.

### What these parameters do not control

They do not determine:

- The fiat exchange rate.
- What a shop charges.
- Whether vendors accept TRST.
- Whether the economy has enough goods.
- Whether verification incentives produce honest behavior in practice.
- Whether wealth or influence disappears outside BURST.

The whitepaper's economic-spectrum descriptions are interpretations of different configurations. Setting two parameters does not implement an entire political or economic system.

The code implements creation, distribution eligibility, transferability, and retirement rules.

## 3. The main objects you need to distinguish

Several terms sound similar but mean different things.

### Person

The human being the verification process is trying to identify uniquely.

### Account

An address and its signed account chain.

An address can exist before its owner is verified.

Creating a key pair does not give you a second verified identity.

### Wallet application

The software that manages keys, displays balances, prepares actions, signs them, and communicates with a node.

It is not the ledger itself.

### Node

A machine running the protocol.

It stores and validates ledger state, exchanges data with peers, processes consensus, and serves wallet queries.

### Representative

A participant that casts transaction-consensus votes using the human voting weight assigned to it.

Running ten nodes does not automatically give you ten units of authority.

### Humanity verifier

A verified participant selected to assess somebody's humanity claim or a challenge.

This is a different role from a transaction representative.

### Governance delegate

Someone authorized to exercise governance voting rights within a delegation scope.

This is also different from choosing a transaction representative.

### TRST holding

A particular amount of TRST associated with origin and lifecycle information.

Two holdings can have the same numeric amount but different:

- Ages.
- Origins.
- Expiry risk.
- Revocation exposure.
- Transfer histories.

That is why a TRST wallet needs more internal structure than a single balance number.

## 4. Creating a wallet and recovering it

### Creating a wallet

At a high level:

1. Generate recovery material and cryptographic keys.
2. Derive the account address.
3. Store the private key in an encrypted local keystore.
4. Ask the user to preserve their recovery phrase.
5. Connect to the selected network.
6. Read the account's actual network state.
7. Establish or recover the device's signing role where required.

The selected wallet backup is a 24-word recovery phrase.

The local keystore uses:

- Argon2id to derive an encryption key from the password.
- AES-256-GCM to encrypt the private key.
- Random salt and nonce.

The current Argon2id settings use approximately 64 MiB of memory and three iterations. Unlocking is deliberately more expensive than simply reading a file.

That cost depends on the password-derivation settings, not the number of accounts on the network.

Source: [keystore implementation](../wallet_core/src/keystore.rs).

### What restoring the phrase restores

It restores the cryptographic identity.

It does not automatically restore every piece of operational state from the previous installation.

In particular:

```text
Recovery phrase
    ≠ pending-action journal
    ≠ local device binding
    ≠ representative vote journal
    ≠ complete node database
```

This distinction matters after an uncertain submission.

If the old device signed a payment and lost the response, recovering the key on another device must not cause the new device to blindly sign a conflicting replacement.

### Multiple devices

The account still has one chain.

Two devices cannot safely treat the same account frontier as independently spendable.

The current block format includes a signing-device grant. The wallet has a local binding to that grant, and changing the active signer is an explicit operation.

This coordinates the supported consumer-wallet workflow. It is not protection against an attacker who already possesses the root private key.

Possession of the recovery phrase remains extremely powerful.

Sources: [signing-device storage](../wallet_core/src/signing_device.rs), [block format](../ledger/src/state_block.rs).

## 5. Becoming verified

A new account does not immediately accrue BRN.

The ordinary path is:

```text
Create account
    → obtain endorsements
    → agree the verification request and eligible pool
    → obtain the agreed randomness
    → select a panel
    → panel assesses evidence and votes
    → certify the outcome
    → activate verified status
```

### Step 1: present evidence

The applicant provides evidence of unique humanity.

The protocol does not mandate one universal evidence method.

A community might use in-person meetings, identity documents, or another method. Those methods have different privacy and fraud risks.

The protocol's job is to make the endorsement, selection, voting, and resulting state transitions checkable.

It cannot prove that a video call was honest.

### Step 2: endorsements

An already verified person endorses the applicant by permanently burning BRN.

This is not a temporary stake.

The endorser cannot later say:

> "The applicant failed, so give me the BRN back."

The resolved requirement also says endorsers receive no protocol reward for successful verification.

That differs from an older whitepaper passage suggesting an endorser reward. The explicit decision wins.

### Step 3: eligible verifier pool

Eligible participants must satisfy the relevant rules, including opting in and meeting applicable requirements.

The pool cannot be "whatever accounts this particular node currently knows about."

Nodes must agree on the underlying state that determines eligibility.

Otherwise two honest nodes could use the same random number but select different committees.

### Step 4: selection and voting

The selected verifiers assess the applicant.

The available judgments are:

- Legitimate.
- Illegitimate.
- Neither.

Legitimate and Illegitimate votes require the applicable BRN stake.

Neither requires no stake and receives no reward. It does not count toward the legitimate threshold.

Excessive Neither participation is tracked using a rolling history, rather than a lifetime percentage.

### Step 5: outcome

A successful outcome establishes verified status at an agreed boundary.

BRN accrual starts from the recorded verification time.

If the threshold is not met, the process can proceed through the configured revote rules. It does not retry forever without limit.

Offline verifiers and deadlines must also resolve through agreed protocol state. A node's local clock reaching the deadline is not enough to independently settle the panel.

### Reward accounting

Correct staked verifiers recover their stake.

The implemented verification reward pool comes from forfeited dissenting stakes. Those BRN burns back the corresponding TRST rewards.

If there are no forfeited stakes, there is no such pool to distribute.

"Correct" here means consistent with the protocol's settled judgment. It does not mean the software has discovered an objective truth about the person.

Sources: [verification outcome accounting](../verification/src/outcomes.rs), [voting rules](../verification/src/voting.rs).

## 6. Genesis and the first people

There is an unavoidable bootstrap problem:

> If only verified people can help verify people, how does the first person become verified?

BURST starts with a configured creator identity and bootstrap rules.

The creator's initial role is explicit. It is not an undisclosed ability to mint arbitrary money or permanently override governance.

The selected rules include:

- Equal BRN accrual.
- No premine.
- No extra consensus weight for the creator.
- A bounded bootstrap phase.
- No assumption that publicly known development keys are safe for the real public network.

The ordinary configured bootstrap exit threshold is 50 verified wallets.

That does not mean the system automatically becomes socially decentralized at wallet number 50. It means the software's bootstrap rule reaches its configured boundary.

Who those people are, how independently they operate, and who controls their representatives still matter.

## 7. BRN accrual, spending, and staking

The simplest BRN formula is:

```text
earned BRN = rate × time since verification
```

Available BRN is closer to:

```text
available BRN
    = accrual over eligible time intervals
    − permanently burned BRN
    − currently locked stakes
```

Spendability also has to respect returned-stake acknowledgment and signed account-state rules.

### No periodic payout transaction

If Alice leaves her wallet closed for a week, the network does not send her 168 hourly payouts.

When she returns, her wallet computes what she has accrued from the recorded facts.

There is no per-second minting job visiting every person.

That is the major efficiency benefit.

### Rate history

The node stores a global sequence of rate periods.

For example:

```text
Period A: 1 BRN/hour
Period B: 2 BRN/hour
Period C: 0 BRN/hour
```

A wallet intersects its eligible lifetime with those periods.

Rate changes therefore do not require updating every wallet's balance immediately.

### Paused accrual

If a person is unverified and later restored, the implementation preserves the periods during which accrual was paused.

It does not move the original verification timestamp forward and accidentally lose historical earnings.

It also does not grant income for the paused interval.

### Staking

Suppose Alice has 600 BRN available and stakes 500 in a panel:

```text
Available to spend: 100 BRN
Locked stake:      500 BRN
```

If the stake is returned, the 500 becomes available again through the settlement rules.

If forfeited, it becomes a permanent burn.

### Why returned stake needs signed dependencies

Nodes may receive messages in different orders.

One node might know a stake has been returned while another has not seen the outcome yet.

A later spend therefore cannot rely on an unexplained local credit.

The account can attach signed release dependencies. The node derives the credit from the settled stake records and prevents claiming it twice.

### Integer representation

Amounts use `u128` integers:

```text
1 BRN  = 10^18 raw units
1 TRST = 10^18 raw units
```

The rate is stored in raw units per second.

The current "1 BRN/hour" constant is:

```text
277,777,777,777,778 raw units per second
```

Multiplying by 3,600 gives 800 raw units more than exactly one BRN.

That difference is:

```text
0.0000000000000008 BRN per hour
```

It is negligible economically, but it is the exact implemented arithmetic. There are no floating-point balances.

Sources: [BRN state and rate calculation](../brn/src/state.rs), [parameter defaults](../types/src/params.rs).

## 8. Paying somebody with BRN

Suppose Alice buys something from Bob for 5 BRN.

The economic operation is:

```text
Alice burns 5 BRN
Bob becomes entitled to receive 5 newly created TRST
```

Alice does not transfer BRN into Bob's BRN balance.

Bob's own BRN counter remains independent.

### The wallet's side

The wallet:

1. Reads Alice's current account frontier and network context.
2. Computes available BRN using the relevant rules.
3. Checks the destination and amount.
4. Presents a review.
5. Obtains valid proof of work.
6. Constructs and signs the exact block.
7. Saves the action durably.
8. Submits it.
9. Reconciles its actual outcome.

The signed block commits to more than "Alice pays Bob 5."

It includes the account position and protocol context needed to prevent reinterpretation later.

### The node's side

The node checks:

- Format and version.
- Hash.
- Work.
- Signature.
- Account predecessor.
- Signing grant.
- Payment epoch.
- Timestamp and applicable rules.
- Verification rights.
- Available BRN.
- Dependencies.

It then processes the candidate and consensus.

"RPC accepted the request" and "the payment is confirmed" are different states.

### Bob's side

BURST uses a send/receive structure.

Alice's confirmed burn creates a pending receipt for Bob. Bob's receive action claims it into his account.

Bob's wallet can automate that when unlocked and when its acceptance policy allows it.

The result preserves Alice's burn as the creation origin.

### No proof of real-world delivery

The protocol proves that Alice authorized the burn and that the accounting followed the rules.

It does not prove that Bob delivered a meal, wrote useful software, or performed any real-world service.

"TRST represents contribution" is the economic interpretation of the payment, not something consensus can independently inspect.

## 9. Paying with existing TRST

Now Bob has 5 TRST and sends 2 to Carol.

No new BRN is burned.

No new economic supply is created.

Conceptually:

```text
Bob before:       5 TRST
Bob remainder:    3 TRST
Carol pending:    2 TRST
```

After Carol receives:

```text
Bob:              3 TRST
Carol:            2 TRST
Total:            5 TRST
```

The 2 TRST retains its original age and provenance.

It does not become fresh money because Bob sent it.

### Why there are holdings rather than one undifferentiated number

Suppose Bob has:

```text
3 TRST from Alice, created yesterday
4 TRST from David, created six months ago
```

A payment may need to select holdings or combine them.

The origin and effective age determine which value is transferable and what can later be revoked.

The user can see a simple total, but the wallet cannot safely discard those distinctions internally.

### Receiving while unverified

An unverified account can receive TRST under the selected rules.

That does not give it the ordinary verified spending rights or BRN accrual.

This permits receiving value before completing verification without treating receipt as proof of humanity.

## 10. Pending payments, rejection, and expiry

A pending payment is not money available to both sender and receiver.

It is value awaiting the receiver's claim.

### Receiver accepts

The receiver signs a receive block.

The pending entry is consumed once.

Replaying the same receive must not credit the account twice.

### Receiver rejects

The receiver can reject the pending payment.

The protocol returns value through the appropriate return-credit accounting.

It does not undo the past by deleting the original send.

For a BRN-origin payment, rejection does not rewind the sender's BRN burn and grant another opportunity to mint the same value.

### Receiver does nothing

A pending payment remains subject to the protocol's pending and expiry rules.

There is no general "sender gets it back after an arbitrary timeout" shortcut.

### TRST expires while pending

The chosen behavior is that expired pending TRST returns to the sender.

It returns with its lifecycle history. It is not made young again.

The receiver cannot receive an old pending transfer after it has already been settled back to the sender.

### Why this needs careful implementation

Several events can race:

- Receiver acceptance.
- Receiver rejection.
- Expiry.
- Origin revocation.
- Node restart.

The durable accounting has to select one valid outcome and preserve the dependencies.

That is why "just subtract here and add there" is not enough.

## 11. Splitting and merging TRST

### Splitting

A partial transfer splits the economic amount.

Example:

```text
Original holding: 100 TRST

Send:              30 TRST
Keep:              70 TRST
```

Both portions inherit the relevant origin and age.

Splitting must conserve value.

### Merging

Suppose you combine:

```text
40 TRST with 10 days remaining
60 TRST with 30 days remaining
```

The result is:

```text
100 TRST with 10 days remaining
```

The oldest effective age wins.

The merge does not reset expiry.

Future transfers can point to the merge as their origin reference, but the merge graph retains the inputs behind that reference.

### Why not copy every original origin into every future token?

Because repeated merges would make every holding carry an increasingly large ancestry map.

The selected design stores immediate inputs and references the graph.

For example:

```text
Burn A ─┐
        ├─ Merge X ─┐
Burn B ─┘           │
                    ├─ Merge Z
Burn C ── Merge Y ──┘
```

Merge Z stores its immediate inputs. It does not need to duplicate the complete history of A, B, and C in every subsequent transfer.

### Automatic merging

The wallet groups holdings with similar remaining lifetimes.

The current default similarity setting is 10%, with at most 256 inputs per merge.

This trades fewer holdings against potentially losing some remaining transferable lifetime.

The wallet's grouping step sorts holdings by expiry.

### Expired holdings

Expired holdings can be consolidated with other expired holdings.

They cannot be voluntarily split for spending.

The implementation rejects mixing live and expired holdings in a merge because the earliest-expiry rule would silently expire the live value.

### Internal fragment IDs

Automatic revocation, return, and restoration can produce distinct lifecycle portions.

Those portions need distinct internal identifiers even when there was no new user-signed transfer for each fragment.

The owner's accepted resolution permits deterministic internal fragment IDs while preserving original transaction and provenance links.

An internal ID distinguishes accounting state. It does not mint value.

Sources: [TRST engine](../trst/src/engine.rs), [wallet auto-merge](../wallet_core/src/auto_merge.rs).

## 12. Expiry and reputation

Expiry changes transferability.

It does not necessarily erase the historical record.

Suppose you hold 100 TRST:

```text
Before expiry:
    Transferable: 100
    Expired:        0

After expiry:
    Transferable:   0
    Expired:      100
```

The expired amount can remain visible as historical contribution.

However, that reputation interpretation has limits.

A person holding expired TRST may have received it through multiple transfers. The balance alone is not a cryptographic proof of how much socially useful work that particular person performed.

The provenance provides more context than the balance.

### Expiry is not gradual balance shrinkage

The protocol does not necessarily subtract 1% of your token quantity each day.

A holding remains transferable until its expiry boundary, then becomes non-transferable.

The wallet's linear age/value reference is informational. It does not force merchants to apply a linear exchange rate.

### Longer expiry can revive old value

This creates a storage consequence:

> If old TRST may become spendable again, the system must retain enough information to restore it correctly.

"Expired" cannot automatically mean "delete everything needed to reconstruct ownership and provenance."

That is one reason the whitepaper's broad pruning claims need more qualification than the economic description alone suggests.

## 13. Challenging a verified person

Suppose Alice believes Mallory's account is fraudulent.

The flow is:

```text
Alice submits a challenge and stakes BRN
    → agreed review context
    → selected verifier panel
    → evidence and votes
    → certified judgment
    → identity and economic consequences
```

Submitting a challenge is not itself a fraud conviction.

The final consequences require the protocol's judgment process.

### If fraud is upheld

The consequences include:

- Mallory loses verified rights.
- BRN accrual stops.
- TRST originating from Mallory's fraudulent BRN burns is revoked.
- Merged descendants are affected proportionally.
- Stakes and rewards settle under the applicable rules.

### If fraud is not upheld

The challenger can lose the challenge stake.

That makes frivolous challenges costly, although it does not prove harassment is impossible.

### Fraud versus non-fraud unverification

These are different decisions.

Someone may cease to qualify as an active verified participant without their earlier activity being fraudulent.

For example, the community's constitution may recognize death or inactivity as grounds for unverification.

That should stop future rights as specified without automatically declaring every past payment fraudulent.

The distinction is intentional.

## 14. How proportional revocation works

This is one of the most important BURST mechanisms.

Suppose a merged holding contains:

```text
40 TRST from a fraudulent origin
60 TRST from legitimate origins
Total: 100 TRST
```

It gets split between two people:

```text
Bob holds:    25
Carol holds:  75
```

Ignoring raw-unit rounding for this example:

```text
Bob's revoked portion:    10
Bob's valid portion:      15

Carol's revoked portion:  30
Carol's valid portion:    45
```

Both lose 40% of the affected holding.

The protocol does not arbitrarily put all of the damage onto whichever holder happens to be processed first.

### How the graph finds them

The graph has forward indexes:

```text
fraudulent burn
    → merges that consumed it
    → later merges
    → current affected holdings and pending value
```

The implementation traverses affected merges in dependency order.

A parent merge's taint must be known before calculating the contribution through its child.

### Rounding

The selected rule rounds the revoked portion upward at raw-unit boundaries.

For example, if the calculation produces a fraction of one raw unit, the revoked portion rounds up.

That is a deterministic conservative choice. It is not mathematically identical to maintaining infinitely precise fractions.

### Revocation does not undo real-world harm

If a vendor gave Mallory food and later loses the payment through revocation, the food is not restored.

The protocol corrects the currency's legitimacy accounting.

It does not provide insurance or reverse delivery of goods.

That is a real economic risk for recipients, and the whitepaper's "recoverable Sybil damage" language should be understood in that narrower sense.

Source: [merger graph](../trst/src/merger_graph.rs).

## 15. Appeals, re-verification, and restoration

A wrong fraud judgment must be distinguishable from a new application for verification.

### Re-verification

This asks whether the account should currently regain verified status.

Under the chosen behavior, restoring the originator can also restore previously revoked TRST, subject to other restrictions.

### Appeal

An appeal reviews a specific certified challenge judgment.

It names that judgment, rather than vaguely asking to "undo whatever happened."

The appeal has a sponsor, stake, panel, and certified result.

A stale appeal must not overwrite a later independent identity transition.

### Restoration does not mint another copy

Suppose 40 TRST was revoked and later restored.

The correct result is:

```text
Previously revoked 40 becomes valid again, if otherwise eligible.
```

The result must not be:

```text
Old 40 still exists
+
Newly minted replacement 40
```

### Expiry still applies

If the restored portion is too old under the current expiry period, it remains expired.

Restored legitimacy and transferable age are separate conditions.

### Reward corrections

The appeal machinery also has to correct rewards associated with the wrong judgment.

The current design tracks relevant issuance origins so rewards from an overturned judgment can become non-transferable without revoking the recipient's unrelated money.

Repeated reversals must not pay a fresh fraud bounty every time.

### Past BRN history is not casually rewritten

Historical burns, stake-return claims, and signed account balances are not simply reversed as if the intervening transactions never happened.

Restoration operates through explicit current-state rules.

This is substantially harder than changing a Boolean from `revoked = true` to `false`.

Source: [challenge review resolution](../spec/DECISION_RESOLUTIONS.md).

## 16. The block-lattice and transaction consensus

Now we get to how nodes agree.

### Each account has its own chain

Instead of one global sequence containing everyone's transactions:

```text
Alice: A0 → A1 → A2 → A3
Bob:   B0 → B1 → B2
Carol: C0 → C1 → C2 → C3
```

A transfer adds a dependency between account chains.

Bob cannot validly receive a payment that Alice never made.

This allows independent account activity without requiring every ordinary transaction to occupy one global ordering position.

It does not mean every operation is independent.

Identity and governance changes affect shared rules.

### A block commits to its context

Current state blocks include fields for:

- Account.
- Previous block.
- Operation type.
- Representative.
- Signed accounting state.
- Relevant links and origins.
- Timestamp.
- Rule commitment.
- Payment epoch.
- Signing-device grant.
- Operation-specific dependencies.
- Signature and work.

The exact structure depends on the operation.

### What a double-spend is

Alice signs two different successors of the same account position:

```text
            ┌→ pay Bob
Alice A10 ──┤
            └→ pay Carol
```

Those blocks compete.

Both cannot be accepted as the same account's next confirmed state.

### How voting weight works

Each verified human contributes one unit to their chosen transaction representative.

Example:

```text
Representative X: 40 humans
Representative Y: 35 humans
Representative Z: 25 humans
```

The units come from people, not token balances.

A wealthy person does not obtain extra units merely by holding more currency.

### Fixed authority context

The current system uses an agreed human roster and payment epoch.

A node cannot lower the quorum denominator merely because some people are unreachable.

Otherwise, two sides of a partition could each declare:

> "Only the people I can see count now."

Then both could finalize conflicting decisions.

### Final votes are durable promises

A representative's final vote must survive restart.

Restarting the process does not give it permission to vote for a conflicting block at the same position.

The durable vote journal is therefore part of consensus safety, not disposable cache data.

### Confirmation

A candidate must obtain the required final voting weight.

Then the node durably confirms it with its required dependencies.

The ordinary default is 67%, implemented through integer threshold arithmetic. The actual authority context determines the applicable quorum.

Some older comments still use "online weight" terminology. The current payment-authority path is based on installed certified authority; those comments should not be used to infer a local-online-denominator design.

Sources: [payment authority](../node/src/payment_authority.rs), [elections](../consensus/src/election.rs).

## 17. Why BURST also needs checkpoints

Account chains solve ordering within accounts.

They do not independently answer shared questions such as:

- Which humans exist for voting purposes?
- Which representative does each human empower?
- Which verifier pool is eligible?
- Which governance result is active?
- Which identity judgment has taken effect?
- Which old signing context is closed?

BURST uses certified checkpoints for these shared transitions.

### A checkpoint describes an agreed boundary

Think of it as:

> "These confirmed account histories, interpreted under these rules, produce this next shared state."

A proposer cannot just submit a preferred list of humans and call it the roster.

Other participants reconstruct the relevant state from the agreed ledger cut.

### Why old authority certifies new authority

If a proposed new electorate could authorize itself, an attacker could invent new voters and have them approve their own existence.

The transition needs authorization from the existing agreed authority.

That is why roster changes and identity outcomes have an explicit handoff.

### Payment epochs

An ordinary action names the payment epoch under which it was signed.

When a certified transition closes that epoch, the system can distinguish old actions from new-context actions.

This prevents a previously signed action from being silently reinterpreted under different authority or economic rules.

### Safety versus availability

If the necessary quorum is unavailable, the system may stop finalizing.

That is a deliberate safety trade-off.

It is not safe to "fix" it by deleting vote locks or ignoring offline humans.

## 18. Randomness and drand

BURST needs an unpredictable panel selection.

But randomness alone is insufficient.

These two steps must happen in the correct order:

```text
1. Agree what is being selected and from which pool.
2. Learn the agreed random output and derive the selection.
```

If an attacker can choose the pool after seeing the random output, the randomness does not provide the intended protection.

### What is frozen

The selection context commits to facts such as:

- Target.
- Triggering request.
- Attempt.
- Ledger cutoff.
- Parameter context.
- Eligible pool.
- Committee size.
- Future beacon round.

### What drand provides

In the current design, drand supplies a publicly verifiable randomness beacon.

The node pins the configured chain identity and public key and verifies the beacon's cryptographic proof.

A relay is a delivery mechanism. Trusting its HTTP response without verifying the proof would not be enough.

Drand does not decide BURST's eligible pool or certify BURST's identity outcomes.

### How the panel is derived

The certified selection path scores eligible addresses using the agreed randomness and context, then orders those scores deterministically.

Every node with the same inputs derives the same committee.

The current `SelectionRequest` implementation sorts the scored pool. Its selection work is therefore `O(P log P)` for a pool of `P` eligible people, before adding checkpoint replay and certificate work.

That is worth stating accurately. A separate helper has a partial-selection optimization, but its existence does not make the certified path linear.

### What if the beacon is unavailable?

The request waits for its exact agreed round.

The node can try another relay for the same data.

It cannot quietly substitute:

- Local randomness.
- A later convenient round.
- A different pool.
- A smaller committee.

### What if the proposal missed its pre-beacon deadline?

There is a distinction between:

- An unagreed proposal.
- A proposal with preparation evidence.
- A finalized selection.

Later consensus rounds can recover an unagreed expired proposal under the specified rules.

A prepared proposal retains its locking obligations.

A finalized selection retains its pool and beacon round.

Elapsed time alone cannot erase evidence that might exist on another node.

### What this does not prove

These rules do not prove unconditional freedom from selection bias under arbitrary network scheduling.

Safety of consensus locks and unpredictability of a beacon are necessary properties. They are not a complete proof of every economic or scheduling attack.

Sources: [selection request](../verification/src/selection_request.rs), [deadline recovery](../spec/DEADLINE_RECOVERY.md).

## 19. Governance and the constitution

The ordinary governance flow is:

```text
Proposal
    → paid support
    → voting
    → cooldown
    → activation
```

The implementation retains some historical names such as `Exploration`, but the current normal flow has one voting phase. A legacy `Promotion` enum value is not a second active voting requirement.

### Proposal

A verified participant submits the proposed change.

For longer content, a signed commitment binds the proposal to the actual content. Content availability matters; a hash alone does not tell voters what they are approving.

### Paid support

Supporters burn BRN to move a proposal through the anti-spam/support stage.

The burn is not refundable simply because the proposal later loses or is withdrawn.

### Voting

Each eligible human has one vote, directly or through authorized delegation.

Two thresholds answer different questions:

```text
Quorum:
    Did enough eligible people participate?

Approval threshold:
    Did enough of the relevant votes support the proposal?
```

A proposal with unanimous approval from one person may still fail quorum.

Abstention can count toward participation without counting as approval.

### Cooldown

A passed proposal waits through the prescribed preparation period.

This lets users and operators see what is coming.

### Activation

The change takes effect at its agreed boundary.

A node must not activate a change solely because its local timer fired while it lacks the certified outcome.

Activation needs to agree with the ledger cut, rules, and authority transition.

### Changing the BRN rate

Activation adds a rate segment.

Past accrual remains under past rates.

### Changing expiry

Activation reevaluates holdings against the new expiry period.

That may expire or reactivate existing holdings.

### Constitution amendments

The constitution supports structured changes to its materialized text while retaining amendment history.

Examples include adding, modifying, or repealing an article.

This does not make natural-language interpretation automatic. Verifiers and participants still have to interpret the community's rules.

### Emergency governance

The selected design has a shorter emergency voting route with higher participation and approval requirements.

It is not a creator-only override.

It also cannot summon a missing consensus quorum. Emergency governance still needs the network to process and certify the relevant decisions.

### Parameter changes versus code changes

Governance can change implemented governable parameters.

It cannot make arbitrary new Rust behavior exist by voting for a sentence.

Code-level self-amendment remains a separate future capability.

Source: [governance proposal model](../governance/src/proposal.rs).

## 20. Delegation, group trust, and family use

### Governance delegation

The owner selected scoped and transitive delegation.

For example:

```text
Alice delegates economic proposals to Bob.
Bob delegates onward to Carol.
```

The system resolves the applicable chain while checking eligibility and preventing loops from becoming extra votes.

More specific scopes can override broader ones.

Delegation can be revoked through the signed protocol path.

It does not create additional people.

### Transaction representative selection is separate

Choosing Bob as your transaction representative does not automatically mean Bob should decide every governance proposal for you.

Those are separate rights.

### Secondary delegation keys

The governance design also includes restricted delegation-key mechanisms.

The purpose is to authorize the relevant voting activity without handing over the wallet's spending key.

The grant's scope, validity, and revocation remain part of the authorization checks.

### Group trust

Group trust is a wallet policy layer.

A wallet may ask:

> "Do groups I trust vouch for the original creators behind this payment?"

This can include looking through merged provenance.

The group does not replace protocol verification or acquire authority to mint money.

A group being unavailable is also different from a group returning a negative or malformed response.

The wallet's configured fallback determines whether an unavailable lookup requires review or permits reliance on protocol checks.

This is an optional extra acceptance policy.

### Children and guardians

The whitepaper permits parents or guardians to manage a child's wallet until independent control is appropriate.

That must not be confused with the parent receiving an extra personal identity.

The code contains a custodianship registry, but a registry helper alone is not proof of a complete deployed guardian-authorization protocol. A finished consumer custody workflow cannot be inferred from that module's existence.

For the supported wallet, actual key custody and device handoff are concrete mechanisms. A richer guardian/recovery system needs its own end-to-end scope.

Sources: [governance delegation](../governance/src/delegation.rs), [trust policy](../wallet_core/src/trust_policy.rs).

## 21. Offline actions and uncertain submissions

Offline support means:

> Prepare the necessary context while connected, review and sign without a connection, then submit that exact action later.

It does not mean the wallet can confirm transactions while disconnected.

### The three phases

#### Preparation

Fetch the required public context:

- Account frontier.
- Epoch.
- Rules.
- Signing grant.
- Work.
- Relevant holdings or pending receipt.
- Panel or governance context where applicable.

#### Offline review and signing

The user reviews the operation.

The wallet signs the exact action and saves it durably.

It should not secretly make an RPC request during the disconnected signing step.

#### Submission and reconciliation

When connectivity returns, submit the saved bytes.

Do not rebuild a slightly different action and call it a retry.

### Why the journal matters

Imagine:

1. Alice signs a payment.
2. The node accepts it.
3. The response is lost.
4. Alice's wallet crashes.

After restart, the wallet cannot assume the payment failed.

If it signs a replacement immediately, it may create a conflicting account-chain action or cause a second payment later.

The saved journal lets it ask about the original hash and account position.

### What safely resolves uncertainty?

Examples include:

- The exact original action is confirmed.
- A different confirmed successor occupies the relevant account position.
- A certified epoch closure proves the old action is excluded.

A node saying "not found" is not necessarily proof.

That node might simply be behind.

### Stale context

An offline action may become stale because:

- Another device advanced the account.
- The epoch changed.
- The signer grant changed.
- A panel completed.
- A governance window closed.
- A holding expired or was revoked.

Offline support must report that truthfully.

It must not make stale actions "work" by bypassing those rules.

### Supported action families

The recent completion work extended the same reviewed-action and durable-journal approach across payments, TRST operations, identity actions, panel votes, and governance operations.

The evidence covers selected installed Linux journeys, including lost responses and restarts. It is not proof of every possible failure point on every platform.

Source: [offline action implementation](../wallet_core/src/offline.rs).

## 22. Node networking, NAT, and synchronization

### Joining the network

A node:

1. Loads its configuration and selected network identity.
2. Opens its own storage.
3. Recovers durable state.
4. Connects to configured bootstrap peers.
5. Performs the peer handshake.
6. Exchanges missing ledger and certified-state information.
7. Validates what it receives.
8. Becomes ready for the roles it can safely perform.

A peer count above zero does not prove the node is ready to sign.

It may still lack the current certified epoch or necessary history.

### Behind NAT

An outbound connection through NAT can carry traffic in both directions once established.

Therefore, a node can often participate by dialing reachable peers without accepting unsolicited inbound connections.

Inbound reachability is a separate issue.

Port forwarding or supported automatic mapping can help where the router permits them.

Carrier-grade NAT, restrictive firewalls, and double NAT can prevent inbound reachability despite local configuration.

The retained tests cover specific outbound, double-NAT, reconnect, forwarding, and emulator scenarios. They do not establish compatibility with every router.

### RPC is not P2P

The node's wallet/admin interfaces are separate from peer networking.

Opening the public P2P port does not require exposing administrative RPC or WebSocket endpoints to the internet.

The handoff intentionally keeps those private.

### Missing dependencies

Blocks may arrive before their predecessors or source payments.

The node can retain authenticated missing-dependency work within bounded queues and request the missing data.

It must not retain an unlimited amount of arbitrary unauthenticated input.

### Clock handling

Time affects:

- BRN accrual.
- Expiry.
- Verification deadlines.
- Governance phases.
- Pre-beacon signing windows.

Clock synchronization helps, but a timestamp alone is not consensus.

The system also needs agreed boundaries so two nodes do not make incompatible economic transitions because one clock crossed a deadline first.

## 23. Snapshots, restart, pruning, and backups

### Restarting an ordinary node

The node must recover more than balances.

Relevant state includes:

- Account frontiers.
- Pending receipts.
- BRN history and stakes.
- TRST holdings and provenance.
- Verification state.
- Governance state.
- Installed authority.
- Final-vote promises.
- Checkpoint locks and certificates.

Losing the last few items can be more dangerous than having to redownload transaction data.

### Authenticated snapshots

A snapshot is not trusted because it came from a familiar server or because its ZIP checksum matches.

The node needs to verify:

1. The chain of authority from the configured genesis.
2. The snapshot's quorum authorization.
3. The signed state commitment.
4. The actual body hash and encoding.
5. State consistency.
6. Compatibility with local durable commitments.

A malicious server must not be able to turn an arbitrary database into authoritative state by calling it a snapshot.

### Installation

Installation stages and validates the candidate, then publishes the appropriate state atomically.

A partial failure must not leave:

```text
new balances
+
old voting authority
+
missing recovery history
```

### Snapshot versus complete replay

A snapshot can avoid replaying every old ordinary transaction.

It does not eliminate the need to authenticate authority history or validate the supplied state.

The current implementation has framed archives and disk-backed handling for larger authority material, but still has explicit resource and size limits.

### Pruning

Pruning means removing data that is no longer needed in its current form while preserving what future validation requires.

It does not mean:

> "This token is expired, so every trace of it is permanently irrelevant."

Expiry reversal, appeals, provenance, and signing safety all constrain what can be discarded.

### Backups

Different backups serve different purposes:

| Backup | What it protects |
|---|---|
| Wallet recovery phrase | Cryptographic identity |
| Encrypted wallet file | Password-protected key material |
| Pending-action journal | Recovery of exact uncertain actions |
| Node database and journals | Local protocol and signing continuity |
| Authenticated snapshot | Verified state bootstrap/recovery |

A backup on the same disk is not protection against losing that disk.

Source: [snapshot archive design](../spec/SNAPSHOT_ARCHIVE.md).

## 24. Time complexity: what the notation means

This is where we need to be precise.

`O(1)` does not mean instant.

It means the operation's work does not grow with the particular input size being considered.

A fixed-size signature verification is `O(1)` with respect to the number of users, but it can still cost much more CPU than a hash-table lookup.

Likewise, `O(N)` does not automatically mean slow. Scanning 20 entries may be cheap. Scanning 20 million entries on every payment is a different matter.

The notation below uses:

| Symbol | Meaning |
|---|---|
| `N` | Verified people or tracked accounts, as stated |
| `B` | Stored ledger blocks |
| `R` | BRN rate-history segments |
| `I` | A wallet's paused-accrual intervals |
| `T` | Holdings in one wallet |
| `T_all` | Holdings across the node |
| `m` | Inputs to one merge, at most 256 |
| `P` | Eligible verifier-pool size |
| `K` | Selected panel size |
| `V` | Representatives whose votes are processed |
| `D` | Delegation-chain depth |
| `S` | Bytes/state material processed |
| `C` | Checkpoints or authority transitions processed |

These are code-path analyses, not measured throughput guarantees.

### BRN

| Operation | Complexity | Qualification |
|---|---|---|
| Time passing without an action | No per-wallet network work | No payout transaction |
| Read the current rate | `O(1)` | Last rate segment |
| Append a rate change | Amortized `O(1)` | Core history append only |
| Compute an ordinary BRN balance | `O(R)` | Scans rate history |
| Compute with paused intervals | Up to `O((I + 1)R)` | Current implementation evaluates eligible intervals |
| Basic burn/stake bookkeeping | Small local arithmetic plus balance calculation | Whole node processing can cost more |

The code comments suggest rate history is usually small. That is an expectation, not a protocol guarantee that governance will only change the rate a few times.

### TRST

| Operation | Complexity | Qualification |
|---|---|---|
| Check one loaded holding's expiry | `O(1)` | Timestamp comparison |
| Check a cached origin revocation flag | Expected `O(1)` | Hash lookup |
| Recalculate one portfolio | `O(T)` | Inspect holdings |
| Select/search holdings | Often `O(T)` | Depends on operation and indexes |
| Wallet auto-merge grouping | `O(T log T)` | Sort, then group |
| Basic merge over inputs | Roughly `O(m)` local work | Excludes provenance-revision lookups, state staging, storage |
| Apply a global expiry change | `O(T_all)` scan | Existing holdings can change state |
| Inspect complete provenance | Grows with visited ancestry | Not inherently constant-time |

The important wording is "one loaded holding."

A complete transfer may still involve portfolio scans, database access, candidate-state copying, consensus, and persistence.

### Revocation

The ideal graph-traversal component visits the affected graph:

```text
O(affected vertices + affected edges)
```

But the actual operation includes more:

- Finding all burn origins associated with the revoked person.
- Traversing the affected merges for each origin.
- Inspecting immediate merge inputs.
- Looking up composition revisions.
- Sorting affected holders.
- Scanning affected portfolios.
- Updating pending value.
- Persisting changes.

Therefore the honest description is:

> Revocation is proportional to affected provenance and accounting state, with additional implementation overhead.

It is not globally `O(1)`.

Revoking a prolific originator whose value spread through much of the economy can be a large operation.

Repeated origins may have overlapping descendants, so simply counting unique affected tokens can understate work.

### Verifier selection

The certified selection path:

1. Canonicalizes the pool.
2. Computes scores.
3. Sorts scores.
4. Takes the selected committee.

Its pool-processing cost is:

```text
O(P log P) time
O(P) temporary storage
```

The request and certification process also need replay and authority validation.

Panel tally and ordinary reward calculation are approximately `O(K)`, excluding the economic effects of a fraud outcome.

### Governance

For one person, resolving a delegation chain is up to:

```text
O(D)
```

Resolving an electorate of `N` people can require approximately:

```text
O(ND)
```

before considering proposal-specific storage, eligibility checks, or caching.

Processing multiple proposals multiplies the relevant work.

A rate update may have a constant-time core mutation, while the checkpoint that authorizes it still requires broader work.

An expiry update can visit all holdings.

### Transaction consensus

For one election:

- Processing a vote uses small map updates and cryptographic verification.
- Across `V` representatives, total vote-processing work grows with `V`.
- Certificate storage and checking grow with the included evidence.
- Broadcasting grows with the communication topology and retransmissions.

Do not translate this into "one payment costs exactly `O(V)` everywhere." The node also has ledger validation, dependencies, economic state, and storage work.

### Database access

LMDB uses tree-based storage.

A useful model for a keyed lookup is:

```text
O(log B)
```

in tree-navigation terms, with caching and page layout strongly affecting actual cost.

An indexed range query that returns `k` entries is roughly:

```text
O(log B + k)
```

But only when the endpoint actually uses an appropriate index.

An API having a cursor does not prove constant-time access to arbitrary pages. The current generic cursor format encodes an offset; some callers can still need to skip earlier records.

### Snapshots and recovery

Reading and hashing a snapshot requires at least:

```text
O(S)
```

work in the bytes processed.

Authority verification adds work for the certificate chain and signatures.

Replay requires visiting the replayed operations and paying their individual costs.

Streaming reduces peak temporary memory. It does not make processing the bytes free.

### Proof of work

For a uniformly distributed 64-bit work value and threshold `θ`:

```text
success probability per attempt = (2^64 − θ) / 2^64
```

Expected attempts:

```text
1 / success probability
```

Validation needs one work calculation.

Generation may need many attempts.

At the ordinary default threshold, the expectation is about 536.9 million attempts. Actual wall-clock time depends on hardware and implementation.

So "fractions of a second" should not be repeated as a universal promise.

Work can be prepared in advance because it uses the account predecessor, or the defined opening root. It does not supply voting weight.

## 25. Where the current implementation is heavier than the ideal model

This is the most important performance qualification.

### State staging can be wider than one account

The implementation stages candidate economic state so a failed transaction does not partially modify the live state.

That is a sound safety goal.

But staging can have a cost:

- BRN engine cloning includes tracked wallet state.
- TRST uses shared snapshots and copy-on-write.
- Mutating shared structures can detach and copy broader maps.
- Delta persistence reduces writes but does not automatically eliminate all scans or copies.

Thus a burn's mathematical balance calculation may be `O(R)` while the current full candidate-processing path can include work growing with wider node state.

Current end-to-end payments should not be described as constant-time in network size.

Sources: [candidate staging](../node/src/economic_candidate/staging.rs), [BRN wallet storage](../brn/src/wallet_states.rs), [TRST shared storage](../trst/src/shared.rs).

### Parallelism is real but bounded

The node has asynchronous networking and a separate authentication pipeline.

However, the central economic admission/installation path coordinates shared engines and the database writer.

It does not give every account an unrestricted independent database writer.

There is a `ParallelBlockProcessor` abstraction in the repository, but its presence is not evidence that the production economic path uses it for unlimited parallel account commits.

The current live path is visible in [node processing](../node/src/node.rs).

### Block-lattice does not imply unlimited throughput

A full node still has to:

- Receive relevant traffic.
- Validate signatures.
- Store data.
- Maintain indexes.
- Resolve dependencies.
- Participate in required agreement.
- Apply shared economic changes.

Independent account chains remove one ordering bottleneck. They do not remove every bottleneck.

The whitepaper phrase "scales with accounts, not transactions" should not be interpreted as "transaction volume has no cost."

### History still grows

With no expiry initially, active economic history can continue growing.

Even with finite expiry, restoration and authority history impose retention requirements.

There is no demonstrated theorem or benchmark showing the current node can run forever with constant storage or constant memory.

## 26. The actual defaults versus testing shortcuts

These distinctions matter when you test the app.

Selected ordinary defaults include:

| Rule | Ordinary configured value |
|---|---|
| BRN rate | Approximately 1/hour |
| TRST expiry | No expiry |
| Humanity endorsements | 3 |
| BRN burned per humanity endorsement | 336 |
| Panel size | 7 |
| Legitimate threshold | 90% |
| Verifier stake | 500 BRN |
| Challenge stake | 1,000 BRN |
| Fraud reward | 1%, capped at 2,000 TRST |
| Bootstrap exit | 50 verified wallets |

At a rate of one BRN per hour:

- 336 BRN represents about 14 days of accrual.
- 500 BRN represents about 20.8 days.
- 1,000 BRN represents about 41.7 days.

Those costs have a substantial effect on onboarding and participation.

Also, **six out of seven is about 85.7%, not 90%**. An old summary table's "90% (6/7)" annotation is mathematically inconsistent. With seven completed assignments and a true 90% requirement, seven legitimate votes are needed.

The actual lifecycle must also account for missing votes and certified deadline settlement; counting only whoever replied early would be a different rule.

### Testnet governance

The current Test configuration accelerates governance:

- Short windows.
- No proposal support cost in that testing configuration.
- Very low participation quorum.
- Lower approval threshold.
- Cheaper work.
- No verification-age wait.

These are not appropriate evidence that the ordinary governance economics work at public real-value scale.

The Test network exists to exercise functionality quickly.

A faucet or convenience issuance route must remain restricted to its intended test/development scope. Testnet convenience is not a general privileged money-creation rule.

Source: [actual defaults and overrides](../types/src/params.rs).

## 27. What can go wrong even if the code follows the rules?

Protocol correctness and social success are different questions.

### False humans get verified

Then those identities can receive BRN and voting power.

BURST's defenses aim to make this costly, detectable, and correctable. They do not prove perfect Sybil prevention.

### Honest humans get falsely revoked

Appeals and restoration provide a correction path.

They do not prevent the initial harm or guarantee an honest appeal panel.

### Representatives concentrate

Equal human units can still be delegated to a few operators.

One-person-one-unit does not imply evenly distributed operational control.

### A quorum goes offline

The network may lose liveness.

Safely preserving the quorum rule is preferable to letting each partition invent its own authority.

### A malicious majority coordinates

Cryptography cannot make a sufficiently powerful malicious electorate act honestly.

The protocol can enforce who voted and what rules were followed. It cannot make governance benevolent.

### TRST is refused by vendors

The accounting can work perfectly while the currency has little practical purchasing power.

The protocol does not force adoption.

### Fraud value was exchanged for real goods

Revocation removes invalid transferable value.

It cannot recover the goods.

### Privacy

Public provenance exposes financial relationships.

The current design is not a private-payment system.

Future privacy work cannot simply hide every origin without preserving whatever is needed for expiry, legitimacy, and revocation proofs.

### Lost keys

The supported backup restores keys if the user kept the recovery material.

There is no general guaranteed social recovery mechanism that can rescue a completely lost secret without introducing additional trust and attack surfaces.

## 28. What the current evidence does and does not establish

The current handoff records software qualification for the selected Linux public-testnet workflows.

That includes evidence for consumer payments, identity/governance journeys, offline signing and reconciliation, node recovery, and distribution artifacts within the recorded scopes.

It does not establish:

- Mainnet economic security.
- A proven Sybil tolerance percentage.
- Sustained public-load capacity.
- A maximum safe population.
- A guaranteed transactions-per-second figure.
- Unlimited history growth.
- Every router and firewall.
- Every desktop platform.
- Every disk failure or power-loss point.
- A formal proof that the incentive system always rewards honesty.

Some retained recovery measurements include roughly 160 seconds for an empty observer to catch up in a particular capped fixture and roughly 41 seconds for its restart recovery.

Those are fixture observations. They are not universal synchronization promises.

The release's limits are recorded in [operating limits](../deploy/OPERATING_LIMITS.md), with scoped evidence in the [completion ledger](../spec/COMPLETION_LEDGER.md).

## 29. The simplest accurate mental model

If you keep only a few distinctions in your head, keep these:

1. **BRN is computed entitlement.** It does not need periodic distribution transactions.
2. **TRST is transferable recorded value.** It carries age and provenance, and those affect what can happen to it.
3. **People supply authority.** Tokens do not supply extra consensus or governance votes.
4. **Account chains handle individual actions.** Certified shared transitions handle changes that affect everyone's interpretation of those actions.
5. **Confirmation is stronger than submission.** A lost response is uncertainty, not permission to sign again.
6. **Revocation and expiry are reversible under specified rules.** Restoration must preserve history and avoid duplication.
7. **Cheap local checks do not make the whole system constant-time.** The current node still pays for portfolios, shared-state staging, agreement, provenance corrections, and recovery.

BURST's distinctive architecture is the connection between equal human accrual, human-based authority, and traceable economic correction. The hardest engineering is preserving that connection when the network disagrees or loses state. The hardest unresolved broader question is whether the human verification and economic incentives remain trustworthy as the community grows.
