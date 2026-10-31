# Governance

An income rate is a choice about how much new personal income people receive.
A currency lifetime is a choice about how long their payments remain spendable.
Both affect people beyond whoever first wrote the software, so BURST gives
verified participants a way to propose and vote on changes.

This page follows a proposal from support to activation, then explains what a
change does to existing balances. Choosing a representative to confirm payments
does not automatically delegate your governance vote. Voting on monetary rules
and agreeing which transactions happened are separate responsibilities.

Governance can change supported settings. A vote cannot install new software,
and constitutional text cannot create behavior the code does not implement.
Nor does a successful vote establish that a policy is economically wise; it
records that the proposal met the community's decision rules.

## Network availability

Current Live gates disable the certified selection path, full governance-content
operations and ledger-backed delegation. Experimental Test/Dev builds can enable
these paths; that does not enable them on Live. The node rejects governance
operations when their required certified path is unavailable.

## Ordinary proposals

Suppose participants want to raise the BRN income rate. A verified proposer
submits the proposed setting. Other participants must support it before it goes
to a vote, so submitting an idea and qualifying it for a ballot are separate
steps. Supporters burn the configured BRN amount to endorse it.
Those burns are not refunded when a proposal loses or is withdrawn. Only the
proposer can withdraw, and only during the proposal phase.

An ordinary proposal goes through these stages:

1. **Proposal:** supporters burn BRN until the endorsement requirement is met.
2. **Voting:** eligible accounts vote yea, nay or abstain during one voting window.
3. **Counting:** the network agrees the result after the propagation buffer.
4. **Cooldown:** an approved proposal waits for its scheduled activation.
5. **Activation:** a certified boundary applies the change.

The code calls the voting phase `Exploration`. The stored `Promotion` value is
not a second ballot.

## Participation and approval

Eligible accounts vote yea, nay or abstain. Each contributes at most one effective
ballot, with [delegation rules](GOVERNANCE.md#governance-delegation) applied where enabled.
Two thresholds must pass. The participation threshold asks whether enough people
took part; the approval threshold asks whether enough of those choosing a side
supported the change. Without the first check, a tiny group could decide a rule
for everyone simply because nobody else voted:

```text
participation = (yea + nay + abstain) / eligible accounts
approval      = yea / (yea + nay)
```

The implementation calculates basis-point ratios with integer division. Abstain
helps meet participation without increasing approval. Unanimous support from a
small minority can therefore fail quorum. A failed proposal is rejected;
resubmission needs a new proposal and endorsements.

The propagation buffer separates voting close from counting. Successful ordinary
proposals wait through cooldown and activate at their certified boundary. A local
timer does not replace the agreed result or authorize an account to submit its
own activation block.

During active bootstrap, the verified creator can explicitly veto an approved
proposal before activation. Otherwise activation needs no creator approval.
The permission ends permanently at the [bootstrap threshold](BOOTSTRAP.md).

## Effects of a change

If Alice earned BRN at one rate yesterday, a new rate today does not rewrite
what she earned. The node keeps the old and new rates with the times they apply.

Changing the TRST lifetime has a different effect. Money that is 100 days old
would be expired under a 90-day lifetime but usable under a 120-day lifetime,
unless invalid for another reason. Shortening the lifetime can also expire money
someone already holds. This is a consequence voters need to consider; a
successful vote does not protect every holder from loss. See
[accounting](../introduction/ECONOMICS.md).

The governed registry exposes supported values and storage widths. Not every
stored field still has an active effect; consult [parameter behavior](PARAMETERS.md)
before assuming a change will alter the network.

A later proposal can change a setting again. It cannot erase payments or
real-world decisions made while the earlier setting applied.

## Constitutional text and content

An amendment can add an article, replace an article's text or mark an article as
repealed. Repeal retains the article record. Applying a patch works on a candidate
document and publishes it only after all operations succeed, with one new document
version for the amendment. The whole-text proposal form adds one article.

An approved patch can become inapplicable if another amendment repeals its target
first. Missing or already-repealed targets are recorded as terminal conflicts by
the activation path; invalid content and other failures remain errors. The local
constitution engine does not authenticate approval or activation timing by itself.
Those checks belong to governance and certified replay.

The constitution supports adding, modifying and repealing articles while retaining
amendment history. Constitutional matters use their applicable quorum and approval
thresholds. Participants still interpret the meaning of that text; the protocol
checks the amendment operation, not every possible interpretation.

Long proposal content is bound by a signed commitment and transferred under size
and encoding limits. A hash does not make unavailable text readable to voters.
Content must be obtained and checked against its commitment before review.

The proposal model also supports representative eviction and reinstatement.
Eviction removes the representative's consensus weight; delegating accounts can
choose another representative. This is a governance decision, distinct from
ordinary transaction-consensus voting.

## Emergency proposals

Emergency parameter proposals enter voting directly and skip ordinary endorsement
and cooldown stages. They use the applicable emergency thresholds and scheduled
activation rules. Changing constitutional thresholds can require the higher
constitutional requirement as well; the exact combinations are in
[the parameter guide](PARAMETERS.md#governance).

An emergency is not a creator-only override. It still needs votes and the
protocol's agreed outcome. It cannot produce a missing consensus quorum.

## Governance delegation

The engine supports global, category, parameter and proposal scopes. It checks the
most specific matching scope first: proposal, then parameter, category and global.
Delegation can be transitive. If Alice delegates to Bob and Bob delegates to Carol,
the resolver follows that chain while checking eligibility, cycles and depth.

Each eligible account contributes at most one ballot. Its direct vote takes
precedence. Otherwise the tally checks its projected key ballot before ordinary
delegation. Cycles and ineligible delegates do not create additional votes.

Scope selection happens at every step. Removing an ordinary proposal-specific
delegation lets a matching parameter, category or global delegation apply again.
The chain's terminal account supplies the direct vote; the resolver does not stop
at an intermediate account merely because that account voted.

Ledger-backed grants and revocations authorize restricted secondary-key ballots.
Their scope and historical grant proof are checked separately from the account
block envelope. Sharing a spending key is not the delegation mechanism.

Key grants support proposal and parameter scopes. A proposal-specific record
takes precedence, including when it records a revocation. Revoking that grant
therefore blocks the broader parameter grant for this proposal instead of falling
back to it. Generations must increase by exactly one, and revocation records remain
stored so an older grant cannot restore authority.

At counting, an explicit key revocation or a key-grant chain with no effective
ballot also blocks ordinary delegation fallback for that owner. The owner's
direct vote still takes precedence. Removing an ordinary graph edge and revoking
a signed key grant are different operations.

The engine's depth limit counts visited accounts, including the starting account
and terminal. Its default of ten therefore allows at most nine delegation edges.
The graph's `voting_power` helpers are structural counts, not ballot results: they
do not establish eligibility or account for direct-vote overrides and key grants.

**Availability:** ledger-backed governance delegation is currently disabled on
Live. The experimental feature can enable it on Test and Dev. Consensus
representative selection is separate and does not automatically delegate
governance votes. See the [release policy](https://github.com/BURST-UBI/protocol/blob/main/node/src/release_policy.rs).

## Implementation

- [Proposals and phase types](https://github.com/BURST-UBI/protocol/blob/main/governance/src/proposal.rs)
- [Submission, voting, withdrawal and veto](https://github.com/BURST-UBI/protocol/blob/main/governance/src/engine.rs)
- [Content commitments and bounds](https://github.com/BURST-UBI/protocol/blob/main/governance/src/content.rs)
- [Constitution changes](https://github.com/BURST-UBI/protocol/blob/main/consti/src/engine.rs)
- [Certified governance replay](https://github.com/BURST-UBI/protocol/blob/main/node/src/confirmed_replay/governance.rs)
- [Network gates](https://github.com/BURST-UBI/protocol/blob/main/node/src/release_policy.rs)
- [Delegation scopes and records](https://github.com/BURST-UBI/protocol/blob/main/governance/src/vote_delegation.rs)
- [Transitive ballot resolver](https://github.com/BURST-UBI/protocol/blob/main/governance/src/vote_delegation/ballot_resolver.rs)
- [Ledger delegation operations](https://github.com/BURST-UBI/protocol/blob/main/governance/src/signing_delegation/operation.rs)
