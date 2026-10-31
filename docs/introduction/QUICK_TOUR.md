# What is BURST?

BURST is a monetary protocol whose participants can change two central rules:
how quickly personal income grows, and how long money created from that income
can be used. These are its economic *parameters*. People choose them through
voting instead of leaving them permanently fixed by the developer.

That describes the mechanism, but why build it? The motivation is to give people
more room to take part in an economy. Someone with savings can afford to learn,
care for a relative or try an idea. Someone without that cushion may not have
the same choice. An income available to everyone could help, while payments
would still let people earn by doing things others value.

You do not have to believe that one particular income rate or currency lifetime
is right to understand the design. BURST makes those choices explicit and lets
the community revise them. A positive income rate gives people ongoing income;
a zero rate stops new income. Currency can have a limited or effectively
unlimited lifetime.

The current defaults use ongoing income and an effectively unlimited currency
lifetime. Let's follow a payment under those settings, then look at the problems
that this creates and how BURST tries to address them.

## An income of your own

Once your account is verified, your personal balance grows over time. It is
called **BRN**, from *burn*. At the default rate, each account that qualifies
for income gains about one BRN per hour. You can save it until you need it;
unused BRN has no expiry date.

Nobody has to send you an hourly payment. Your wallet calculates your income
from how long your account qualified, which rates applied, and what you have
already spent. The network can check the same calculation. This avoids making
every hour of income a separate transaction for every person.

Calling it income does not guarantee that it will cover someone's needs. BRN
only becomes useful purchasing power when people are willing to accept payments
for things others want.

## Something worth paying for

Suppose Alice has saved 50 BRN. Bob has made something she wants, and they agree
on a price of 50. Alice spends her BRN, which is permanently used up, or burned.
That creates 50 **TRST**, from *trust*, for Bob to accept. The two names together
give BURST its name.

![Alice spends BRN to create a payment in TRST, which Bob signs to accept.](../assets/conversion.png)

Bob can pay Carol with that TRST, and she can pass it on again. These later
payments move the same money; they do not create another 50 TRST each time.
Alice and Bob continue to gain their own BRN while they qualify for income.
Earning TRST does not take away Bob's personal allowance.

Why have two balances? They let the community treat unspent personal income
and circulating money differently. Alice can keep her unused BRN. Once she
spends it, the TRST has a creation date and a history, so the network can apply
a currency lifetime or trace money created through a false identity. These are
real differences in the rules, not two names for the same balance.

## Why would someone accept TRST?

Bob accepts because he expects to be able to use the payment himself. The
software cannot make that expectation true. It cannot supply the goods, choose
Bob's price or force another shop to accept TRST. Those depend on the people
participating in the economy.

Expiry is another choice people may disagree about. A limited lifetime lets the
community decide how long earlier payments can keep circulating while new
income continues to grow. But it also gives the recipient a deadline for using
the money. With a 30-year lifetime, Alice's payment would expire 30 years after
its creation, even if it had changed hands many times. Passing it on cannot
restart the clock.

The default lifetime is effectively unlimited. A finite lifetime is an option
the community can choose, with consequences it must weigh. Neither setting
promises stable prices or equal wealth.

## One income for each person

If Alice could create ten accounts and claim ten incomes, the equal-income rule
would mean very little. That is why participation needs more than a new key.
It also matters for voting: additional false identities could gain extra votes.

People joining first get endorsements from verified participants. Endorsers
burn some of their own BRN to support the application. A small group of
verifiers then examines it, chosen by a random draw from volunteers who meet
the verification requirements.

The applicant cannot pick their judges. The network agrees the candidate list
before the random result is known, and nodes can check the draw using the same
public result. This prevents hand-picking or changing the list to fit the
result. It does not guarantee that the people chosen are honest or correct.

Verification has to remain open to challenge. If a fraud challenge succeeds,
the account loses its verified rights and the TRST created from its BRN can
become invalid, even after changing hands. Otherwise, a false identity could
leave its earlier income circulating after it was caught.

That choice can hurt an honest recipient. Bob might already have delivered
goods when some of his payment becomes invalid. The network cannot recover
those goods for him. Random selection, stakes and appeals are attempts to
improve the judgments; they do not remove this risk. The
[verification guide](../protocol/VERIFICATION.md) explains the process and its
current restrictions.

## Who checks the payments?

Your wallet signs actions with your account's key. Computers called **nodes**
check those signatures and whether the actions follow the rules. A signature
proves that the key authorized a payment; it cannot make an overspend valid.

If someone signs conflicting payments, **representatives** vote on which block
to confirm. Each verified person can give one equal unit of voting weight to a
representative. More money does not give that person more weight. This is a
separate job from the verifiers who judge whether an applicant is a unique person.

The distinction matters: selecting people fairly for identity checks and agreeing
which payments happened solve different problems. The
[consensus guide](../protocol/CONSENSUS.md) explains the latter.

## Rules people choose together

The community can propose changes to the income rate, currency lifetime and other
supported settings. Proposals need enough participation and support to pass.
An ordinary approved change then waits before taking effect, so participants
can see what is coming. Choosing a representative to confirm payments does not
automatically give that representative your vote on these proposals.

Voting gives people a say; it does not guarantee they will agree or choose well.
A new income rate applies from then on, leaving previously earned BRN intact.
A new currency lifetime can affect money people already hold. The
[governance guide](../protocol/GOVERNANCE.md) explains those consequences and the
creator's temporary powers while a network is starting.

The [whitepaper](WHITEPAPER.md) develops the broader argument. To see the
accounting in detail, continue with [balances and units](ECONOMICS.md). Before
using or building on the software, read [limits and assumptions](LIMITATIONS.md):
some of the mechanisms described here are currently enabled only on experimental
networks, and their economic and social assumptions remain open to scrutiny.
