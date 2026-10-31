# BURST

Money gives people room to choose. It can mean being able to learn something new,
care for someone, or take a chance on an idea. We want that room to belong to
more people.

BURST starts with two beliefs: everyone should have access to an income, and
people should be able to earn money by doing things others value. The question
is how to put both into the same system, with rules its participants can change
together.

> An income gives you a starting point. What you do with it is yours to decide.

## What should money make possible? {#why-burst}

Imagine two people with the same idea. One has enough savings to spend a few
months trying it. The other needs every working hour to cover rent. Their
ability is not the difference. Their freedom to use it is.

An income available to everyone could give more people some breathing room.
But an economy also needs people willing to make things and help one another.
When someone does something you value, you should be able to pay them, and they
should be able to use that payment elsewhere.

BURST gives these two purposes different forms. **BRN**, from *burn*, is your
personal income. **TRST**, from *trust*, is what another person receives when
you spend it. Together, they give BURST its name.

That distinction may seem unusual at first. Follow a payment through the system
and the reason for it becomes clearer.

## BRN: your personal income

Once you are verified, your BRN grows over time at the same rate as everyone
else's eligible account. At the current default rate, you gain about one BRN
per hour. Leave it untouched for 50 eligible hours and you have about 50 BRN.

You can save that allowance until you need it. It has no expiry date. Nobody
needs to employ you or send you a payment for it to grow.

BRN belongs to your account. When you spend it, that amount is used up, or
**burned**. An equal amount of TRST is then created for the person you are paying.
You cannot keep the BRN and spend it a second time.

There is no hourly payment sent to every wallet. Your wallet works out how much
you have earned from your verified time and the income rates that applied, then
accounts for what you have spent or locked as a stake. This lets income grow
without filling the network with millions of routine payments.

BRN also gives people something of their own to put behind certain decisions.
Supporting a new person's application burns BRN. A stake locks BRN while a
verification or challenge is decided. You cannot spend that locked amount in
the meantime. [Balances and units](ECONOMICS.md) gives the full calculation.

## TRST: money you can pass on

Alice has saved 50 BRN. Bob has made something she wants to buy, and they agree
on a price of 50. Alice's wallet burns her 50 BRN and creates a payment of
50 TRST. Bob accepts it in his wallet.

![Burning BRN creates an equal amount of TRST for a recipient.](../assets/conversion.png)

Alice used her personal income to buy something she wanted. Bob earned money
by making it. He can now pay Carol with that TRST, and Carol can pay someone
else. The same money keeps moving; those later payments do not create more of it.

Meanwhile, Alice's BRN keeps growing while she remains eligible. So does Bob's.
Earning TRST does not take away his personal income.

This is the reason for having two balances. BRN gives each verified person an
allowance of their own. TRST lets people earn from one another and carry that
value into their next exchange. What a payment buys is still for the people
making the exchange to decide.

## Accepting a payment {#sending-and-receiving}

There is one step in Alice and Bob's exchange that deserves a closer look.
Alice signs the payment, but Bob also signs to receive it. Why both?

Each account keeps its own signed history. Alice can add a payment to hers;
she cannot add an entry to Bob's on his behalf. Bob's signed receive accepts the
payment into his account. Until then, it waits as a pending payment and he
cannot spend it.

This also gives Bob's wallet a place to apply his acceptance rules. It can accept
a payment automatically when those rules allow it, or leave it for him to review.

| The payment | What changes |
| --- | --- |
| Alice pays 50 BRN | Her BRN is burned and 50 new TRST is created for Bob. |
| Bob pays 50 TRST | Existing TRST moves to the next person. No new TRST is created. |

Bob can reject Alice's payment. That creates a return for Alice to accept.
She receives TRST back; the original burn is not undone.

An account can receive TRST before it is verified, but needs verification before
ordinary spending. And signing a payment is only part of making it happen: the
network must also confirm it. The [payment guide](../wallet/PAYMENTS.md) explains
those steps for wallet developers.

## Spending part of a payment {#splitting-trst}

Bob does not have to spend all 50 TRST at once. If he pays Carol 30, he keeps 20.
Carol can later divide her 30 into payments of 10, 5 and 15.

![A 50-TRST payment splits into 30 and 20; the 30 splits into 10, 5 and 15.](../assets/splitting.png "The payment gets divided, but the amounts still add up to 50. Each part remembers where it came from.")

Nothing new has been created. We have only divided the original amount into
smaller pieces. The numbers inside the shapes show steps along the way.

Each piece keeps a connection to Alice's original burn. Even after several
payments, the network can still tell where that money began and how old it is.
Those details matter for the next two questions: how long money should last,
and what to do if someone received income under a false identity.

## Should money have a lifetime? {#expiry}

Personal income keeps adding new spending power. Should money created years ago
also keep circulating forever? BURST leaves that choice to its participants.
They can vote to give TRST a limited lifetime.

Suppose they choose 30 years. The TRST Alice created would stop being spendable
30 years after her burn, whoever holds it then. Sending it from Bob to Carol
would not restart the clock. Otherwise, a quick payment between two accounts
would be enough to avoid the rule.

This has a cost for the person accepting a payment. They need to know how much
time is left. A holding of 50 TRST remains 50 TRST until it expires; the number
does not gradually shrink. People can still agree on a lower price for money
that is closer to expiry.

Alice's unused BRN is different. It does not expire while she saves it. A limited
TRST lifetime would apply to the money she creates when she finally spends.

The current defaults give TRST an effectively unlimited lifetime. The limited
lifetimes in this paper are examples of a choice the community could make.

Expired TRST stays in the records. If a later vote extends its lifetime enough,
it can become spendable again. Expiry means that money has grown too old under
the current rule; it does not mean its history was false.

## Bringing payments together {#merging-trst}

Over time, Bob may collect many small payments. Keeping each one separate would
make a wallet awkward to use. A **merge** brings compatible amounts together
into one larger holding.

Suppose Bob now has three payments: 10, 15 and 25 TRST. Together they make 50.
If they have 150, 145 and 140 days left, the combined amount has 140 days left.
It takes the earliest expiry.

> Bringing money together must not make old money young again.

![10 TRST with 150 days left, 15 with 145 days left, and 25 with 140 days left merge into 50 TRST with 140 days left.](../assets/merge_time.png "10 + 15 + 25 becomes 50 TRST. The combined amount has 140 days left, the earliest expiry of the three.")

Without that rule, Bob could keep old TRST alive by mixing it with a small amount
of newer TRST. The tradeoff is that some of the newer money's remaining time is
lost. A wallet can reduce that loss by choosing amounts with similar expiry dates.

The merge also remembers all three payments. Nodes keep links from the combined
holding back to the amounts that made it. These links form the **merger graph**.
A later merge can link to this one instead of copying its entire history again.

A merge currently accepts up to 256 inputs. It cannot mix expired and unexpired
holdings, or include value removed because of fraud. Combining money should make
it easier to use, without making an invalid amount valid again.

## When an identity turns out to be false {#revocation-and-restoration}

Now suppose one of the people who created Bob's TRST had enrolled under a false
identity. Stopping their future income would prevent more harm. But the money
they had already created would still be moving through the network.

BURST keeps the connection to that original account so a successful fraud
challenge can also invalidate its earlier TRST. This is called **revocation**.
Sending the money elsewhere, splitting it, or merging it cannot erase that
connection.

Return to Bob's merge of 10, 15 and 25. Suppose the 10 came from the false
identity. It made up one fifth of the combined 50, so one fifth of each later
piece is affected. A piece of 10 contains 2 invalid TRST and 8 valid TRST.
A piece of 20 contains 4 invalid and 16 valid.

![Revoking the 10-TRST contribution separates one fifth of each remaining portion from its unaffected value.](../assets/merge_split.png "The dark pieces show the invalid money. The other four fifths remain, subject to their expiry dates.")

The other people's contributions are not thrown away. The network separates the
invalid share, and later payments or merges use what the holder actually has left.

A judgment can be wrong. If the original account is restored through
re-verification, its separated value can become valid again. It does not get
added to a newer merge that used only the valid remainder.
[This worked example](ECONOMICS.md#merging-a-clean-remainder) follows that case.

There is a difficult consequence here. Bob may have accepted the money honestly,
yet still lose its invalid share. The protocol cannot give him back the goods he
sold. Keeping a payment's history makes this rule possible; good verification
is what people depend on to avoid being hurt by it.

## One person, one income {#humanity-verification}

All of this rests on a person receiving one personal income. If making ten
wallets were enough to receive ten incomes, the promise would fall apart.
Creating a key is easy. Verifying a person has to mean more.

An applicant first gets support from verified participants. Those people burn
some of their own BRN to endorse the application. Once there is enough support,
the network selects a panel from eligible volunteers to examine it.

The panel members vote **Legitimate**, **Illegitimate**, or **Neither**.
The first two require a stake: BRN they lock behind their judgment. Neither
requires no stake and does not count as support for acceptance. A missing vote
becomes Neither when time runs out.

The rules set how much agreement is needed and what happens to the stakes.
For example, the default panel has seven members and needs 90% Legitimate votes.
Six out of seven is only about 85.7%, so all seven must support acceptance in
that case. The community can vote to change these settings.

The applicant does not choose the panel. Nodes first agree on who is eligible,
then use a specified future random result from drand to select the members.
They all use the same result, so they all select the same panel. Fixing the list
first prevents it from being changed after the result is known.

People still have to judge the evidence. The software records their votes and
applies the rules; it does not automatically decide whether a face or identity
document belongs to a unique person. The
[verification guide](../protocol/VERIFICATION.md) explains the requirements.

### Correcting a decision {#challenges-and-appeals}

Verification does not need scheduled renewal. But it must be possible to
question an account later. An eligible participant can put up a stake and
challenge it. The challenge starts a review; an accusation alone does not
remove someone's rights.

A finding of fraud removes verified rights and invalidates the TRST created
from that account. A finding of inactivity stops the account's new income but
leaves its earlier TRST intact. Both outcomes stop BRN from growing while the
account is inactive.

Re-verification can restore the account's rights. It does not award income for
the time it was inactive. An appeal provides a way to review an earlier challenge
decision. These are ways to correct mistakes, though their quality still depends
on the people making the judgments.

### Choosing which payments to accept {#optional-trust-policies}

Bob may want his wallet to accept payments automatically only when he trusts
the people whose BRN created them. He can name those people directly or use
membership in selected groups. Groups run their membership services outside
the protocol.

The distinction between a sender and an original creator matters here. When Bob
passes Alice's TRST to Carol, a rule about its creator checks Alice. A rule about
who sent it checks Bob. Carol may care about either, or both.

These are personal wallet choices. They do not change the network's rules.
A payment left for review can still be accepted manually. See
[acceptance policies](../wallet/TRUST.md).

## How the network agrees {#agreement-between-nodes}

So far, we have followed the payment as people see it. Underneath, computers
called **nodes** check the signatures and keep the shared records, or ledger.
Each account has its own chain of signed entries, called blocks. Bob's receive
links his account's history to Alice's payment.

But what if Alice signs two conflicting payments that both try to spend the
same money? Representatives vote on which block to confirm. Each verified
account gives one equal unit of weight to its chosen representative. Owning
more money adds no weight.

> Each verified person has the same weight in choosing representatives.

Some decisions need agreement across accounts. A change to the income rate,
for example, has to mean the same thing to everyone. Nodes use **checkpoints**
to agree on the recorded history and shared rules at those points. They check
the representatives' signatures and work out the resulting state from that history.

Enough representative voting weight must be online for the network to move
forward. A representative also keeps a record of what it has signed, so a
restart cannot let it make conflicting promises. The
[consensus](../protocol/CONSENSUS.md) and [checkpoint](../protocol/CHECKPOINTS.md)
guides explain the details.

## Agreeing on time {#time-and-deadlines}

An income that grows by the hour needs a shared way to account for time.
Otherwise, a fast clock could claim extra income, or one node could close a
vote while another thought it was still open.

Nodes use clock estimates for local checks. Shared deadline effects happen
through agreed checkpoints. Representative signing of those checkpoints uses
authenticated external time sources. Panel selection separately requires a
verified random result from drand.

If the required time or random result is unavailable, the affected work waits.
A node cannot invent an answer so it can keep going. That dependence on outside
services is a limit of the current design. See
[node time configuration](../node/NODES.md#time-and-agreed-transitions).

## Who gets to change the rules? {#governance}

The income rate affects everyone. So does the lifetime of money. BURST puts
those choices in the hands of verified participants, through voting. The person
who wrote the software does not get to keep choosing for everyone.

An ordinary proposal first gathers endorsements. It then goes to a vote, where
enough people must take part and enough must support it. An abstention counts
as taking part, but not as support.

An approved change waits through a cooldown before it takes effect. That gives
people time to see what is coming. Emergency proposals have a shorter path,
with higher voting requirements.

If the income rate changes, the new rate applies from then on. Alice keeps the
BRN she earned under earlier rates. If the TRST lifetime changes, the new limit
affects how old money may be while remaining spendable.

The community can also govern supported verification settings, record
constitutional text, and delegate votes within a chosen scope. Choosing a
representative to confirm payments does not automatically give that person
your governance vote.

A vote cannot install a software feature that has not been written. Some changes
need a software upgrade as well. The [governance guide](../protocol/GOVERNANCE.md)
and [parameter reference](../protocol/PARAMETERS.md) describe what can change.

### How the first people join {#the-first-participants}

There is a practical problem at the beginning: a new network has no established
community of verifiers. During this starting period, the creator can endorse
people through a special path. Those endorsements still burn BRN.

The creator can also veto an approved governance proposal before it takes
effect. Without a veto, it activates without needing separate creator approval.

Those powers are temporary. They end permanently at the starting threshold,
which is capped at 50 verified accounts including the creator. Even if the
population later falls, the powers do not return. The point is to get a community
started, then leave its members on equal terms.
See [bootstrap permissions](../protocol/BOOTSTRAP.md).

## What these choices can, and cannot, do {#economic-choices}

A positive income rate gives people new BRN over time. A limited TRST lifetime
sets a limit on how long the money they create can circulate. An unlimited
lifetime lets it keep moving regardless of age. Setting the income rate to zero
stops new BRN from growing.

These are rules people can choose together. They cannot tell us what a loaf of
bread will cost. That depends on what people make, need, and agree to exchange.
Saved BRN matters too: Alice can spend an allowance she built up long ago, so
this month's income rate alone cannot tell us how much new TRST people will
create this month.

The ambition is to give more people the freedom to take part in an economy.
Software can record the rules and enforce the accounting. A useful economy
still has to be built by the people using it.

## Keeping your account and its history {#wallets-nodes-and-history}

Your wallet holds the key that signs for your account. A recovery phrase can
restore that key, and the wallet can read your balances and verification state
from the network.

If a wallet loses its connection after sending a payment, it must check what
happened before trying again. No response does not mean no payment. Saved signed
actions need their own recovery handling.

Nodes also need more than each account's latest balance. Our examples show why:
they must remember pending payments, merges, and the history behind them.
Even expired money can matter if a later vote extends its lifetime. A verified
snapshot can help a node recover that state; a list of balances alone cannot.

These records are public. An address does not include your name, but payments
and outside information may connect it to you. See
[wallet recovery](../wallet/WALLETS.md), [node storage](../node/NODES.md), and
[privacy](../release/PRIVACY.md).

## Cryptography and upgrades

Digital signatures let nodes check that an action was authorized by the
account's key. BURST uses Ed25519 for those signatures and Blake2b for hashing
and proof of work. Proof of work adds a computation cost to discourage
spam; it does not create money or give someone extra voting weight.

Changing those algorithms or adding new executable rules requires a software
upgrade. A parameter vote or epoch marker does not install code on people's
machines. The [cryptography reference](../development/CRYPTOGRAPHY.md) describes
the formats developers need.

## Implementation status

The paper explains the design. Some parts are still restricted in the current
software: certified verifier selection, governance, and appeals are enabled only
on experimental Dev/Test builds, not Live. Physical deletion of historical block
bodies also remains release-gated. The
[availability table](LIMITATIONS.md#feature-availability) tracks those restrictions.

Tests can check the accounting and whether the software follows its rules.
They cannot by themselves show how well people will verify one another, resist
collusion, or build an economy. Those need real-world observation and independent
review. The [limitations page](LIMITATIONS.md) describes what remains uncertain.

To work on the implementation, start with
[accounts and blocks](../protocol/BLOCKS.md), [RPC](../development/RPC.md), or
[build and test](../development/DEVELOPING.md). The
[architecture guide](../development/ARCHITECTURE.md) maps the design to the code.
