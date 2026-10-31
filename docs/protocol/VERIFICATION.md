# Account verification

If creating another wallet gave someone another income and vote, one person
could claim the share of many. Verification is BURST's attempt to establish one
participating account per person. Passing it gives an account the right to gain
BRN and take part in voting.

In ordinary enrollment, a small group of verifiers, called a panel, is drawn randomly from eligible volunteers to assess the application.
The applicant cannot choose its members. Nodes can independently check the draw
from the agreed candidate list and public random result.

Why involve people at all? A digital signature can establish that someone
controls a key. It cannot establish that the key belongs to a unique person.
Verifiers examine the evidence; the software records their votes and applies
the agreed result. This makes the quality of those judgments part of the
system's security. A random draw can prevent hand-picking without proving that
any particular judgment is true.

## Network availability

The current global certified-selection switch is off. The `experimental-testnet`
feature can enable it on Test and Dev, never Live. Appeals have a separate gate
with the same network restriction. A function's presence in the library does not
make it an available production operation.

The rules below describe those implemented paths. Bootstrap has separate
[creator-authority rules](BOOTSTRAP.md).

## Ordinary enrollment

1. Existing verified participants endorse the applicant by burning BRN.
2. The network fixes the application, the eligible volunteers and a future public
   randomness round before its result is known.
3. That result selects the verifiers. The network agrees when their review starts.
4. The selected verifiers examine the evidence and vote.
5. The network certifies the result and applies the resulting account changes.

An endorsement puts some of an existing participant's own allowance behind an
application. Endorsers permanently burn BRN; they do not receive a protocol
reward when the applicant succeeds, and failure does not refund that burn.
This gives endorsements a cost, but an endorsement is still support for an
application, not proof that the applicant is a unique person.

Selection uses agreed ledger state rather than whichever accounts a node happens
to know. The replay eligibility check requires verified status, an on-chain
verifier opt-in, sufficient verified age, the minimum BRN balance and completed
external TRST activity with a counterparty that is still verified. Opt-in alone
is not enough. Panel selection also applies its request-specific exclusions.

The random result comes from drand, a public randomness service. The request
fixes the eligible list and a future drand round before that result is available. After verifying the checkpoint certificate and that exact beacon
round, nodes rank pool members by a hash of the beacon value and wallet address.
The lowest-ranked members form the panel. This gives each account in the frozen
pool a random rank, without weighting it by wealth. Freezing the list first
prevents it from being adjusted to favor particular people after seeing the
result. Requiring the specified future round prevents a node from choosing
among random results until it finds a panel it likes. Later eligibility changes do not
rewrite the frozen pool. A certificate relies on its signers checking the pool
and signing deadline; the signatures alone cannot prove when they were made.

With a seven-member panel and a 90% Legitimate threshold, all seven must vote
Legitimate: six of seven is only about 85.7%.

The vote stake is separate from the minimum selection balance. Current defaults
and durations are listed in [protocol parameters](PARAMETERS.md).

## Votes and settlement

A verifier votes **Legitimate** to support acceptance, **Illegitimate** to oppose
it, or **Neither** when making neither judgment. Legitimate and Illegitimate
require BRN to be locked as a stake. Neither requires no stake, but the verifier
still counts as a panel member when the acceptance threshold is calculated.

This matters when someone abstains. With seven members and a 90% acceptance
threshold, six Legitimate votes and one Neither do not pass. Ignoring the
abstention would turn uncertainty into apparent unanimity.

Absent votes become Neither at the applicable deadline. Repeated Neither votes
can trigger selection exclusion under a rolling assignment window. Neither is
therefore distinct from both a substantive Illegitimate judgment and an ignored
assignment.

By default the window retains up to 100 assignments and the penalty triggers
strictly above 50% Neither. There is no minimum sample size: one Neither on a
first assignment already exceeds that threshold. Applying a penalty clears the
history immediately and starts the selection cooldown, seven days by default.
The window counts assignments rather than days.

If the required Legitimate share is not reached, the configured re-vote limit
controls further attempts. Deadline settlement belongs to agreed protocol state;
a node's local clock alone cannot independently decide a panel outcome.

The activation fixes the deadline using the parameters at activation. Missing
members receive synthetic zero-stake Neither votes, so absence does not reduce
the completed panel's denominator. Those records are distinct from signed votes.
A revote excludes the preceding panel and requires a new selection; prior votes
remain available for stake settlement.

Staked voters who agree with the settled outcome recover their stakes. Rewards
share forfeited dissenting stakes equally among those winning staked voters,
using integer division. With no forfeited stakes there is no such reward pool.
“Winning” means agreement with the settled judgment, not proof of factual truth.

A successful verification establishes the recorded eligibility time for BRN.
Balances are still subject to burns, locked stakes and release acknowledgment.

## Challenges

A challenge names a target, supplies the required stake and goes through the
applicable admission, panel and outcome rules. Submission itself does not revoke
an account.

An upheld fraud challenge removes verified rights and revokes TRST derived from
the target's origins, including affected contributions through merges. An upheld
inactivity challenge deactivates the account without revoking past TRST. These
are different economic consequences, not interchangeable labels.

A rejected challenge can forfeit the challenger's stake. A successful fraud
outcome can pay a capped reward based on revoked TRST. Costs and rewards do not
prove that collusion or harassment is unprofitable.

## Re-verification and appeals

Verified status has no calendar-based renewal deadline. Voting deadlines and
verifier penalty cooldowns do not impose periodic enrollment on every account.
An unresolved challenge retains verified rights until its outcome is applied.

An unverified or deactivated verification state can restart with a new
endorsement once any active challenge settles. The endorsement must not predate
the prior outcome. Restart clears the old panel, votes and attempts; ordinary
enrollment must meet its requirements again. Successful re-verification resumes
retained BRN history without accrual for the inactive interval.

Re-verification asks whether the account should regain eligibility. When the
commit path verifies a previously revoked account, it restores that originator's
affected holdings and pending contributions, then updates account state. It does
not mint replacement copies. Restored amounts remain subject to expiry.

An appeal instead names an earlier challenge judgment. Settlement compares the
new judgment with that recorded judgment; it is not an ordinary challenge payout
applied again. This path is currently unavailable on Live. Do not promise users a
production appeal service based on its experimental implementation.

The wallet appeal API refetches the node's review immediately before submission.
It requires the outcome and required stake to match what the user reviewed, rejects
an active challenge, and checks that appeals remain enabled. A changed review must
be loaded again. These checks use the node's report; parsing a nonzero checkpoint
reference is not independent verification of its certificate.

Neither restoration nor an appeal can reverse goods or services exchanged outside
the ledger. Recipients can bear the consequences of a wrong or fraudulent origin
judgment even when they acted honestly.

## Evidence and privacy

Evidence assessment happens outside vote counting. The `VerificationMethod` trait
is an interface, not a shipped identity-document or biometric verification
service. Communities must decide how evidence is collected and assessed without
assuming that the protocol supplies those integrations.

Verification evidence may expose personal information. A public account address
and its ledger history should not be mistaken for an anonymous identity. See
[privacy and network behavior](../release/PRIVACY.md).

## Read an account's verification state

```json
{"action":"verification_status","account":"YOUR_ACCOUNT_ADDRESS"}
```

Send this to the node's [RPC listener](../development/RPC.md). The result includes
`status`, `phase`, `verified_at`, endorsers, selected verifiers and votes.
Check `selection_available` and `selection_unavailable_reason` before showing a
user a panel workflow. `verifier_opted_in` means the account volunteered; it does
not mean it was selected or meets all selection requirements.

## Endorsement limits and revocation

The current node has no per-endorser limit on simultaneous applications across
different targets. It rejects duplicate endorsements within an attempt and
requires the ordinary endorsement burn. Endorsements disclose the endorser and
target on the ledger.

Revoking an endorser does not currently remove its endorsements from other pending
applications. A standalone removal helper exists, but the node revocation path
does not invoke it. Automatic invalidation and a cross-target active-endorsement
cap were intended rules that remain unimplemented. Neither should be assumed
when assessing the current verification system.

## Evidence and notifications

The challenge transaction has no evidence attachment or URL field. Applications
must arrange evidence sharing separately. Its signature and stake authorize a
request; neither proves fraud or inactivity.

The WebSocket library exposes a verification topic, but the node does not
currently publish challenge outcomes through its publisher. Clients must query
current verification state rather than rely on a push notification. Generic
WebSocket broadcasts can lose events when a subscriber falls behind.

## Implementation

- [Selection gate](https://github.com/BURST-UBI/protocol/blob/main/verification/src/lib.rs)
- [Replay eligibility](https://github.com/BURST-UBI/protocol/blob/main/node/src/confirmed_replay.rs)
- [Voting and timeout defaults](https://github.com/BURST-UBI/protocol/blob/main/verification/src/voting.rs)
- [Stake and reward outcomes](https://github.com/BURST-UBI/protocol/blob/main/verification/src/outcomes.rs)
- [Challenge transitions](https://github.com/BURST-UBI/protocol/blob/main/verification/src/orchestrator.rs)
- [Verification and restoration commit](https://github.com/BURST-UBI/protocol/blob/main/node/src/verification_commit.rs)
- [Appeal release policy](https://github.com/BURST-UBI/protocol/blob/main/node/src/release_policy.rs)
