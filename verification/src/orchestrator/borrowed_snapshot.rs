//! Borrowed serialization view of verification state. Decoding and restoration
//! use the owned OrchestratorSnapshot representation.
use super::*;
use crate::tracked_map::TrackedMap;

// Field order and names match OrchestratorSnapshot, including non-human-readable
// serializers. TrackedMap iterates wallet keys in the same string order as the
// owned snapshot's ordered_wallet_map; neither tracking nor events are serialized.
#[derive(Serialize)]
pub struct Snapshot<'a> {
    states: WalletMap<'a, VerificationState>,
    active_challenges: WalletMap<'a, Challenge>,
    penalized_verifiers: WalletMap<'a, u64>,
    neither_tracker: &'a NeitherVoteTracker,
    #[serde(serialize_with = "prior_votes")]
    prior_votes: WalletMap<'a, Vec<VerifierVote>>,
    selections: WalletMap<'a, SelectionRound>,
    attempts: &'a AttemptContexts,
    active_appeals: WalletMap<'a, AppealContext>,
}

/// Read-only entries of one persisted snapshot map. Mutation tracking stays private.
/// Entry order depends on the backing map; canonical writers must sort keys.
pub trait SnapshotMap {
    type Key: Serialize;
    type Value: Serialize;
    fn len(&self) -> usize;
    fn get(&self, key: &Self::Key) -> Option<&Self::Value>;
    fn entries(&self) -> impl Iterator<Item = (&Self::Key, &Self::Value)>;
}

struct ReadMap<'a, K: Ord + Clone, V: Clone>(&'a TrackedMap<K, V>);
impl<K: Ord + Clone + Serialize, V: Clone + Serialize> SnapshotMap for ReadMap<'_, K, V> {
    type Key = K;
    type Value = V;
    fn len(&self) -> usize {
        self.0.len()
    }
    fn get(&self, key: &K) -> Option<&V> {
        self.0.get(key)
    }
    fn entries(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter()
    }
}

// Both representations borrow values. Only owned HashMap source serialization
// needs key sorting; canonical consumers sort through their existing writer.
enum WalletMap<'a, V: Clone> {
    Tracked(&'a TrackedMap<WalletAddress, V>),
    Owned(&'a HashMap<WalletAddress, V>),
}
impl<V: Clone> Copy for WalletMap<'_, V> {}
impl<V: Clone> Clone for WalletMap<'_, V> {
    fn clone(&self) -> Self {
        *self
    }
}

enum WalletEntries<'a, V> {
    Tracked(crate::tracked_map::Iter<'a, WalletAddress, V>),
    Owned(std::collections::hash_map::Iter<'a, WalletAddress, V>),
}
impl<'a, V> Iterator for WalletEntries<'a, V> {
    type Item = (&'a WalletAddress, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Tracked(iter) => iter.next(),
            Self::Owned(iter) => iter.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Tracked(iter) => iter.size_hint(),
            Self::Owned(iter) => iter.size_hint(),
        }
    }
}
impl<V> ExactSizeIterator for WalletEntries<'_, V> {}
impl<'a, V: Clone> WalletMap<'a, V> {
    fn iter(&self) -> WalletEntries<'a, V> {
        match self {
            Self::Tracked(map) => WalletEntries::Tracked(map.iter()),
            Self::Owned(map) => WalletEntries::Owned(map.iter()),
        }
    }
}
impl<V: Clone + Serialize> SnapshotMap for WalletMap<'_, V> {
    type Key = WalletAddress;
    type Value = V;
    fn len(&self) -> usize {
        self.iter().len()
    }
    fn get(&self, key: &WalletAddress) -> Option<&V> {
        match self {
            Self::Tracked(map) => map.get(key),
            Self::Owned(map) => map.get(key),
        }
    }
    fn entries(&self) -> impl Iterator<Item = (&WalletAddress, &V)> {
        self.iter()
    }
}
impl<V: Clone + Serialize> Serialize for WalletMap<'_, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Tracked(map) => {
                serializer.collect_map(map.iter().map(|(key, value)| (key.as_str(), value)))
            }
            Self::Owned(map) => crate::state::ordered_wallet_map(map, serializer),
        }
    }
}

impl<'a> Snapshot<'a> {
    pub(super) fn new(state: &'a VerificationOrchestrator) -> Self {
        Self {
            states: WalletMap::Tracked(&state.states),
            active_challenges: WalletMap::Tracked(&state.active_challenges),
            penalized_verifiers: WalletMap::Tracked(&state.penalized_verifiers),
            neither_tracker: &state.neither_tracker,
            prior_votes: WalletMap::Tracked(&state.prior_votes),
            selections: WalletMap::Tracked(&state.selections),
            attempts: &state.attempts,
            active_appeals: WalletMap::Tracked(&state.active_appeals),
        }
    }
    pub(super) fn from_owned(state: &'a OrchestratorSnapshot) -> Self {
        Self {
            states: WalletMap::Owned(&state.states),
            active_challenges: WalletMap::Owned(&state.active_challenges),
            penalized_verifiers: WalletMap::Owned(&state.penalized_verifiers),
            neither_tracker: &state.neither_tracker,
            prior_votes: WalletMap::Owned(&state.prior_votes),
            selections: WalletMap::Owned(&state.selections),
            attempts: &state.attempts,
            active_appeals: WalletMap::Owned(&state.active_appeals),
        }
    }
    pub fn states(&self) -> impl SnapshotMap + 'a {
        self.states
    }
    pub fn challenges(&self) -> impl SnapshotMap + 'a {
        self.active_challenges
    }
    pub fn penalties(&self) -> impl SnapshotMap + 'a {
        self.penalized_verifiers
    }
    pub fn selections(&self) -> impl SnapshotMap + 'a {
        self.selections
    }
    pub fn appeals(&self) -> impl SnapshotMap + 'a {
        self.active_appeals
    }
    pub fn prior_votes(&self) -> impl SnapshotMap<Value = Vec<VerifierVote>> + 'a {
        self.prior_votes
    }
    pub fn vote_contexts(&self) -> impl SnapshotMap + 'a {
        ReadMap(&self.attempts.votes)
    }
    pub fn challenge_contexts(&self) -> impl SnapshotMap + 'a {
        ReadMap(&self.attempts.challenges)
    }
    pub fn neither_history(&self) -> impl SnapshotMap + 'a {
        ReadMap(&self.neither_tracker.history)
    }
    pub fn neither_rules(&self) -> (u32, u32) {
        (
            self.neither_tracker.penalty_threshold_bps,
            self.neither_tracker.window_assignments,
        )
    }
}

fn prior_votes<S: serde::Serializer>(
    values: &WalletMap<'_, Vec<VerifierVote>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match values {
        WalletMap::Tracked(map) => serializer.collect_map(
            map.iter()
                .map(|(wallet, votes)| (wallet.as_str(), ordered_prior_votes(votes))),
        ),
        WalletMap::Owned(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map
                .iter()
                .map(|(wallet, votes)| (wallet.as_str(), votes))
                .collect();
            serializer.collect_map(
                sorted
                    .into_iter()
                    .map(|(wallet, votes)| (wallet, ordered_prior_votes(votes))),
            )
        }
    }
}

/// Serialize prior votes in canonical verifier/timestamp/vote/stake order,
/// rather than their arrival order, when streaming individual map values.
pub fn ordered_prior_votes(votes: &[VerifierVote]) -> impl Serialize + '_ {
    #[derive(Serialize)]
    struct Ordered<'a>(#[serde(serialize_with = "crate::state::ordered_votes")] &'a [VerifierVote]);
    Ordered(votes)
}

#[cfg(test)]
pub(super) fn assert_map_views(state: &VerificationOrchestrator) {
    struct Encoded<M>(M);
    impl<M: SnapshotMap> Serialize for Encoded<M> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(self.0.entries())
        }
    }
    fn check<M: SnapshotMap, K: Ord + Serialize, V: Serialize>(view: M, expected: &HashMap<K, V>) {
        let sorted: std::collections::BTreeMap<_, _> = expected.iter().collect();
        assert_eq!(view.len(), expected.len());
        assert_eq!(
            bincode::serialize(&Encoded(view)).unwrap(),
            bincode::serialize(&sorted).unwrap()
        );
    }
    let view = state.snapshot_view();
    let owned = state.snapshot();
    assert_eq!(
        bincode::serialize(&owned.snapshot_view()).unwrap(),
        bincode::serialize(&owned).unwrap()
    );
    check(view.states(), &owned.states);
    check(view.challenges(), &owned.active_challenges);
    check(view.penalties(), &owned.penalized_verifiers);
    check(view.selections(), &owned.selections);
    check(view.appeals(), &owned.active_appeals);
    check(view.prior_votes(), &owned.prior_votes);
    assert_eq!(
        bincode::serialize(&Encoded(view.vote_contexts())).unwrap(),
        bincode::serialize(&owned.attempts.votes).unwrap()
    );
    assert_eq!(
        bincode::serialize(&Encoded(view.challenge_contexts())).unwrap(),
        bincode::serialize(&owned.attempts.challenges).unwrap()
    );
    assert_eq!(
        bincode::serialize(&Encoded(view.neither_history())).unwrap(),
        bincode::serialize(&owned.neither_tracker.history).unwrap()
    );
    assert_eq!(
        view.neither_rules(),
        (
            owned.neither_tracker.penalty_threshold_bps,
            owned.neither_tracker.window_assignments
        )
    );
}
