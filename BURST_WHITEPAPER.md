# BURST: The General Form of Money

**A Parameterized Economic Protocol Where Every Monetary Distribution Model Is a Configuration**

*Nitesh Gautam — February 2026*

---

## Abstract

BURST is a decentralized economic protocol built on two parameters: a universal income rate and a currency expiry period. By adjusting these two values, BURST can express any monetary distribution model — from pure capitalism to universal basic income to a reputation economy — as a configuration. Normal money is BURST with its parameters set to their extremes: income rate at zero, expiry at infinity. No other protocol has demonstrated this property. Credit, debt, interest, and complex financial instruments can be built on top of BURST the same way they are built on top of any base currency — but the base layer itself is more general than any that currently exists. The choice of where to sit on this spectrum is made democratically by the people using the system, not by the people who designed it. BURST does not advocate for any economic model. It builds the infrastructure for all of them.

---

## Table of Contents

- [Part 1 — Vision](#part-1--vision)
- [Part 2 — The General Equation](#part-2--the-general-equation)
- [Part 3 — Architecture](#part-3--architecture)
- [Part 4 — Security and Verification](#part-4--security-and-verification)
- [Part 5 — Infrastructure](#part-5--infrastructure)
- [Part 6 — Economics](#part-6--economics)
- [Part 7 — Comparison](#part-7--comparison)
- [Part 8 — Open Questions and Roadmap](#part-8--open-questions-and-roadmap)

---

# Part 1 — Vision

## The Problem

Every monetary system in history makes the same assumption: you start with nothing, and you must earn your way to survival. Whether that system is gold, fiat, Bitcoin, or any other cryptocurrency, the starting point is zero. Distribution is someone else's problem — usually a government's, an employer's, or no one's at all.

This creates a cycle:

- Those born with wealth accumulate more.
- Those born without it struggle to catch up.
- Personal freedom is limited by financial circumstance.
- Creativity is stifled by the need to survive.
- The gap between rich and poor grows with each generation.

Existing approaches to Universal Basic Income (UBI) attempt to solve this by distributing tokens — but they are particular solutions with particular trade-offs. Circles UBI uses personal currencies and trust graphs — an elegant model for social accountability, but one that creates community lock-in and penalizes the socially disconnected. Government UBI programs depend on political will and centralized distribution. No existing system treats fair distribution as a *first principle of money itself*.

## The Two Guarantees

A fair economic system should guarantee two things:

1. **Every individual has access to the essential resources needed to not just survive, but to thrive.** This is a birthright, not a reward.

2. **Genuine contributions that improve the well-being of others are recognized and rewarded fairly.** This is accountability, not charity.

BURST encodes both guarantees into a single protocol. The first is represented by BRN — a deterministic income that accrues for every verified human. The second is represented by TRST — a transferable currency minted when someone provides value to others.

## BURST in 30 Seconds

- You get a wallet. Money (BRN) accumulates in it automatically, at the same rate as every other human on the planet.
- When you buy something, your BRN is spent and the vendor receives TRST — a tradeable currency.
- TRST can be spent like normal money, saved, or traded.
- TRST eventually expires. How long it lasts is decided by a democratic vote.
- If the expiry is set very long, TRST works like normal money. If set very short, it works like reputation points. Society chooses.
- The system can't be cheated because one person can only have one wallet, enforced by decentralized verification.

That's it. Everything below is how it works.

## Three Novel Contributions

BURST introduces three ideas not found in any existing system:

1. **Universal income as a computed counter, not a distributed token.** Every other UBI system distributes tokens — minting and sending them periodically. BURST eliminates distribution entirely. BRN is a deterministic function of time. It doesn't exist on the ledger until it's spent. Zero infrastructure for the most fundamental operation.

2. **Parameterized money — the general equation.** Two parameters (BRN rate and TRST expiry) span the entire economic spectrum. Normal money is the special case where rate = 0 and expiry = never. No other monetary protocol has this property — every other system is a particular solution. BURST is the general form.

3. **Symbiotic security between identity and currency.** In every other system, identity verification and currency are separate concerns. In BURST, verification determines who produces BRN, and BRN powers verification. The two systems are structurally interdependent — the integrity of one is the integrity of the other. This circular reinforcement is a designed feature, not an accident.

---

# Part 2 — The General Equation

## Parameterized Money

BURST is defined by two parameters:

- **r** — the BRN accrual rate (how fast universal income accumulates)
- **e** — the TRST expiry period (how long earned currency remains transferable)

Every economic model is a configuration of these two values:

| TRST Expiry (e) | BRN Rate (r) | Economic Model |
|---|---|---|
| Never | 0 | **Normal money.** No UBI, no expiry. Just a cryptocurrency. |
| Never | > 0 | **Capitalism + UBI.** Inflationary (no TRST leaves circulation). |
| Very long | > 0 | **Capitalist-compatible UBI.** TRST works like familiar money with minimal depreciation. |
| Long | > 0 | **Balanced mode.** UBI with moderate value demurrage. |
| Short | > 0 | **Reputation economy.** TRST is accountability, not wealth. |
| Zero | > 0 | **Full communism.** TRST expires instantly. No tradeable wealth. Everyone has only BRN (equal for all). |

Normal money is a special case of BURST — specifically, the case where r = 0 and e = ∞. This is not a metaphor. It is structurally true. BURST does not claim to model every financial instrument — credit, debt, interest rates, and derivatives are built on top of base money, not properties of it. What BURST generalizes is the *base layer*: how money is created, distributed, and retired. No other protocol parameterizes this layer.

Every configuration is reachable from any other via democratic vote. The protocol never changes — only the parameters do. Any choice is reversible.

## Adoption Without Ideology

This generalization solves the cold start problem that kills most alternative currencies.

BURST does not need to be adopted *as a UBI system*. It can launch as normal money — BRN rate set to zero, expiry set to never. Vendors accept it because it's just money. Users adopt it because it's just money. There is nothing alien about it.

Then, democratically, the community can vote to:

1. Turn on BRN accrual (activate universal income)
2. Set an expiry period (activate value demurrage)
3. Adjust over time as society evolves

The UBI features emerge from democratic choice, not from asking people to adopt something unfamiliar. No one gives up what they have. They get what they have, plus the option for more.

A concern: what if early adopters who joined for normal money resist the transition to UBI? This is self-selecting — there is no incentive to use BURST over existing money unless you understand and support its broader vision. People who want normal money already have normal money. BURST's community will be composed of people who want the *option* of UBI, even if they don't activate it immediately. And if the community genuinely splits on the question, the protocol can fork — same as any open-source project. The transition is not forced. It's voted on by the people who chose to be here.

## The Economic Spectrum

BURST doesn't pick a position on the economic spectrum. It builds the spectrum itself and says: *you decide*.

The same protocol, without modification, can serve:

- A libertarian who wants pure free-market money with no UBI
- A social democrat who wants UBI alongside a market economy
- A post-scarcity community that wants reputation-based economics
- And everything in between

No other base-layer monetary protocol has this property. Every other system is a particular solution — a single point in the parameter space. BURST is the space itself.

## Not Proposing, Preparing

BURST does not advocate for any specific societal, governmental, or economic changes. It does not claim that any position on the spectrum is superior. It exists as infrastructure — so that when humanity decides what it wants, the distribution mechanism is ready.

---

# Part 3 — Architecture

## The Two-Token System

BURST consists of two components:

**BRN (Burn)** — the birthright. Not a token. A computed counter.

**TRST (Trust)** — the reward. An actual transferable crypto token.

This separation is the foundation of the entire system. It is not an implementation detail — it is the core design insight.

### BRN: The Birthright

BRN represents universal income. Every verified wallet accrues BRN at the same rate:

```
BRN(w) = r × (t_now - t_verified(w)) - total_burned(w)
```

Where:
- `r` is the accrual rate (a protocol parameter, democratically set)
- `t_verified(w)` is the timestamp when wallet `w` was verified as a unique human
- `total_burned(w)` is the cumulative BRN burned by this wallet

Properties:

- **Not a token.** BRN does not exist on the ledger. It is a deterministic function of time. Any node can independently compute any wallet's BRN balance at any moment, as long as clocks are synchronized. There is no minting, no distribution, no periodic payouts.
- **Non-transferable.** BRN cannot be sent to another wallet. It can only be burned to create TRST.
- **Does not expire.** Demurraging a birthright would incentivize unnecessary spending. Since everyone accrues at the same rate, expiring BRN cancels itself out mathematically while adding harmful behavioral incentives.
- **Equal for everyone.** Every verified human gets the same rate. No exceptions. No bonuses. No penalties.
- **Rate changes split counting.** If the community votes to change the BRN rate from `r1` to `r2`, accrual is split at the change point: BRN accumulated before the change stays at `r1 × time_at_old_rate`, and new accrual continues at `r2`. No retroactive recalculation. This is a straightforward implementation detail — the formula becomes `r1 × (t_change - t_verified) + r2 × (t_now - t_change) - total_burned`.
- **Used for staking.** BRN is staked (temporarily locked) in humanity verification voting and in challenging bad actors. Staked BRN cannot be burned during the lock period. It is returned after the process completes — unless the staker voted against the outcome, in which case it is forfeited.
- **Represents production potential.** BRN balance defines how much TRST a wallet is eligible to produce.

### TRST: The Reward

TRST is the actual currency of the ecosystem. It is created when BRN is burned:

```
burn(consumer, provider, amount) → mint(provider, amount TRST)
```

A consumer burns `amount` BRN from their wallet. The provider receives `amount` freshly minted TRST. The ratio is always 1:1.

Properties:

- **Transferable.** TRST can be sent between wallets. This is the money.
- **Transaction-stamped.** Every TRST token carries the complete history of its creation and all subsequent transfers.
- **Has an expiry date.** Measured from the timestamp of the original burn transaction (the origin). After the expiry period `e` has elapsed, the TRST becomes non-transferable.
- **Can be split and merged.** A batch of TRST can be divided into smaller amounts or combined with other TRST (see TRST Lifecycle below).

### Why Separation Matters

Most UBI systems use a single token for both income and currency. This creates cascading problems: the token must be distributed (requiring infrastructure), it conflates entitlement with earnings (preventing meaningful economic signals), and it requires demurrage on the birthright itself (incentivizing waste).

BURST's separation eliminates all three:

1. **No distribution infrastructure.** BRN is computed, not distributed. Zero overhead.
2. **Clear economic signal.** BRN is what you're given. TRST is what you've earned. The distinction is permanent and visible.
3. **Targeted demurrage.** Only TRST (earned excess) undergoes value demurrage. BRN (birthright) is never penalized. You're never punished for existing.

## Transaction Architecture

Every transaction in BURST is recorded with the following fields:

### Burn Transaction (BRN → TRST)

```json
{
    "method": "burn",
    "sender": "brst_1l8j…",
    "receiver": "brst_eliesh…",
    "amount": 50,
    "timestamp": 1740914000,
    "hash": "A1E5…",
    "signature": "9a8b7c…"
}
```

| Field | Description |
|---|---|
| `method` | `"burn"` — BRN is consumed, TRST is minted |
| `sender` | Wallet address of the consumer |
| `receiver` | Wallet address of the provider |
| `amount` | Quantity of BRN burned / TRST minted |
| `timestamp` | Time of creation — determines TRST expiry |
| `hash` | Unique identifier for this transaction |
| `signature` | Cryptographic signature of the sender |

### Send Transaction (TRST → TRST)

```json
{
    "method": "send",
    "sender": "brst_eliesh…",
    "receiver": "brst_3v9k…",
    "amount": 50,
    "timestamp": 1740914040,
    "hash": "B5I9…",
    "link": "A1E5…",
    "origin": "A1E5…",
    "signature": "6d5e4f…"
}
```

Two additional fields appear:

| Field | Description |
|---|---|
| `link` | Hash of the immediately preceding transaction this TRST was derived from |
| `origin` | Hash of the original burn transaction that created this TRST |

The `origin` field enables:
- **O(1) expiry check** — read the origin's timestamp, compare to expiry threshold
- **Instant originator identification** — know which wallet burned the BRN
- **Legitimacy verification** — if the originating wallet is later found illegitimate, all downstream TRST can be revoked

The `link` field enables:
- **Complete backward traversal** — trace the full transaction history of any TRST

For the first send after a burn, `link` and `origin` are the same (both point to the burn transaction). For subsequent sends, `origin` is copied forward while `link` is updated to the most recent transaction's hash.

## TRST Lifecycle

### Creation

A consumer burns X BRN. The provider receives X TRST. The burn transaction becomes the `origin` for all TRST derived from it. The timestamp of the burn determines the expiry date.

### Transfer

TRST can be sent to any wallet. The receiver can choose to accept or reject the TRST. Each transfer creates a new transaction with `link` and `origin`.

### Splitting

Splitting is the default form of transfer, since amounts rarely match exactly. When 50 TRST is split into 30 and 20, all splits share the same `origin` and `link` from the parent transaction. Only the `amount` varies. The sum of all splits always equals the parent amount.

### Merging

Different TRST tokens may have different origins (and therefore different expiry dates). Merging combines them into a single batch. The merged token's expiry date is the **earliest expiry** among all merged tokens — a conservative rule that protects against hidden expiry.

The merged token's amount equals the sum of all merged amounts. Future transactions from the merged token use the merge transaction's hash as the new origin.

Merging is a **wallet-level process** — the wallet software handles it automatically by default, grouping tokens with similar expiry dates to maximize retained value. Users don't need to think about expiry-date optimization. The wallet does it for them, the same way a modern email client sorts your inbox without asking.

Because not all TRST is valued equally (different origins, different expiry dates, different histories), there is a possibility of speculative arbitrage around merge timing. This is acknowledged — it's an inherent property of any system where tokens have unique histories, and it mirrors how bond markets price instruments with different maturities.

### Expiry

When the time elapsed since the origin's timestamp exceeds the expiry threshold `e`, the TRST becomes **expired**:

- Still visible in the wallet and on the ledger — nothing disappears
- Non-transferable (cannot be sent to anyone)
- Serves as **virtue points** — a permanent, visible record of contribution to the economy

Expired TRST is not destroyed. It transitions from currency to reputation. Your balance doesn't shrink — the transferable portion does. The total remains as proof of everything you've earned and provided.

### Revocation

If a wallet is found to be **fraudulent** (not a unique human), all TRST originating from that wallet becomes **revoked** — immediately non-transferable, regardless of expiry.

For merged tokens containing revoked TRST: proportional splitting occurs. All current holders lose TRST in the same ratio, ensuring fairness. The merger graph enables this splitting.

### Unverification (Without Revocation)

Not all illegitimate wallets are fraudulent. A wallet whose holder has died, or which has been inactive beyond what the community deems acceptable, can be **unverified** — BRN accrual stops and the wallet loses transaction rights — but the TRST it originated is **not revoked**. The holder was a real person who legitimately earned that TRST. The community defines what counts as grounds for unverification (death, prolonged inactivity, etc.) through the Consti. This is a distinct operation from fraud revocation.

### The Merger Graph

The normal transaction chain is backward-linked — any TRST can trace back to its origin via the `link` field:

```
current_tx --link--> prev_tx --link--> ... --link--> origin (burn)
```

The merger graph is the **inverse index**. It maps from origins forward to every merge that consumed them, then to every subsequent merge, all the way to current live balances:

```
origin (burn) --> [merges containing it] --> [merges of merges] --> ... --> current balances
```

It is the same relationship as the `link`/`origin` chain, traversed in the opposite direction. Origin-keyed instead of holder-keyed.

This enables **proactive invalidation** — a critical performance property:

- **Without the merger graph (reactive):** Every transaction would require the receiving node to trace backward through the merge tree and check every constituent burn origin for revocation. The merge tree branches at each level — a merge of merges of merges can have arbitrarily many leaf origins. Since the revoked origin could be in any branch, no branch can be skipped. This is **O(n)** per transaction, where n is the total number of original burn transactions that contributed to the token. Every spend pays this cost, for fraud that may never have happened.

- **With the merger graph (proactive):** When a bad actor is caught, nodes traverse the merger graph *forward* from the illegitimate origin to every merge that consumed it, through every subsequent merge, to every current live balance containing tainted TRST. Proportional splitting happens immediately, at catch time. After that, every subsequent transaction is a simple **O(1)** validity check — the revocation has already been applied.

The cost of fraud is paid once, at catch time, by the network — not repeatedly, at every transaction, by every participant. Invalidation scales with the frequency of fraud, not with the frequency of transactions.

## Value Demurrage

BURST uses **value demurrage**, not quantity demurrage.

- **Quantity demurrage** (Circles UBI): The protocol reduces your token balance over time. You literally have fewer tokens.
- **Value demurrage** (BURST): Your token quantity stays the same. The *market value* naturally decreases as expiry approaches. 50 TRST is always 50 TRST on the ledger — but 50 TRST expiring tomorrow is worth less than 50 TRST expiring in 30 years.

The protocol does not enforce devaluation. The market does. This is more natural, more efficient, and preserves honest accounting — the number in your wallet is always truthful.

### TRST Velocity

Expiry incentivizes spending — TRST near expiry is worth less, so holders prefer to use it sooner. This creates high velocity (money changing hands frequently). In a high-velocity regime, TRST values tend to stabilize because the market quickly converges on consistent pricing — there's no incentive to hold and wait. Wallet software can facilitate this by uniformly agreeing on a valuation curve (value as a function of time-to-expiry), making pricing predictable for both consumers and providers. Sophisticated market strategies for TRST valuation will emerge, but the protocol doesn't need to prescribe them — the market handles it.

---

# Part 4 — Security and Verification

## Proof of Unique Humanity

The foundational rule of BURST: **one person, one wallet.**

If this rule holds, BRN accrues fairly (everyone gets the same), voting is democratic (one person, one vote), and the system is secure. If it doesn't hold, everything collapses. This is the hardest problem in the system, and BURST is honest about that.

BURST uses a two-phase verification process with economic incentives designed to make fraud expensive.

## Endorsers

Endorsers are people in the wallet holder's social circle — family, friends, colleagues — who personally vouch for their humanity.

- Endorsers **permanently burn** their own BRN to endorse a new wallet
- This is not a temporary stake — the BRN is gone forever
- The cost is real, making false endorsement expensive
- Once enough endorsers have vouched (the endorsement threshold), the wallet enters the verification phase
- For children, initial endorsers can be parents and relatives. Parents act as custodians of the child's wallet — they can manage it (including burning BRN) until the child is capable of using it independently, at which point the child assumes ultimate authority over their own wallet

## Verifiers

Verifiers are randomly selected from the pool of verified wallets that have opted in to verify others.

- Verifiers **temporarily stake** their own BRN to participate
- They independently assess whether the wallet holder is a unique human
- They vote: **Legitimate**, **Illegitimate**, or **Neither**
  - Legitimate/Illegitimate votes require staking BRN
  - Neither counts as an illegitimate vote but requires no stake (and forfeits rewards)
  - Voting Neither excessively incurs penalties

### Verification Process

1. Wallet holder prepares proof of unique humanity (the burden of proof is on them)
2. Endorsers burn BRN to vouch for the wallet
3. Once the endorsement threshold is met, verification begins
4. Verifiers are randomly selected and vote
5. **If the verification threshold is reached (~90% legitimate)**:
   - Wallet is verified
   - Endorsers are rewarded
   - Dissenters (those who voted against) lose their staked BRN
6. **If the threshold is not reached**:
   - A revote occurs with new verifiers (same rules)
   - Revotes continue until an algorithmic limit is reached
7. **If still not verified after maximum revotes**:
   - Endorsers lose their burned BRN (already gone)
   - Dissenters are rewarded

### Verifier Integrity

A concern: what if verifiers rubber-stamp wallets as legitimate without actually checking? This would degrade verification quality silently.

The defense is structural and game-theoretic. The verification threshold is high (~90%), so rubber-stamping only works if nearly all verifiers collude simultaneously — among randomly selected strangers. More importantly, rubber-stamping creates a self-defeating incentive: every fake wallet that passes verification produces BRN that dilutes everyone else's share. If verifiers rubber-stamp fake wallets, they are reducing the value of their own BRN. It's a form of selfish jealousy that, if everyone does it, leaves everyone worse off. The challenge mechanism provides a correction — any wallet that slipped through can be caught later, and the challenger is rewarded for catching it.

## The Symbiotic Security Loop

The verification system determines who produces BRN. BRN powers the verification system. This creates a self-reinforcing feedback loop:

1. Fair verification → only real humans produce BRN → BRN is distributed equally
2. Equal BRN distribution → voting power is decentralized → verification remains fair
3. The two systems strengthen each other continuously

Unlike systems where identity and currency are separate concerns (Worldcoin verifies identity, then distributes tokens — the two don't interact), BURST's identity system and currency system are mutually dependent. The security of one is literally the security of the other.

## Modular Verification

The method for proving unique humanity is **not baked into the BURST protocol**.

This is a deliberate design choice. The protocol specifies that verification must happen — endorsers must vouch, verifiers must vote — but it does not specify *how* the wallet holder proves their humanity or *how* verifiers assess the evidence.

This means BURST can accommodate:

- Its native endorser + verifier model
- Trust graphs (like Circles UBI)
- Biometric verification (like Worldcoin)
- Government ID verification
- Composable identity (multiple weak signals combined, like Gitcoin Passport)
- Methods that don't exist yet

Different communities can use different verification methods within the same protocol. A university campus might use student IDs. A nation might use national identity systems. A crypto-native community might use trust graphs. The protocol doesn't care *how* — only *that* the verifiers voted legitimate.

This makes BURST resilient to the advancement of fraud technology (deepfakes, synthetic identities, AI-generated evidence). When one method is defeated, another can be adopted — without changing the protocol.

## Group Trust Layer

Beyond protocol-level verification, BURST supports an optional **group trust layer** — an additional line of defense that operates entirely off-chain, at the application level.

### Three Layers of Verification

1. **Protocol baseline (on-chain).** The endorser + verifier system. Every wallet goes through it. Sybil attacks are expensive, risky, and recoverable via revocation.

2. **Origin checking (person-level).** Every TRST token carries metadata about who created it. A receiver can inspect the originator and make person-level trust decisions — identical in granularity to Circles UBI's trust graph, but with one universal currency instead of personal currencies.

3. **Group trust (scalable convenience).** Instead of checking every originator yourself, you trust a group — any organization that vouches for its members' humanity. A local community, a professional association, an online DAO, a verification service. The group manages internal verification however it wants (in-person, biometrics, ID, social vouching). The receiver's wallet queries the group: "Is this originator a member in good standing?" The group responds. Nothing on-chain. The node has no concept of groups.

### Groups as Trust Graphs

A group is a trust graph node at a higher level of abstraction. Instead of person→person→person (Circles' mesh model), it's person→group→person (hub-and-spoke). Groups can vouch for other groups, providing multi-hop trust transitivity at the group level — the same property that makes Circles' trust graph powerful, but at a granularity that scales.

This design achieves the trust-graph benefits without personal currencies. Personal currencies create community lock-in (your wealth is trapped in the community's trust circle), irrecoverable reputation damage (tainted group currency can't be "whitewashed" by other groups — honest members are stuck), and punishment for trusting the system (accepting a sybil's personal currency costs you money). BURST avoids all of this: TRST is universal, groups are informational rather than economic, and the protocol's revocation mechanism handles sybil damage without tarring innocent participants.

## Handling Bad Actors

Even after verification, any verified wallet can challenge another by staking BRN:

1. Challenger stakes BRN and initiates a revote
2. New verifiers are randomly assigned to review the wallet
3. Verifiers independently assess the evidence
4. **If found to be fraud**:
   - Wallet is unverified — loses all BRN accrual and transaction rights
   - All TRST originating from that wallet is immediately **revoked**
   - For merged tokens containing revoked TRST: proportional splitting
   - Challenger is rewarded in TRST
   - Verifiers who voted correctly are rewarded
5. **If not fraud**: Challenger loses their staked BRN

This creates powerful deterrence:

- Fraud is never safe, even after initial verification — anyone can challenge at any time
- The economic reward for catching fraud incentivizes vigilance across the entire network
- TRST holders are incentivized to do their own due diligence on who they transact with
- The cost of maintaining a fake wallet (risk of losing all originated TRST) exceeds the benefit

### The Fungibility Trade-off

The revocation mechanism means that TRST from unknown wallets carries more risk than TRST from well-established ones. This is intentional. It incentivizes participants to verify who they're transacting with, which strengthens the overall security of the network. If accepting TRST were risk-free regardless of origin, there would be no economic pressure to catch fraud.

This does reduce fungibility — not all TRST is equally "safe" to accept. In practice, this is mitigated by the same tools that the modular verification architecture enables: trust graphs, reputation services, and automated origin-checking tools can fast-track acceptance of TRST from verified sources. The wallet software can integrate these checks transparently, so the user experience remains simple while the security benefit is preserved.

The trade-off is explicit: BURST prioritizes network security over perfect fungibility. A currency where fraud has no consequences is not more useful — it's more vulnerable.

## Genesis: The Identity Bootstrap

The endorser/verifier loop requires verified wallets to verify new wallets. This creates a chicken-and-egg problem: who verifies the first wallets?

Every decentralized system has a genesis moment that is, by necessity, centralized. Bitcoin has a genesis block mined by Satoshi. Ethereum had a pre-sale. BURST has a genesis set: the first wallets are endorsed directly by the protocol's creator.

This is transparent, not hidden. The creator's wallet, endorsement transactions, and all subsequent activity are permanently visible on the ledger. The creator's incentives are aligned — BURST has no pre-mine, no token sale, no profit mechanism. The creator earns BRN at the same rate as everyone else. If the genesis endorsements were fraudulent, the entire project fails, and the creator gains nothing.

As the network grows, the genesis set becomes irrelevant. The endorser/verifier loop sustains itself. The creator's initial authority is diluted by every new verified wallet. Within a few hundred wallets, the genesis set is a rounding error. Within a few thousand, it's a historical footnote.

This is the same trust model as any open-source project: you trust the initial commit because the creator's reputation is on the line, and you verify everything after that independently.

## Sybil Tolerance

BURST is honest about this: **no system has solved the one-person-one-wallet problem perfectly.** Worldcoin, Proof of Humanity, Circles, BrightID — all have been gamed. BURST does not claim to solve sybil. It makes sybil attacks expensive, risky, and recoverable.

### Why Sybil Is Expensive

Creating a fake wallet requires clearing multiple economic hurdles, each of which costs real resources:

1. **Endorsement cost.** Real people must permanently burn their own BRN to endorse the fake wallet. This BRN is gone forever — not staked, not recoverable. The endorsers are putting their own production potential on the line. If the wallet is later found fraudulent, the endorsers gain nothing (their BRN is already burned) and lose reputation.

2. **Verification cost.** Randomly selected verifiers independently assess the wallet. The attacker cannot choose who verifies — the selection is random (VRF-based). To pass verification, the attacker must convince a supermajority of strangers that the wallet is legitimate.

3. **Ongoing risk.** Even after passing initial verification, the fake wallet is never safe. Any verified wallet can challenge it at any time by staking BRN. If the challenge succeeds, the fake wallet is unverified, all TRST it originated is revoked (including proportional splitting of merged tokens), and the challenger is rewarded. The longer a fake wallet operates, the more TRST it originates, and the larger the bounty for catching it.

4. **Collusion cost.** Coordinated sybil (multiple fake wallets endorsing each other) requires burning BRN from *real* wallets to bootstrap the first fakes. Each layer of the scheme multiplies the cost. The cost scales with the number of fake wallets, while the benefit (BRN accrual) scales linearly — the economics get worse, not better, at scale. Additionally, the creator will personally curate the first several thousand wallets during the genesis phase, preventing collusion rings from forming in the critical early period when the network is most vulnerable. As the network grows, the challenge mechanism and modular verification methods take over.

### Why Sybil Is Recoverable

Unlike systems where sybil damage is permanent, BURST has a rollback mechanism: TRST revocation. When a fake wallet is caught, every TRST token that originated from it is revoked — even tokens that have been transferred, split, or merged. The merger graph enables proportional splitting of merged tokens. The economic damage of sybil is undone retroactively. This means that even a successful sybil attack is temporary: the community can detect and reverse it after the fact.

### Modular Defense

The method for proving unique humanity is not baked into the protocol. When one verification method is compromised (deepfakes defeat video calls, or synthetic IDs defeat document checks), the community can adopt a different method — without changing the protocol. This means BURST's sybil resistance evolves with the threat landscape. Trust graphs, biometrics, composable identity signals, and methods that don't exist yet can all be plugged in.

### The Tolerance Question

The relevant question is not "can sybil be eliminated?" but "how much sybil can the system tolerate while still functioning?" If 2% of wallets are fake, the economic model likely survives — the extra BRN accrual is noise. If 20% are fake, it likely doesn't — the UBI floor becomes meaningless if a fifth of recipients are ghosts. There is a threshold somewhere. Finding that threshold through economic simulation is critical future work.

---

# Part 5 — Infrastructure

## Feeless DAG Architecture

BURST targets a **feeless DAG (Directed Acyclic Graph)** architecture, modeled after Nano's block-lattice:

- **Each account has its own chain.** No global chain. No blocks containing everyone's transactions.
- **Transactions are asynchronous.** No global ordering. Accounts update independently.
- **No transaction fees.** The system is feeless by design. Anti-spam is handled through lightweight proof-of-work per transaction (following Nano's approach).
- **Sub-second confirmation.** Consensus via representative voting (similar to Nano's Open Representative Voting).
- **Minimal resource requirements.** Designed to run on low-power hardware.

Why DAG over blockchain:

- No miners, no staking for block production — decentralization is not compromised by economies of scale
- Feeless transactions enable microtransactions and daily use as actual money
- Each account's chain can be pruned independently
- Scales with number of accounts, not number of transactions

### Clock Synchronization

BRN is a function of time, so nodes must agree on the current time. BURST uses UTC as the global reference. Nodes sync via standard protocols (NTP or equivalent). A minimal margin of error is ignored — BRN values are rounded for uniformity. Peer-to-peer latency between nodes is handled with a threshold: a transaction's timestamp must fall within an acceptable window of the receiving node's local time. If it doesn't, it's rejected until consensus is reached. This is not fundamentally different from how any distributed system handles time — the precision required for BRN computation (seconds-level, not milliseconds) is well within what NTP provides.

### Anti-Spam

In a feeless system, transaction spam is a real threat. BURST uses the same approach as Nano: a small proof-of-work per transaction. This is not mining — it's a lightweight computational cost (fractions of a second) that makes flooding the network with millions of transactions prohibitively expensive while keeping legitimate use free. Transactions are prioritized by account balance and PoW difficulty. This mechanism is well-tested in Nano's production network.

## VRF for Verifier Selection

BURST needs a way to randomly and verifiably select verifiers for humanity verification. This requires a **Verifiable Random Function (VRF)** — a cryptographic primitive that produces a random output anyone can verify was correctly generated.

The core challenge: VRF typically needs a shared seed that all participants agree on. In a linear blockchain, this is the previous block hash. In a DAG, there is no single "previous state."

**This is solvable.** BURST's use case (infrequent verifier selection, not continuous block production) makes it significantly more tractable than general-purpose VRF in a DAG.

### Phased Approach

**Phase 1 — Bootstrap (MVP):** Use **drand**, an operational decentralized randomness beacon run by the League of Entropy (18+ organizations including Cloudflare and Protocol Labs). It emits publicly verifiable random values every 30 seconds. Integration requires minimal code. This lets BURST launch while the harder solutions are developed.

**Phase 2 — Self-Sovereign Randomness:** Implement **commit-reveal with representatives**. When verification is needed, representatives commit hashed random values, then reveal them. Combined outputs produce the seed. Slashing penalties for non-reveal mitigate the last-revealer problem. No external dependency.

**Phase 3 — Gold Standard:** Implement **threshold VRF (DVRF)** — a multiparty protocol where a committee collectively produces randomness. No single participant can predict or bias the output. Strongest possible guarantees, justified when the network is mature enough for the complexity.

## Democratic Governance

BURST implements democratic governance modeled after Tezos's self-amendment, adapted for a DAG architecture. The foundation of this governance is simple: Unique Humanity Verification guarantees one person = one wallet, which guarantees one person = one vote. From this, everything follows.

### Protocol Parameters

All numeric protocol parameters are stored in every BURST node and are directly governable through the 4-phase voting process:

- TRST expiry period (`e`)
- BRN accrual rate (`r`)
- Endorsement and verification thresholds
- Penalty amounts
- Spending limits for new wallets
- Governance parameters themselves (supermajority threshold, quorum, phase durations)

These are on-chain state — every node knows the current values, and parameter changes propagate automatically after activation.

### The Consti (On-Chain Constitution)

The Consti is a separate ledger — distinct from both the transaction ledger and the protocol parameters — for governance that can't be reduced to numbers. It is a human-readable constitution: a space where participants propose, discuss, and vote on what is fair, what is unfair, what constitutes legitimate behavior, and how the community should align.

Protocol parameters define *how much* and *how fast*. The Consti defines *what* and *why*: what it means to be a legitimate participant, what constitutes fraud, what standards of evidence are acceptable for verification, what rights and responsibilities participants have. These are questions of human judgment, not numerical tuning.

The Consti uses the same 4-phase governance mechanism as parameter changes, but can have its own separate threshold — potentially higher or lower than the parameter threshold. The Consti threshold is changed by hitting that same Consti threshold (not the parameter threshold). This allows the community to treat constitutional amendments with different gravity than numerical tuning. It lives on its own ledger because it is not transactions and it is not parameters — it is the social contract within which both operate.

Because BURST's governance is global (one person = one vote, regardless of nationality), the Consti is not tied to any nation, government, or jurisdiction. It starts with the rules of the BURST protocol — definitions of legitimacy, standards of evidence, rights and responsibilities of participants. Where it goes from there is up to the community. The infrastructure is general enough that it *could* extend beyond protocol governance, but that's a consequence of the design, not a goal.

### Interpretation and Ambiguity

Numeric parameters are unambiguous — either the expiry is 100 years or it isn't. Constitutional text is not. "What constitutes fraud" and "what standards of evidence are acceptable" require human judgment, and two participants may interpret the same rule differently.

BURST already has a mechanism for resolving disagreement: voting. When verifiers disagree about whether a wallet is legitimate, the majority vote decides. The same principle applies to the Consti — disputes about interpretation are resolved by the same democratic process. If a rule is ambiguous enough to cause recurring disputes, participants can propose a clarifying amendment through the standard governance process.

The Consti can also develop its own interpretation mechanisms over time — designated interpreters, precedent systems, advisory votes — through the same governance process that creates every other rule. The protocol provides the voting infrastructure. How the community builds interpretive norms on top of it is itself a governance decision.

### Governance Process

**Phase 1 — Proposal:** Any verified wallet submits a proposal transaction to the DAG (e.g., "change TRST expiry from 100 years to 80 years"). The proposal needs N endorsements (BRN burns) to advance, preventing spam.

**Phase 2 — Voting:** Every verified wallet gets one vote — a transaction on the DAG. If a wallet has delegated its vote, the delegate votes on its behalf. A supermajority and a quorum are required to pass.

**Phase 3 — Cooldown:** No voting. The community discusses and prepares for the change.

**Phase 4 — Activation:** At a deterministic future timestamp, all nodes apply the new parameter. Every transaction confirmed after that timestamp uses the new value.

All durations (proposal window, voting window, cooldown period), the supermajority threshold, and the quorum requirement are themselves governable parameters.

### Delegation

Most people won't vote on every parameter change. Delegation lets them entrust their vote to a representative:

1. The wallet generates a secondary key pair for delegation
2. The delegation private key is encrypted with the delegate's public key
3. The encrypted key is broadcast to the network
4. The delegate can now vote on behalf of the wallet
5. Delegation can be revoked at any time by the wallet owner broadcasting a new delegation key, signed by their primary private key

### Voting Properties

- **One wallet = one vote.** Not stake-weighted. Democratic, not plutocratic.
- **Delegation is always revocable.** No permanent power transfer.
- **DAG-compatible.** Phases are time-based (clock sync already required for BRN computation). A propagation buffer between voting end and counting prevents edge cases.
- **Reversible.** Any parameter change can be reversed by a subsequent vote.
- **Self-governing thresholds.** The supermajority threshold itself (whether 66%, 80%, or 90%) is a protocol parameter — it can be changed by the same governance process. The community decides not just the rules, but how hard the rules are to change. A higher threshold means more stability; a lower one means more agility. The initial value is set conservatively (high), and the community can adjust it as the network matures.
- **No hard constitutional limits.** A concern: what if the majority votes to lower the threshold, then abuses the lower bar? This is a real tension, but BURST's answer is deliberate: the community decides everything, including how hard the rules are to change. Hardcoding limits contradicts the "society decides" principle. The defense is pragmatic — a supermajority set high enough (e.g., 90%) makes destructive coordination extremely difficult, and the cooldown phase gives dissenters time to mobilize. If a supermajority genuinely wants to change the system, that *is* the democratic outcome — even if observers disagree with it.

### Future: Full Self-Amendment

Initially, only parameter changes are governed on-chain. Code-level protocol upgrades use traditional software updates. As the system matures, the governance mechanism can be extended to full self-amendment (Tezos-style) — the community can vote to upgrade the protocol itself.

## Wallet and Node Design

### Wallet

- Transacts in both BRN and TRST
- Requires passing Unique Humanity Verification
- BRN balance computed deterministically as `r × (t_now - t_verified) - total_burned`
- Key pairs:
  - Primary key pair (identity and transaction signing)
  - Secondary key pairs (delegation, voting)
- Wallet addresses prefixed with `brst_`
- Displays: BRN balance, transferable TRST, expired TRST (reputation), revoked TRST

### Node

- Synchronizes clocks with other nodes (required for BRN computation)
- Validates transactions (checks BRN production potential, balance, signatures)
- Facilitates humanity verification
- Coordinates governance voting
- Acts as delegate for wallets that have delegated
- Maintains the merger graph for TRST splitting on revocation

---

# Part 6 — Economics

## Market-Based Valuation

The BURST protocol does **not** determine the value of TRST. The market does.

All transaction history is public. Third-party tools and services can assess TRST value based on:

- **Time until expiry** (newer is generally more valuable)
- **Transfer chain length** (shorter chain is generally more valuable)
- **Originator** (who burned the BRN)
- **Legitimacy** of all wallets in the chain
- **Transfer frequency** and patterns
- **Amount**

Each TRST batch with a particular transaction history can be thought of as analogous to an NFT — different instances of the same thing, valued differently by the market based on their unique properties.

Valuation is off-chain because:
- It saves computational resources of the network
- Different people can use different valuation methods
- The market can evolve valuation approaches independently
- No single point of failure in methodology

## Natural Equilibrium

Two opposing forces create a natural equilibrium:

1. **Consumers want to undervalue TRST** — this increases their BRN purchasing power
2. **But they can't undervalue too much** — providers won't provide goods and services if compensation is insufficient

The result: consumers undervalue TRST just enough that providers still find it worthwhile to work. This is the same dynamic that exists in any market economy, with the additional floor that everyone has BRN regardless.

### Boundary Conditions

The equilibrium argument assumes a functioning market. It's worth examining what happens when that assumption breaks:

- **Providers collectively refuse TRST.** If every provider demands fiat instead, TRST has no utility and the economy collapses. This is the adoption problem, not an equilibrium failure — it can only happen before TRST achieves critical mass. The bootstrapping path (launch as normal money, introduce UBI gradually) is designed to prevent this scenario from ever arising. Once a provider accepts TRST from one customer, the competitive pressure to accept it from all customers follows naturally.
- **Consumers hoard BRN indefinitely.** BRN is non-transferable and only converts to TRST via burning. If consumers never burn, no TRST enters circulation, and the economy stalls. But this is self-correcting: hoarding BRN means forgoing consumption, which has a real cost. The first person to break the hoarding equilibrium and burn BRN gains a first-mover advantage in a market with unmet demand. Game-theoretically, universal BRN hoarding is not a stable equilibrium.
- **Economy is too small for competitive dynamics.** In a small community with few providers, the competitive pressure that drives TRST acceptance is weak — a monopolist provider can refuse TRST with impunity. This is a real constraint on early adoption. BURST works best in economies large enough for competition. In small communities, social pressure (rather than market pressure) must do the work, or BURST must operate in normal-money mode (r=0, e=∞) until the community grows.

## Inflation Management

**The problem:** New BRN is continuously created (every wallet accrues over time). Without deflation of TRST, too much money chases too few goods — hyperinflation — BRN becomes worthless — the purpose of UBI is defeated.

**The solution:** TRST expiry. New BRN creation is offset by TRST expiry. At equilibrium, the rate of BRN entering the system roughly matches the rate of TRST leaving it (through expiry). Only TRST (earned excess) undergoes value demurrage — BRN (birthright) does not. This preserves BRN purchasing power while preventing unbounded wealth accumulation.

### Why Not Expire BRN?

If BRN expired, people would rush to spend it before it disappeared — incentivizing unnecessary purchases. Since everyone receives BRN at the same rate, expiring it at the same rate cancels out mathematically but adds harmful behavioral incentives. BRN is not currency — it is production potential. There is no reason to expire potential.

## Vendor Adoption

Why would vendors accept TRST?

1. **Long expiry minimizes risk.** With expiry set to 80–200 years, TRST depreciates negligibly per year — more stable than many real-world inflationary currencies.

2. **Early adopters benefit from ecosystem growth.** If BURST adoption grows, the value of TRST holdings increases. Even after minor depreciation, net gains from growth can be positive.

3. **Consumer demand drives acceptance.** If enough consumers use BURST, vendors must accept TRST to retain customers. Consumer preference drives currency adoption (this is why USD dominates global trade while smaller currencies don't, regardless of stability).

4. **Vendors are consumers too.** They also receive BRN. They aren't disproportionately disadvantaged by value demurrage.

5. **Risk-adjustable pricing.** Vendors can price slightly higher in TRST to account for potential depreciation.

6. **Bootstrapping as normal money.** With BRN rate = 0 and no expiry, TRST is just normal money. Vendors adopt it as money. The UBI features are introduced later, democratically. There is no moment where vendors must suddenly accept "a strange new UBI currency."

7. **Consumer-driven pressure.** Consumers can proactively refuse to transact with providers who don't accept TRST. If a critical mass of consumers coordinates (through social movements, community norms, or simply personal preference), providers face a choice: accept TRST or lose customers. This is the same mechanism that drives adoption of any payment method — consumer demand, not provider enthusiasm.

### The Bootstrapping Path

The strongest argument for vendor adoption is that it doesn't need to happen all at once. BURST can launch with r = 0 and e = ∞ — literally identical to normal money. There is no TRST expiry, no UBI, nothing unfamiliar. Vendors adopt it as money because it *is* money. Then, through democratic vote, the community can gradually introduce UBI features. At no point does a vendor face a sudden shift to "a strange UBI currency." The transition is incremental, reversible, and controlled by the same people using the system.

### Honest Limitations

Vendor adoption is the most debated aspect of BURST. The equilibrium argument assumes a competitive provider market and assumes TRST has no viable substitutes — in practice, providers can always demand fiat. The bootstrapping-as-normal-money strategy significantly mitigates this, and consumer-driven pressure provides a forcing function, but neither eliminates the risk entirely. The behavior of an economy with short TRST expiry (high-velocity regime) has no empirical analog. Real-world testing in bounded communities and economic simulation are critical prerequisites.

## Ledger Pruning

All history of expired or revoked TRST can be pruned from the ledger. Once TRST is non-transferable, tradeability tracking is no longer needed. This drastically reduces ledger size over time and makes the system future-proof — the ledger does not grow without bound.

---

# Part 7 — Comparison

## vs Circles UBI

**Universal currency vs personal currencies.** BURST has one universal currency (TRST) with origin metadata. Circles gives each person their own token. TRST's origin tracking gives Circles-equivalent trust granularity — receivers can inspect who created the TRST — without fragmenting the currency into thousands of personal tokens.

**No community lock-in.** In Circles, once your wealth is denominated in a community's personal currencies, leaving means abandoning your savings. BURST's TRST is universal — you can change communities, cities, social groups, and your money comes with you.

**No currency ghettos.** If a Circles community is infiltrated by sybils, their currency is tainted and no other community will accept it — doing so would damage their own reputation. Honest members are trapped with devalued coins. This disproportionately hurts the most vulnerable communities. In BURST, revocation targets the specific sybil wallet, not the group.

**No punishment for trust.** In Circles, accepting someone's personal currency is a financial bet on their legitimacy. In BURST, you accept universal TRST, and protocol revocation handles sybil damage.

**Sybil recovery.** BURST revokes sybil-originated TRST retroactively via the merger graph. In Circles, sybil damage through accepted personal currencies is permanent.

**Token distribution.** BRN is computed, not distributed — zero network overhead. Circles periodically mints and distributes tokens.

**Pathfinder dependency.** BURST doesn't need one. Circles requires a pathfinder for transitive transfers — heavy computation, hard to decentralize.

**Demurrage.** BURST uses value demurrage (via expiry). Circles uses quantity demurrage — the protocol periodically reduces your balance. BURST only demurrages TRST (earned excess). BRN (birthright) is never penalized. Circles demurrages all tokens, including the UBI itself.

**Governance.** BURST has democratic self-amendment (one wallet = one vote) with the Consti. Circles has no built-in governance evolution.

**Ledger size.** BURST is prunable — expired TRST can be removed. Circles grows without bound.

**Economic flexibility.** BURST spans the full spectrum from capitalism to UBI via two parameters. Circles has a fixed economic model.

**Group trust compatibility.** BURST's group trust layer achieves trust-graph benefits without personal currencies. Groups are trust graph nodes at a higher abstraction level (hub-and-spoke instead of person-to-person mesh), providing scalable social verification as an optional layer on top of the protocol.

**Where Circles has the advantage:** Circles has stronger per-person accountability — vouch for a sybil and you directly lose money, creating sharp incentive to verify carefully. Trust relationships form organically without organizational overhead — no groups to create or maintain. The system requires zero infrastructure beyond the trust graph itself. Circles is simpler to explain ("I trust you, you trust them"), already deployed, and its intuitive trust model doesn't require the conceptual overhead of two tokens, expiry mechanics, or merger graphs.

**The honest trade-off:** Circles optimizes for trust-graph richness and per-person accountability. BURST optimizes for universality, accessibility, and mobility. BURST's position is that the people who need UBI most — refugees, the homeless, the socially disconnected — are the ones least likely to have strong trust graph connections. A system where your money's value depends on your social connections fails exactly the people it's supposed to serve.

## vs Kleros (PNK)

**Voting power.** BURST: one wallet = one vote, equal for everyone. Kleros: PNK tokens = voting power — wealthy participants buy more votes.

**Verification.** BURST is democratic — BRN staking prevents plutocratic capture. Kleros is plutocratic — more PNK means more influence.

**Security feedback.** BURST has a symbiotic loop — verification and BRN strengthen each other. Kleros has no feedback loop; PNK concentration doesn't self-correct.

## vs Bitcoin and Fiat

**Starting point.** BURST: BRN accrues for existing — a guaranteed floor. Bitcoin: zero — must mine or buy. Fiat: zero — must earn or receive government aid.

**Distribution.** BURST: deterministic, equal, protocol-enforced. Bitcoin: mining rewards favor economies of scale (the wealthy). Fiat: government-controlled, subject to politics.

**Expiry.** BURST: configurable, democratic. Bitcoin: none. Fiat: none (though inflation serves a loosely similar function).

**Generality.** BURST is a superset — Bitcoin is a special case (r=0, e=∞). Both Bitcoin and fiat are particular solutions.

**Governance.** BURST: on-chain democratic self-amendment with the Consti. Bitcoin: off-chain via BIPs and social consensus. Fiat: centralized (central banks, legislatures).

**Where Bitcoin has the advantage:** Bitcoin has a 15+ year track record, proven security under adversarial conditions, massive network effects, and a simplicity that makes it easy to reason about. BURST is unproven. No amount of elegant design substitutes for years of real-world operation.

**Where fiat has the advantage:** Fiat currencies are backed by governments with enforcement power, established legal frameworks, and universal acceptance. BURST has none of these — it must earn adoption purely on merit.

## Developer Funding

BURST has no easy profit mechanism for its creators. This is by design — it's a UBI system focused on redistribution, not accumulation.

**Proposed model:** Developers burn their own BRN to create TRST tokens. This "developer fund" is valued by the market based on the transaction history. Each batch can be valued differently. The market decides what the development work is worth.

This is also an anti-scam signal. There is no pre-mine, no token sale, no ICO, no venture funding mechanism built into the protocol. The only way the development team earns *from BURST itself* is by burning their own BRN — the same BRN everyone else gets at the same rate.

**Practical funding:** The BRN-to-TRST model only works once the ecosystem has meaningful value. Before that, development is funded through traditional means: grants from organizations that fund public goods and open-source infrastructure (Gitcoin, Ethereum Foundation, Open Collective, etc.), charitable contributions from people who believe in the mission, and personal investment from the creator. This is how most open-source projects survive their early years — the protocol's integrity is preserved because the funding source is external to the system, not extracted from it. This bootstrapping phase is philosophically different from the steady-state model: it relies on the kind of external patronage that BURST is designed to make unnecessary. That tension is acknowledged and accepted — every self-sustaining system needs an initial push from outside itself.

---

# Part 8 — Open Questions and Roadmap

## What's Designed

- Two-token system (BRN + TRST) with full lifecycle
- Transaction architecture with `hash`, `link`, `origin`
- Parameterized economic model (the general equation)
- Humanity verification framework (endorsers, verifiers, challenges)
- Democratic governance process (4-phase parameter amendment)
- Delegation system (secondary key pairs, revocable)
- Modular verification architecture (off-chain market)
- VRF feasibility in DAG (phased approach: drand → commit-reveal → threshold VRF)

## What Needs Formalization

### Economic Simulation

This is the single most important next step. Before production code is written, the economic model must be simulated:

- What BRN accrual rate produces stable equilibrium?
- What TRST expiry period balances inflation and utility?
- How does the system behave during rapid growth (many new wallets)?
- How does it behave during contraction (users leaving)?
- What is the velocity of TRST? Is it stable?
- What is the sybil tolerance threshold (maximum percentage of fake wallets before the system destabilizes)?

### Game-Theoretic Analysis

The verification incentive structures must be formally proven, not just argued intuitively:

- Is honest verification a Nash equilibrium?
- Under what conditions does collusion become profitable?
- What is the minimum endorsement burn and verifier stake to make sybil unprofitable in expectation?
- How does the challenge mechanism affect long-term system stability?

### Formal Security Analysis

- Sybil attack resistance under various collusion scenarios
- Economic attack vectors (inflation manipulation, governance capture)
- Verification gaming (systematic false endorsement networks)

### Privacy

Every TRST token carries its full transaction history — origin, link chain, every hand it passed through. This enables revocation and the merger graph, but it also means zero financial privacy. Anyone can trace who paid whom, when, and for how much.

This is a fundamental tension: revocation needs traceability, but users need privacy. Zero-knowledge proofs (ZKP) could potentially allow a wallet to prove a token is valid (not expired, not revoked) without revealing its full history. This is an open research problem — not a roadblock, but a significant area for future cryptographic work. The protocol can launch without privacy and add it later as ZKP techniques mature.

### Key Loss and Recovery

If a wallet holder loses their private key, their BRN and TRST are inaccessible. There is currently no recovery mechanism. The holder would need to create a new wallet, go through verification again, and start BRN accrual from zero. Designing a recovery mechanism is potential future work — but any recovery path is also an attack surface (social engineering to "recover" someone else's wallet). For now, key management is the user's responsibility, same as any cryptocurrency.


## What Needs Engineering

### DAG Implementation

The feeless DAG (block-lattice) architecture needs to be built, including:

- Account chain management
- Transaction validation and propagation
- Representative voting for conflict resolution (double-spend prevention)
- Anti-spam mechanism (lightweight proof-of-work)

### Node-to-Node Communication

The networking layer is completely undesigned:

- Node discovery and peer management
- Transaction propagation protocol
- Clock synchronization protocol
- Partition handling and recovery

### Node Software

Full node implementation including:

- BRN computation engine
- TRST lifecycle management (splitting, merging, expiry tracking)
- Merger graph maintenance
- Verification coordination
- Governance vote tallying
- Ledger pruning

### Wallet Software

User-facing wallet application:

- Key generation and management
- BRN balance display (computed)
- TRST portfolio (transferable, expired, revoked)
- Transaction interface
- Delegation management
- Voting interface

## Roadmap

**Phase 1 — Formalization:** Economic simulation, game-theoretic proofs, formal security analysis. Validate that the design works mathematically before building it.

**Phase 2 — Core Protocol:** DAG implementation, transaction engine, BRN computation, TRST lifecycle. The minimum viable protocol.

**Phase 3 — Verification:** Endorser and verifier system, VRF integration (starting with drand), challenge mechanism.

**Phase 4 — Governance:** Parameter amendment process, delegation system, on-chain constitution.

**Phase 5 — Applications:** Wallet software, node software, developer tools, documentation.

**Phase 6 — Testing:** Bounded community testing (university campus, co-op, small town). Validate economic dynamics in practice.

**Phase 7 — Launch:** Public network. Bootstrap as normal money (r = 0, e = ∞). Let the community decide when to activate UBI features.

---

*For Eliesh.*

---

*BURST is open source under the MIT License. Copyright 2025 Nitesh Gautam.*

*Website: brst.cc | GitHub: github.com/BURST-UBI*
