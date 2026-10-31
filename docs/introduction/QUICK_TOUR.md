# What is BURST?

BURST lets people choose the monetary rules they live with. How much personal
income should each person receive? Should money keep circulating forever, or
have a lifetime? Participants decide these settings together.

That is what *parameterized* means here. The personal income rate and currency
lifetime are settings, rather than permanent choices made by the developer.
The income rate can be positive or zero; the lifetime can be limited or
effectively unlimited. The same payment system supports these combinations.

The current defaults give verified people ongoing income and give transferable
currency an effectively unlimited lifetime. Here is how a payment works under
those settings.

## An income of your own

Once you are verified, your personal balance grows over time. It is called
**BRN**, from *burn*. At the current default rate, you gain about one BRN per
hour, just like everyone else's eligible account.

You can save it until you need it. You do not need someone to send it to you,
and unused BRN does not expire.

## Something worth paying for

Suppose Alice has saved 50 BRN. Bob has made something she wants, and they agree
on a price of 50. Alice spends her BRN, which is burned, and Bob receives
50 **TRST**, from *trust*. BRN and TRST together give BURST its name.

![Burning 50 BRN creates 50 TRST for a recipient.](../assets/conversion.png)

Bob can use that TRST to pay Carol. She can pass it on again. These later
payments move the same money without creating more of it.

Alice used her personal income. Bob earned money from his work. Both continue
to gain BRN while eligible. That is why the two balances exist: everyone has
an allowance of their own, and people can also earn from one another.

## One income for each person

Making another wallet must not be enough to claim another income. People joining
BURST first get endorsements from existing participants. A small group of
verifiers then checks the application. This group is chosen by a random draw
from people who have volunteered and meet the verification requirements.

The applicant cannot pick who judges them. The network fixes the eligible list
before the random result is known, and every node can check the draw using the
same public result. This makes the selection fairer by preventing hand-picking
or changing the list to suit the result. The people chosen still have to judge
honestly, and their decisions can be challenged later.

The history of a payment matters too. If an account was verified under a false
identity, a successful fraud challenge can invalidate the TRST it created, even
after that money has changed hands. Honest recipients can be affected, which is
why the quality of verification matters so much.

## Rules people choose together

How quickly should BRN grow? Should TRST keep circulating forever, or have a
limited lifetime? Verified participants vote on these choices. More money does
not buy more voting weight.

The current defaults give TRST an effectively unlimited lifetime. If the community
chooses a limit, passing money to someone else does not restart its age.
Unused BRN still has no expiry date.

The [whitepaper](WHITEPAPER.md) develops the parameterized design and follows
the payment further: how it divides,
combines with other payments, and keeps its history. It also explains verification
and voting. Some of these features are currently restricted to experimental
networks; the [implementation status](WHITEPAPER.md#implementation-status)
sets out what is available today.
