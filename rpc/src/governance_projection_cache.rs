//! Bounded derived display results. No engine roots or electorate are retained.
use burst_governance::Proposal;
use burst_types::{BlockHash, Timestamp, TxHash};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Context {
    identity: usize,
    uses_projection_identity: bool,
    now: Timestamp,
    params_hash: BlockHash,
}

impl Context {
    pub(crate) fn from_snapshot(snapshot: &crate::governance_snapshot::Snapshot) -> Option<Self> {
        snapshot.generation.map(|generation| Self {
            identity: snapshot.projection_identity.unwrap_or(generation),
            uses_projection_identity: snapshot.projection_identity.is_some(),
            now: snapshot.now,
            params_hash: snapshot.params.params_hash(),
        })
    }
}

#[derive(Default)]
struct Entries {
    context: Option<Context>,
    proposals: BTreeMap<TxHash, Entry>,
    bytes: u64,
}

struct Entry {
    proposal: Proposal,
    bytes: u64,
    computed_at: Timestamp,
    valid_until: Option<Timestamp>,
}

/// Scoped to one immutable Reader instance in RpcState. At most 128 proposals
/// and 8 MiB of serialized proposal payload are retained. Allocator overhead and
/// response clones are not included in that payload budget.
#[derive(Clone)]
pub struct GovernanceProjectionCache {
    entries: Arc<Mutex<Entries>>,
    max_entries: usize,
    max_bytes: u64,
}

impl Default for GovernanceProjectionCache {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Entries::default())),
            max_entries: 128,
            max_bytes: 8 << 20,
        }
    }
}

impl GovernanceProjectionCache {
    #[cfg(test)]
    fn project(
        &self,
        context: Option<Context>,
        hash: TxHash,
        compute: impl FnOnce() -> Proposal,
    ) -> Proposal {
        self.project_until(context, hash, || {
            (
                compute(),
                context.and_then(|c| c.now.as_secs().checked_add(1).map(Timestamp::new)),
            )
        })
    }

    /// Reuse a proposal while its input identity and parameters match and time
    /// stays before `valid_until`. `None` means no time-based expiry for those inputs.
    /// The caller supplies that deadline. Clock reversal clears the cache; missing
    /// context bypasses it. Capacity eviction removes the lowest proposal hash.
    pub(crate) fn project_until(
        &self,
        context: Option<Context>,
        hash: TxHash,
        compute: impl FnOnce() -> (Proposal, Option<Timestamp>),
    ) -> Proposal {
        let Some(context) = context else {
            return compute().0;
        };
        {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let same_inputs_forward = entries.context.is_some_and(|previous| {
                previous.identity == context.identity
                    && previous.uses_projection_identity == context.uses_projection_identity
                    && previous.params_hash == context.params_hash
                    && context.now >= previous.now
            });
            if !same_inputs_forward {
                *entries = Entries::default();
            }
            entries.context = Some(context);
            if let Some(entry) = entries.proposals.get(&hash) {
                if context.now >= entry.computed_at
                    && entry.valid_until.is_none_or(|at| context.now < at)
                {
                    return entry.proposal.clone();
                }
            }
            if let Some(expired) = entries.proposals.remove(&hash) {
                entries.bytes -= expired.bytes;
            }
        }
        // Tallying must not hold the cache lock or serialize concurrent misses.
        let (proposal, valid_until) = compute();
        if valid_until.is_some_and(|at| at <= context.now) {
            return proposal;
        }
        let Ok(bytes) = bincode::serialized_size(&proposal) else {
            return proposal;
        };
        if self.max_entries == 0 || bytes > self.max_bytes {
            return proposal;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // A different request may have advanced (or rewound) the context while
        // this tally ran. Its results must not be labelled with our old inputs.
        if entries.context != Some(context) || entries.proposals.contains_key(&hash) {
            return proposal;
        }
        while entries.proposals.len() >= self.max_entries || entries.bytes > self.max_bytes - bytes
        {
            let Some((_, removed)) = entries.proposals.pop_first() else {
                break;
            };
            entries.bytes -= removed.bytes;
        }
        entries.bytes += bytes;
        entries.proposals.insert(
            hash,
            Entry {
                proposal: proposal.clone(),
                bytes,
                computed_at: context.now,
                valid_until,
            },
        );
        proposal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_governance::{GovernableParam, GovernancePhase, ProposalContent};
    use burst_types::WalletAddress;

    fn proposal(n: u8) -> Proposal {
        Proposal {
            hash: TxHash::new([n; 32]),
            proposer: WalletAddress::new("brst_proposer"),
            content: ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 1000,
            },
            phase: GovernancePhase::Proposal,
            created_at: Timestamp::new(100),
            endorsement_count: 0,
            exploration_votes_yea: 0,
            exploration_votes_nay: 0,
            exploration_votes_abstain: 0,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            exploration_started_at: None,
            cooldown_started_at: None,
            promotion_started_at: None,
            activation_at: None,
            total_eligible_voters: 1,
            round: 0,
        }
    }
    fn context() -> Context {
        Context {
            identity: 1,
            uses_projection_identity: false,
            now: Timestamp::new(100),
            params_hash: BlockHash::ZERO,
        }
    }

    #[test]
    fn semantic_identity_cannot_alias_database_generation() {
        let cache = GovernanceProjectionCache::default();
        let c = context();
        let p = proposal(1);
        cache.project_until(Some(c), p.hash, || (p.clone(), None));
        let semantic = Context {
            uses_projection_identity: true,
            ..c
        };
        let mut changed = p.clone();
        changed.round = 7;
        assert_eq!(
            cache.project_until(Some(semantic), p.hash, || (changed.clone(), None)),
            changed
        );
        assert_eq!(
            cache.project_until(Some(semantic), p.hash, || panic!(
                "same input identity recounted"
            )),
            changed
        );
        let different = Context {
            identity: 2,
            ..semantic
        };
        assert_eq!(
            cache.project_until(Some(different), p.hash, || (p.clone(), None)),
            p
        );
    }

    #[test]
    fn reuse_stops_at_deadline_and_rewinds_even_within_the_old_window() {
        let cache = GovernanceProjectionCache::default();
        let c = context();
        let p = proposal(1);
        cache.project_until(Some(c), p.hash, || (p.clone(), Some(Timestamp::new(112))));
        for now in [101, 110, 111] {
            assert_eq!(
                cache.project_until(
                    Some(Context {
                        now: Timestamp::new(now),
                        ..c
                    }),
                    p.hash,
                    || panic!("unnecessary tally before counting")
                ),
                p
            );
        }
        let mut closed = p.clone();
        closed.phase = GovernancePhase::Rejected;
        assert_eq!(
            cache.project_until(
                Some(Context {
                    now: Timestamp::new(112),
                    ..c
                }),
                p.hash,
                || (closed.clone(), None)
            ),
            closed
        );
        assert_eq!(
            cache.project_until(
                Some(Context {
                    now: Timestamp::new(150),
                    ..c
                }),
                p.hash,
                || panic!("terminal result recounted")
            ),
            closed
        );
        assert_eq!(
            cache.project_until(
                Some(Context {
                    now: Timestamp::new(120),
                    ..c
                }),
                p.hash,
                || (p.clone(), None)
            ),
            p
        );
        let newer = Context {
            identity: 2,
            now: Timestamp::new(121),
            ..c
        };
        cache.project_until(Some(newer), p.hash, || (closed.clone(), Some(newer.now)));
        assert_eq!(
            cache.project_until(Some(newer), p.hash, || (p.clone(), None)),
            p,
            "already-due deadline must not retain a result"
        );
    }

    #[test]
    fn identical_inputs_skip_work_but_every_input_change_recomputes() {
        let cache = GovernanceProjectionCache::default();
        let p = proposal(1);
        let c = context();
        assert_eq!(cache.project(Some(c), p.hash, || p.clone()), p);
        assert_eq!(
            cache.project(Some(c), p.hash, || panic!("repeated tally")),
            p
        );
        for next in [
            Context { identity: 2, ..c },
            Context {
                params_hash: BlockHash::new([1; 32]),
                ..c
            },
            Context {
                now: Timestamp::new(101),
                ..c
            },
            c, // backwards clock is a miss too
        ] {
            let mut changed = p.clone();
            changed.round = next.identity as u32 + 2;
            assert_eq!(
                cache.project(Some(next), p.hash, || changed.clone()),
                changed
            );
        }
        let mut unknown = p.clone();
        unknown.round = 50;
        assert_eq!(cache.project(None, p.hash, || unknown.clone()), unknown);
        assert_eq!(cache.project(None, p.hash, || p.clone()), p);
        let other = proposal(2);
        assert_eq!(cache.project(Some(c), other.hash, || other.clone()), other);
    }

    #[test]
    fn late_tally_cannot_replace_another_context() {
        let cache = GovernanceProjectionCache::default();
        let old = proposal(1);
        let mut new = old.clone();
        new.round = 5;
        let c = context();
        let next = Context { identity: 2, ..c };
        let result = cache.project(Some(c), old.hash, || {
            // Reentrant execution also proves computation does not hold the lock.
            cache.project(Some(next), new.hash, || new.clone());
            old.clone()
        });
        assert_eq!(result, old);
        assert_eq!(
            cache.project(Some(next), new.hash, || panic!("lost new result")),
            new
        );
    }

    #[test]
    fn entry_and_payload_limits_evict_and_oversize_bypasses_retention() {
        let p = proposal(1);
        let size = bincode::serialized_size(&p).unwrap();
        for (max_entries, max_bytes) in [(1, size * 10), (10, size)] {
            let cache = GovernanceProjectionCache {
                max_entries,
                max_bytes,
                ..Default::default()
            };
            let c = Some(context());
            cache.project(c, p.hash, || p.clone());
            let other = proposal(2);
            cache.project(c, other.hash, || other.clone());
            assert_eq!(
                cache.project(c, other.hash, || panic!("missing retained result")),
                other
            );
            let mut replacement = p.clone();
            replacement.round = 9;
            assert_eq!(
                cache.project(c, p.hash, || replacement.clone()),
                replacement
            );
            let entries = cache.entries.lock().unwrap();
            assert!(entries.proposals.len() <= max_entries);
            assert!(entries.bytes <= max_bytes);
        }
        let cache = GovernanceProjectionCache {
            max_bytes: size - 1,
            ..Default::default()
        };
        cache.project(Some(context()), p.hash, || p.clone());
        let mut replacement = p.clone();
        replacement.round = 9;
        assert_eq!(
            cache.project(Some(context()), p.hash, || replacement.clone()),
            replacement
        );
        assert!(cache.entries.lock().unwrap().proposals.is_empty());
    }
}
