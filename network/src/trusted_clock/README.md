# Time-source outages: deferred design

Decision, 2026-10-31: keep the current authenticated NTS clock model for now.
Revisit prolonged external time-source outages later. This is a scope decision,
not a claim that current builds or outage recovery have passed validation.

Today, signing needs at least three fresh authenticated source observations.
Observations expire after 90 seconds; insufficient or uncertain time pauses
operations requiring a trusted signing clock. There is no quorum-based fallback.

The desired future behavior is to track UTC normally, but let a connected honest
quorum advance a shared economic timeline conservatively during a time-service
outage. Slower accrual is acceptable if the same timeline governs everyone.

Before implementing that:

- Bound time advancement using local elapsed clocks; prevent cumulative acceleration.
- Define restart, rejoining, partition and gradual return-to-UTC behavior.
- Audit accrual, expiry, governance, admission and display clocks for consistent use.
- Separate economic time from randomness security: verifier selection currently
  requires commitments before a drand round is released. A slower protocol clock
  cannot establish that the randomness is still unknown. Keep new selections
  paused without trusted time unless that mechanism is redesigned safely.
- Test drift, dishonest proposals, lost sources, restarts and reconnection.

Reuse certified checkpoint timestamps and signing journals. Do not implement
this by extending freshness limits or treating peer agreement as proof of UTC.
