# Account chains, consensus and randomness

Each account has its own signed chain. A payment links the sender's operation to
the recipient's receive, so independent account chains still have dependencies.
Identity and governance transitions also change shared state.

## Competing blocks

Two different successors of the same account position compete. They cannot both
become that account's next confirmed state. Representatives vote on candidates;
confirmation requires the applicable final-vote weight and durable handling of
the block's dependencies.

Each verified account contributes one equal unit to its chosen representative.
Token balances and creator status add no weight. The protocol depends on human
verification to make those accounts correspond to distinct people.

In the certified payment path, the installed checkpoint fixes the electorate,
representative weights, payment epoch and quorum. Local peer availability does
not shrink that electorate. The election helper defaults to 67%, rounded upward
in integer arithmetic; the installed authority supplies the actual threshold.

Representatives persist final-vote commitments. Restart is not permission to
sign a conflicting final vote at the same position. Preserve signing journals
alongside keys and ledger state; see [node recovery](../node/NODES.md).

## Representatives and delegation

A representative receives voting weight when verified humans delegate to its
address. There is no separate registration operation. To sign, its operator must
run a node with the matching key and voting enabled. There is no principal-only
voting threshold.

A representative change has no dedicated cooldown, but certified payment weights
change only when the new roster is certified and installed. An offline
representative's weight stays in the quorum denominator. There is no automatic
fallback, and changing delegation cannot bypass the quorum needed to install a
successor. If too much weight goes offline, the network can stop advancing.

## Election lifecycle

An election starts passive and becomes active when it accepts a vote. Soft votes
can be replaced by later signed sequences. Once a representative's final vote is
accepted, that election rejects any replacement. The lead in combined soft and
final weight controls when final voting can begin; confirmation counts final
votes only.

Confirmed and expired are terminal election states. The node processes durable
cementation separately. Its repair pass expires unresolved elections after five
minutes, but expiry does not erase a representative's persisted voting commitments.

## Which candidates run first

The node queues election candidates by their block's recorded TRST balance,
with larger balances first and hash order breaking ties. This affects admission
under contention, not representative weight or the confirmation threshold.
A bounded ledger scan rediscovers unconfirmed account positions when work must
wait or the node restarts.

Votes can arrive before their candidate block. A bounded cache retains some of
those authenticated votes until the candidate is validated and registered.
Replay assigns weight from the election's frozen roster; a cached weight is not
authority. Cache eviction can lose a vote, so this cache is not durable evidence
of finality.

## Durable cementation

After final-vote confirmation, the node records pending confirmation durably.
A background worker walks dependencies and advances each account's confirmation
height without gaps. It commits confirmed authority and applicable governance
state with that progress. Rollback cannot cross the confirmation height.

One transaction handles up to 256 targets and cements up to 4096 blocks, with a
131,072-entry dependency traversal bound. Larger cuts continue in later batches.
If an implicit dependency still needs its own quorum, the group defers without
committing its staged writes. Confirmation therefore does not promise immediate
cementation or a fixed finality latency.

## Signed votes and conflicting signatures

Wire votes contain one to eight candidate hashes. Their signature covers the
voter, final-vote flag, timestamp and sequence as well as those hashes, under the
BURST vote domain. The receiver still needs the candidate blocks and their
validated dependencies. A signature alone does not make a candidate valid.

The node reserves local final-vote commitments before signing and refuses to
replace an accepted final vote within an election. The standalone equivocation
helper is not connected to this voting path. It stores conflicting hashes without
signatures, so it cannot provide a proof for peers or establish a network-wide
penalty. Soft-vote changes are allowed and must not be treated as conflicting
final signatures.

## Shared checkpoints

Account chains alone do not agree on a verifier pool, human roster, governance
result or identity outcome. Certified checkpoints bind those shared decisions to
an agreed ledger cut and parameter context.

Existing authority authenticates its successor. A proposed new roster cannot
establish its own legitimacy merely by signing with the voters it lists.
Ordinary actions also name their payment epoch, so an old signature cannot be
silently reinterpreted under a new authority context.

If the necessary quorum is unavailable, progress can stop. Discarding vote locks
or counting only reachable voters is not a valid recovery shortcut.

For the replay, signing, installation and recovery contracts, see
[checkpoints and state transitions](CHECKPOINTS.md).

## Verifier selection

Selection first commits to the request, eligible pool, committee size, ledger
cutoff, parameters and future beacon round. Only then can the agreed randomness
be used. Choosing a pool after seeing the output would give the proposer a way to
influence the selected panel.

Drand supplies a verifiable beacon. BURST verifies its pinned chain policy and
proof; an HTTP relay only delivers the data. Drand does not determine eligibility
or certify BURST's identity judgments.

The certified selection path validates the checkpoint certificate and exact
beacon round, scores each eligible address with the verified random output, then
sorts scores with address order as the tie-breaker. Identical inputs produce the
same committee. For a pool of P accounts, this scoring/sorting step takes
O(P log P) work; checkpoint replay and certificate verification add their own costs.

If the agreed output is unavailable, the finalized request waits for that same
round. Another relay can supply it, but the node cannot substitute local
randomness, a different pool or a convenient later round.

An unagreed proposal, a prepared proposal and a finalized selection have different
recovery obligations. Consensus recovery must retain prepared evidence and vote
locks; elapsed time alone cannot erase evidence held by another participant.
These mechanisms do not constitute a proof against every scheduling or economic
attack.

## Experimental randomness helper

`vrf::commit_reveal` is a local transcript helper, not the node's production
beacon. It seals the commitment set and requires every participant to reveal.
A participant withholding a reveal prevents output. The helper does not implement
authenticated network membership, deadline agreement or distributed recovery;
it cannot replace the drand selection path on those terms.

## Availability

The current certified-selection gate is disabled on Live. Experimental builds
can enable the path on Test and Dev. These implementation rules are not a claim
that a Live deployment or release has been qualified. See
[verification availability](VERIFICATION.md#network-availability).

## Implementation

- [Signed block fields](https://github.com/BURST-UBI/protocol/blob/main/ledger/src/state_block.rs)
- [Installed payment authority](https://github.com/BURST-UBI/protocol/blob/main/node/src/payment_authority.rs)
- [Election threshold arithmetic](https://github.com/BURST-UBI/protocol/blob/main/consensus/src/election.rs)
- [Durable voting](https://github.com/BURST-UBI/protocol/blob/main/node/src/durable_votes.rs)
- [Certified selection request](https://github.com/BURST-UBI/protocol/blob/main/verification/src/selection_request.rs)
- [Experimental commit-reveal transcript](https://github.com/BURST-UBI/protocol/blob/main/vrf/src/commit_reveal.rs)
- [Exact-round beacon retrieval](https://github.com/BURST-UBI/protocol/blob/main/node/src/checkpoints/beacon.rs)
