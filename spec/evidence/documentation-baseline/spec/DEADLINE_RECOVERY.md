# Selection deadline recovery

The owner authorized recovery on 2026-10-12: "okay add the recovery and do the
full release". The whitepaper selection section and decision 19.4 now distinguish
an unagreed proposal from a certified selection. Decision 19.3 is unchanged.

## Rule

A fresh selection proposal is valid only before its pinned drand round can be
known. The clock's uncertainty includes time spent replaying history and waiting
for a writer. Missing or uncertain clock authority does not permit a late vote.

After an expired proposal, representatives finish the existing consensus round.
Prevote and precommit timeouts require authenticated participation from the
fixed parent electorate. The node may follow a later round with more than one
third of that electorate's signed participation. Neither transition clears a
prepared lock. Offline humans stay in the quorum denominator.

At an unsigned start in a later round, a proposer without a known prepare proof
may replay a new cut and propose its first future beacon round. Replay, not the
proposer's address list, determines the request and complete eligible pool.
Refreshing the candidate cache does not sign anything. Durable signing rechecks
the payment epoch, final-vote promises, replay commitments and pre-beacon clock.
Once that proposer has signed a proposal in the round, refresh cannot change it.

A proposer with a valid prepare proof uses its exact checkpoint. Delayed
prepare proofs are authenticated even when they refer to an earlier round.
They survive restart and can justify the original selection after its beacon
deadline. A timeout does not prove that no such certificate exists elsewhere.
Votes remain subject to the lock/valid-round rules described in
[Tendermint Algorithm 1](https://arxiv.org/abs/1807.04938).

A decided selection retains its request, pool, parameters and beacon round.
Missing drand data means wait and retry exact-round relays. Recovery does not
create a paid endorsement, refund, new verification attempt or local random
fallback. Those require their own agreed lifecycle transitions.

## Security boundary

The implementation keeps consensus safety separate from randomness bias.
Preserving locks prevents timeout-based double decisions under the consensus
assumptions. It does not prove that a scheduler cannot influence which
uncommitted proposal reaches quorum before its beacon. In particular, an
adversary may delay messages until outputs become known before the network
becomes synchronous. No test result here is a proof against selective abort.

Progress requires enough reachable honest human weight, bounded message/replay
latency within a beacon window, a trusted clock bound and eventual drand
availability. The current first-future-round policy has a window below 30
seconds; a larger generic consensus timeout cannot lengthen it. Deployment
timing and adversarial scheduling review remain release prerequisites.

## Evidence

- `deadline_recovery::expired_minority_selection_recovers_in_a_new_round_after_restart`
  uses nine independent signer stores. Six cannot prepare or time out the vote
  phase alone. Nil participation from returning humans advances the round. A
  restarted proposer supplies a new cut and all nine decide the same checkpoint.
- `delayed_prepare::old_round_prepare_survives_deadline_catchup_and_restart_without_fresh_selection`
  rejects a minority proof and an expired fresh proposal, then retains the old
  quorum proof through refresh and restart. It decides the original checkpoint.
- `checkpoint_service_recovers_expired_minority_selection_over_tcp` exercises
  the service scheduler and transport with controlled clocks. Seven humans
  recover an expired minority proposal; two late signers learn the decision
  without fresh clock authority. It does not test real-time drand secrecy.

All tests are in `node/src/confirmed_replay`. Source release gates remain closed.
