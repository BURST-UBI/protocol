# Numeric policy boundaries

A number's role determines how it can change. Economic parameters, signed
encodings, agreement thresholds and local resource budgets have different
constraints.

## Governed parameters

The [parameter reference](../../docs/protocol/PARAMETERS.md) lists all 40 fields in
`ProtocolParams`, their source defaults and enforcement caveats. The governance
registry exposes every field. Four are explicitly marked inactive; the separate
`new_wallet_limit_duration_secs` field is also unused by spending admission but
is not marked inactive by the value API.

| Policy | Parameter fields |
| --- | --- |
| Verifier selection balance | `min_verifier_brn_balance` |
| Excessive Neither votes | `neither_window_assignments`, `neither_penalty_threshold_bps`, `neither_penalty_cooldown_secs` |
| Emergency voting | `emergency_voting_duration_secs`, `emergency_quorum_bps`, `emergency_supermajority_bps` |
| Governance endorsement burn | `governance_proposal_cost` |
| Verification endorsement burn | `endorsement_burn_amount` |
| Certified challenge voting duration | `challenge_duration_secs` |

The catalog checks field coverage, not every numeric constant in the repository.
A representable parameter value is not necessarily a viable network setting.
See [governance](../../docs/protocol/GOVERNANCE.md) for voting and activation rules.

## Encoding and agreement

The basis-point denominator of 10,000 and the BRN/TRST scale of `10^18` raw units
are representation rules. Changing them would reinterpret amounts or thresholds
in existing data; they are not independently adjustable economic rates.

The genesis checkpoint anchor uses a 6,700-basis-point human quorum. Checkpoint
validation accepts quorum values from 6,667 through 10,000 and validates a
certificate against its parent anchor's roster and quorum. An ordinary economic
parameter change must not bypass those agreement rules. See
[consensus](../../docs/protocol/CONSENSUS.md) and [decision C03](../DECISION_RESOLUTIONS.md).

The legacy `CHALLENGE_TIMEOUT_SECS` helper measures time from admission. Certified
challenge panels use `challenge_duration_secs` from certified activation instead.
The helper alone cannot authorize certified settlement.

## Resource limits

Message sizes, merge/release source counts, history-transfer budgets and cache
limits bound different operations. Some constrain signed formats; others govern
only local admission or scheduling. Check the consuming code before changing one
or treating it as a network-wide rule. See [service limits](../../docs/node/SERVICES.md)
and [performance](../../docs/development/PERFORMANCE.md).

## Validation scope

The historical numeric-policy results remain in iteration 132 of
[the reference review](REFERENCE_IMPLEMENTATION_REVIEW.md). Controlled-time
fixtures and signed historical drand proofs do not establish production-time
beacon secrecy, partition availability or safe economic settings. This guide does
not certify parameter interactions or change any release gate.
