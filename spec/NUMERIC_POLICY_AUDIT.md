# Numeric policy audit

This is a scoped audit of runtime constants, not a sign-off of every numeric
requirement. The whitepaper's Protocol Parameters section and decisions 20.2
and 20.10 require democratic control of economic and governance policy.

## Corrections

| Rule | Previous behavior | Current implementation |
| --- | --- | --- |
| Verifier balance | Hardcoded 50 BRN in confirmed replay | `min_verifier_brn_balance`, default 50 BRN; available balance at the certified selection cut |
| Emergency participation | Ordinary quorum despite decision 20.11 | `emergency_quorum_bps`, default 9500; all eligible verified humans are the denominator |
| Emergency approval | Hardcoded 9500 | `emergency_supermajority_bps`, default 9500; abstentions excluded from the approval denominator |
| Emergency duration | Hardcoded 24 hours | `emergency_voting_duration_secs`, default 86400; frozen at submission |
| Emergency admission | Node sent emergency content through ordinary support | Verified submission enters voting and reserves its counting boundary immediately |
| Proposal endorsement burn | Default 336 BRN, despite the 100 BRN field comment and decision 20.1 | Default 100 BRN; verification endorsement burns stay 336 BRN |
| Neither history | Lifetime counters and fixed 50% | Iteration 131 added a governed rolling assignment window and threshold |

The parameter catalog compares every serialized field name against its governance
mapping. It currently covers 41 fields, including four explicitly inactive legacy
fields. This catches missing mappings inside `ProtocolParams`, not constants
elsewhere in the code. Parameter values keep their storage widths; units and raw
amounts are exposed losslessly to the wallet.

## Constants that need different treatment

The denominator 10,000 and the BRN/TRST raw-unit scales define representation.
Changing them independently would reinterpret existing balances and signed data.
They are not adjustable economic rates.

The 67% human consensus quorum defines certificate intersection. C03 records the
safety/availability choice. Do not expose it as an ordinary parameter without a
transition design that preserves finality across old and new electorates. Local
online-weight sampling and vote spacing are separate networking behavior; they
must never substitute for certified human authority.

`verification/src/challenge.rs::CHALLENGE_TIMEOUT_SECS` is a legacy admission
helper. Live certified settlement uses `challenge_duration_secs`. Removing the
constant alone would not improve live deadline agreement.

Message sizes, inline merge/release limits, bounded replay histories, content
chunks and cache limits constrain resource use or signed encoding. Several come
from explicit implementation decisions or C01/C07. Their classification and safe
upgrade behavior still need a complete requirement audit. Calling them resource
limits does not sign off the corresponding whitepaper requirement.

## Evidence boundary

See iteration 132 in `REFERENCE_IMPLEMENTATION_REVIEW.md` for commands and results.
The connected-node fixture uses controlled time and signed historical drand
proofs. It cannot establish production-time secrecy, availability under partitions
or safe economic settings. The source documents are unchanged and the release
gates remain closed.
