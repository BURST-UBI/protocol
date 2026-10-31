# Protocol parameters

The tables below describe `ProtocolParams::burst_defaults()`, the source defaults
for Live. They are not a report of any running network's current state. Governance
can change values, and Test and Dev override some defaults.

To inspect the connected node's actual values:

```bash
curl http://127.0.0.1:7077/ \
  -H 'Content-Type: application/json' \
  -d '{"action":"protocol_params"}'
```

Governance changes only supported parameters and is subject to the current
[feature switches](../introduction/LIMITATIONS.md#feature-availability).

Amounts are stored as integer raw units: one BRN or TRST is `10^18` raw units.
Durations use whole seconds. A basis point is 0.01%, so `9000` means 90%.
Parameter RPC values use decimal strings to preserve integer precision.
The rounded-up default hourly BRN rate accrues 800 raw units more than exactly
one BRN in 3,600 seconds: `0.0000000000000008` BRN. Balances use integer arithmetic.

## Accrual and expiry

| Field | Live default | Meaning |
| --- | --- | --- |
| `brn_rate` | 277,777,777,777,778 raw/second | Approximately 1 BRN/hour during eligible time. |
| `trst_expiry_secs` | `u64::MAX` | Effectively unlimited lifetime; finite values set age at expiry. |
| `new_wallet_spending_limit` | 0 | Cap on each Send or Burn amount; zero disables it. |
| `new_wallet_limit_duration_secs` | 0 | Stored but not consulted by current node spending admission. |

Changing the rate affects accrual over the corresponding rate-history intervals.
Changing expiry affects the age comparison; a transfer does not reset origin age.
Neither parameter fixes a market price. See [BRN and TRST](../introduction/ECONOMICS.md).

The current spending-cap check uses `new_wallet_rate_limit_duration_secs` for its
age window, not `new_wallet_limit_duration_secs`. Changing the latter does not
change that check. The cap applies separately to each operation, not to a daily
total. A wallet without a verification timestamp remains subject to the cap.
The governance value API does not mark `new_wallet_limit_duration_secs` as inactive;
that metadata alone is therefore insufficient to infer enforcement.

## Verification and challenges

| Field | Live default | Meaning |
| --- | --- | --- |
| `endorsement_threshold` | 3 | Endorsements required for ordinary enrollment. |
| `endorsement_burn_amount` | 336 BRN | Permanent burn per endorsement. |
| `num_verifiers` | 7 | Selected panel size. |
| `verification_threshold_bps` | 9000 | Required Legit share of the panel votes. |
| `verifier_stake_amount` | 500 BRN | Stake for a substantive verifier vote. |
| `max_revotes` | 3 | Maximum re-votes before verification fails. |
| `verification_timeout_secs` | 7 days | Ordinary voting duration from certified panel activation. |
| `min_verification_age_secs` | 30 days | Minimum verified age for selection. |
| `min_verifier_brn_balance` | 50 BRN | Available balance required at selection, separate from vote stake. |
| `neither_penalty_threshold_bps` | 5000 | Neither share must be strictly above this to trigger the penalty. |
| `neither_window_assignments` | 100 | Completed assignments retained for Neither tracking; zero disables tracking. |
| `neither_penalty_cooldown_secs` | 7 days | Selection exclusion after excessive Neither votes. |
| `challenge_stake_amount` | 1000 BRN | Challenger's admission stake. |
| `challenge_duration_secs` | 7 days | Challenge voting duration from certified activation. |
| `challenge_reward_bps` | 100 | Challenger reward fraction of revoked TRST, subject to the cap. |
| `challenge_reward_cap` | 2000 TRST | Maximum challenge reward. |
| `bootstrap_exit_threshold` | 50 | Verified count that ends bootstrap; the effective threshold cannot exceed 50. |

Neither is distinct from Legit and Illegit. It does not support acceptance or
receive a stake reward; missing votes become Neither at the deadline. The rolling
penalty measures retained assignments, not simply elapsed time.

Bootstrap has separate authorization rules. Its completion is permanent even if
the population later falls or governance raises the threshold. See
[creator authority](BOOTSTRAP.md).

## Governance

| Field | Live default | Meaning |
| --- | --- | --- |
| `governance_proposal_duration_secs` | 7 days | Ordinary endorsement window. |
| `governance_proposal_endorsements` | 10 | Endorsements required to enter voting. |
| `governance_proposal_cost` | 100 BRN | Burn per endorsement, not a proposal-submission fee. |
| `governance_exploration_duration_secs` | 14 days | The single ordinary voting window. |
| `governance_supermajority_bps` | 8000 | Required yea share among yea and nay votes. |
| `governance_quorum_bps` | 3000 | Required participation share of the eligible population. |
| `governance_propagation_buffer_secs` | 1 hour | Buffer between voting end and counting. |
| `governance_cooldown_duration_secs` | 7 days | Preparation period after ordinary approval. |
| `consti_supermajority_bps` | 9000 | Approval threshold for constitutional matters. |
| `consti_quorum_bps` | 3000 | Participation threshold for constitutional matters. |
| `emergency_voting_duration_secs` | 24 hours | Emergency voting window. |
| `emergency_quorum_bps` | 9500 | Emergency participation threshold. |
| `emergency_supermajority_bps` | 9500 | Emergency approval threshold. |

Abstentions count toward participation, but not the approval denominator. An
emergency change to either constitutional threshold uses the higher of the
constitutional and emergency quorums. Changing the constitutional approval
threshold also uses the higher of the two approval thresholds. Supported values are limited by the
stored integer types; downstream operations also enforce their validity rules.
A representable value is not a recommendation for a functioning network.

### Inactive stored fields

These four fields are explicitly marked inactive by the parameter value API.
They remain in the serialized parameter set and registry. Changing them
does not restore the old behavior.

| Field | Stored default | Current behavior |
| --- | --- | --- |
| `governance_promotion_duration_secs` | 14 days | No second voting phase. |
| `governance_ema_participation_bps` | 5000 | Does not adjust quorum. |
| `governance_max_rounds` | 3 | A failed proposal requires a new submission. |
| `governance_proposal_window_secs` | 7 days | Ordinary proposals use their own proposal duration. |

## Admission limits

There is no per-account daily transaction quota.

| Field | Live default | Meaning |
| --- | --- | --- |
| `min_work_difficulty` | `0xffff_fff8_0000_0000` | Minimum proof-of-work difficulty. |
| `new_wallet_rate_limit_duration_secs` | 30 days | Used by the current spending-cap age check. |

Proof of work, spending limits and queue policies serve different purposes. None
adds voting weight or replaces signature and ledger validation.

## Test and Dev differences

Dev keeps Live economic rules with easier proof of work. Test also shortens
ordinary governance windows to 30 seconds for proposals, 60 seconds for voting,
15 seconds for cooldown and a 5-second propagation buffer. It removes governance
endorsement cost/count requirements, uses 0.01% quorum and 51% approval, and
removes the verifier minimum-age requirement. These settings make interactive
tests easier; they are not Live defaults.

## Source

- [Parameter definitions and network defaults](https://github.com/BURST-UBI/protocol/blob/main/types/src/params.rs)
- [Governable registry](https://github.com/BURST-UBI/protocol/blob/main/governance/src/params.rs)
- [Lossless parameter values and inactive-field markers](https://github.com/BURST-UBI/protocol/blob/main/governance/src/params/view.rs)
- [New-wallet spending admission](https://github.com/BURST-UBI/protocol/blob/main/node/src/limits.rs)
- [Governance tally and threshold selection](https://github.com/BURST-UBI/protocol/blob/main/governance/src/engine.rs)
