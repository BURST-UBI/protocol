//! Persistent map with conservative key tracking for one explicit write scope.
//! Read access cannot bypass mutation tracking; tracking is never serialized.
use imbl::{ordmap::DiffItem, OrdMap, OrdSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{borrow::Borrow, ops::Index, sync::Arc};

// Identity equality is only for conservative change discovery. Detached equal
// payloads must still be visited; callers compare their encoded record images.
#[derive(Clone, Debug)]
struct StoredValue<V>(Arc<V>);
impl<V> PartialEq for StoredValue<V> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl<V> Eq for StoredValue<V> {}

#[derive(Clone, Debug)]
pub(crate) struct TrackedMap<K: Ord + Clone, V: Clone> {
    values: OrdMap<K, StoredValue<V>>,
    changed: OrdSet<K>,
    baseline: Option<OrdMap<K, StoredValue<V>>>,
}
impl<K: Ord + Clone, V: Clone> Default for TrackedMap<K, V> {
    fn default() -> Self {
        Self {
            values: OrdMap::new(),
            changed: OrdSet::new(),
            baseline: None,
        }
    }
}
impl<K: Ord + Clone, V: Clone> TrackedMap<K, V> {
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        self.values.ptr_eq(&other.values)
    }
    pub(crate) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.values.get(key).map(|value| value.0.as_ref())
    }
    pub(crate) fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.values.contains_key(key)
    }
    pub(crate) fn iter(&self) -> Iter<'_, K, V> {
        Iter(self.values.iter())
    }
    pub(crate) fn keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.values.keys()
    }
    pub(crate) fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.values.values().map(|value| value.0.as_ref())
    }
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn untracked_clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            changed: OrdSet::new(),
            baseline: None,
        }
    }
    /// Start a new tracking scope at the current root, discarding earlier tracking.
    pub(crate) fn begin_changes(&mut self) {
        self.changed = OrdSet::new();
        self.baseline = Some(self.values.clone());
    }
    pub(crate) fn replacement_scope(&mut self, before: &Self) {
        self.changed = OrdSet::new();
        for change in before.values.diff(&self.values) {
            let key = match change {
                DiffItem::Add(key, _) | DiffItem::Remove(key, _) => key,
                DiffItem::Update { new: (key, _), .. } => key,
            };
            self.changed.insert(key.clone());
        }
        self.baseline = Some(before.values.clone());
    }
    /// Check baseline root identity, not equality of decoded map contents.
    pub(crate) fn scope_matches(&self, before: &Self) -> bool {
        self.baseline
            .as_ref()
            .is_some_and(|baseline| baseline.ptr_eq(&before.values))
    }
    /// Keys that may differ from the baseline, including unchanged mutable borrows
    /// and changes later reversed. Callers compare values before emitting writes.
    pub(crate) fn changed_keys(&self) -> impl Iterator<Item = &K> {
        self.changed.iter()
    }
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.changed.insert(key.clone());
        self.values
            .insert(key, StoredValue(Arc::new(value)))
            .map(|value| Arc::unwrap_or_clone(value.0))
    }
    pub(crate) fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let owned = self.values.get_key_value(key)?.0.clone();
        self.changed.insert(owned);
        self.values
            .get_mut(key)
            .map(|value| Arc::make_mut(&mut value.0))
    }
    pub(crate) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let owned = self.values.get_key_value(key)?.0.clone();
        self.changed.insert(owned);
        self.values
            .remove(key)
            .map(|value| Arc::unwrap_or_clone(value.0))
    }
    pub(crate) fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        Entry { map: self, key }
    }
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let removed: Vec<_> = self
            .values
            .iter()
            .filter(|(k, v)| !keep(k, v.0.as_ref()))
            .map(|(k, _)| k.clone())
            .collect();
        for key in removed {
            self.remove(&key);
        }
    }
}
pub(crate) struct Entry<'a, K: Ord + Clone, V: Clone> {
    map: &'a mut TrackedMap<K, V>,
    key: K,
}
impl<'a, K: Ord + Clone, V: Clone> Entry<'a, K, V> {
    pub(crate) fn or_insert_with(self, value: impl FnOnce() -> V) -> &'a mut V {
        if !self.map.contains_key(&self.key) {
            self.map.insert(self.key.clone(), value());
        }
        self.map.get_mut(&self.key).expect("inserted tracked entry")
    }
    pub(crate) fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}
impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for TrackedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(items: T) -> Self {
        Self {
            values: items
                .into_iter()
                .map(|(key, value)| (key, StoredValue(Arc::new(value))))
                .collect(),
            changed: OrdSet::new(),
            baseline: None,
        }
    }
}
impl<K: Ord + Clone + Serialize, V: Clone + Serialize> Serialize for TrackedMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}
impl<'de, K: Ord + Clone + Deserialize<'de>, V: Clone + Deserialize<'de>> Deserialize<'de>
    for TrackedMap<K, V>
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        OrdMap::<K, V>::deserialize(deserializer).map(|values| values.into_iter().collect())
    }
}
impl<'a, K: Ord + Clone, V: Clone> IntoIterator for &'a TrackedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(crate) struct Iter<'a, K, V>(
    imbl::ordmap::Iter<'a, K, StoredValue<V>, imbl::shared_ptr::DefaultSharedPtr>,
);
impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, value)| (key, value.0.as_ref()))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}
impl<K: Ord + Clone, V: Clone> Index<&K> for TrackedMap<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V {
        self.get(key).expect("tracked map key missing")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Payload {
        copies: Arc<AtomicUsize>,
        value: usize,
    }
    impl Clone for Payload {
        fn clone(&self) -> Self {
            self.copies.fetch_add(1, Ordering::Relaxed);
            Self {
                copies: self.copies.clone(),
                value: self.value,
            }
        }
    }

    #[test]
    fn mutations_clone_only_the_touched_value_and_preserve_scope() {
        for count in [128, 1024, 10_000] {
            let copies = Arc::new(AtomicUsize::new(0));
            let before: TrackedMap<_, _> = (0..count)
                .map(|n| {
                    (
                        n,
                        Payload {
                            copies: copies.clone(),
                            value: n,
                        },
                    )
                })
                .collect();
            let mut after = before.untracked_clone();
            after.begin_changes();
            assert_eq!(copies.load(Ordering::Relaxed), 0);
            after.get_mut(&(count / 2)).unwrap().value = count;
            assert_eq!(
                copies.load(Ordering::Relaxed),
                1,
                "unrelated leaf payloads must stay shared"
            );
            assert_eq!(before.get(&(count / 2)).unwrap().value, count / 2);
            assert_eq!(after.get(&(count / 2)).unwrap().value, count);
            assert!(after.scope_matches(&before));
            assert_eq!(
                after.changed_keys().copied().collect::<Vec<_>>(),
                vec![count / 2]
            );
            // Repeated writes in the same detached scope need no second clone.
            after.get_mut(&(count / 2)).unwrap().value += 1;
            assert_eq!(copies.load(Ordering::Relaxed), 1);
            after.insert(
                count + 1,
                Payload {
                    copies: copies.clone(),
                    value: 1,
                },
            );
            assert_eq!(copies.load(Ordering::Relaxed), 1);
            after.remove(&0).unwrap();
            assert_eq!(copies.load(Ordering::Relaxed), 2);
        }
    }

    #[test]
    fn independently_shared_values_keep_the_old_map_wire_shape() {
        let old: OrdMap<u32, Vec<u32>> = (0..128).map(|n| (n, vec![n, n + 1])).collect();
        let bytes = bincode::serialize(&old).unwrap();
        let shared: TrackedMap<u32, Vec<u32>> = bincode::deserialize(&bytes).unwrap();
        assert_eq!(bincode::serialize(&shared).unwrap(), bytes);
        assert_eq!(shared.iter().count(), old.len());
        assert!(shared.baseline.is_none());
        assert_eq!(shared.changed_keys().count(), 0);
    }

    #[test]
    fn replacement_scope_skips_shared_subtrees_without_comparing_payloads() {
        #[derive(Clone, Debug)]
        struct Key {
            value: usize,
            comparisons: Arc<AtomicUsize>,
        }
        impl PartialEq for Key {
            fn eq(&self, other: &Self) -> bool {
                self.value == other.value
            }
        }
        impl Eq for Key {}
        impl PartialOrd for Key {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for Key {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.comparisons.fetch_add(1, Ordering::Relaxed);
                self.value.cmp(&other.value)
            }
        }
        for count in [1024, 16_384] {
            let comparisons = Arc::new(AtomicUsize::new(0));
            let copies = Arc::new(AtomicUsize::new(0));
            let key = |value| Key {
                value,
                comparisons: comparisons.clone(),
            };
            // Payload deliberately has no PartialEq implementation.
            let before: TrackedMap<_, _> = (0..count)
                .map(|n| {
                    (
                        key(n),
                        Payload {
                            copies: copies.clone(),
                            value: n,
                        },
                    )
                })
                .collect();
            let mut after = before.untracked_clone();
            after.get_mut(&key(count / 2)).unwrap().value += 1;
            comparisons.store(0, Ordering::Relaxed);
            after.replacement_scope(&before);
            let visited = comparisons.load(Ordering::Relaxed);
            assert!(
                visited < count / 2,
                "shared subtrees were scanned: {visited}/{count}"
            );
            assert!(after.scope_matches(&before));
            assert_eq!(
                after
                    .changed_keys()
                    .map(|key| key.value)
                    .collect::<Vec<_>>(),
                vec![count / 2]
            );
            assert_eq!(copies.load(Ordering::Relaxed), 1);
            eprintln!("replacement scope: {count} keys, {visited} comparisons, one payload clone");
            after.remove(&key(0));
            after.insert(
                key(count),
                Payload {
                    copies: copies.clone(),
                    value: count,
                },
            );
            after.replacement_scope(&before);
            assert_eq!(
                after
                    .changed_keys()
                    .map(|key| key.value)
                    .collect::<Vec<_>>(),
                vec![0, count / 2, count]
            );
            let mut same = before.untracked_clone();
            same.replacement_scope(&before);
            assert_eq!(same.changed_keys().count(), 0);
        }
        let before: TrackedMap<_, _> = [(1, vec![2]), (2, vec![3])].into_iter().collect();
        let bytes = bincode::serialize(&before).unwrap();
        let mut detached: TrackedMap<i32, Vec<i32>> = bincode::deserialize(&bytes).unwrap();
        detached.replacement_scope(&before);
        assert_eq!(
            detached.changed_keys().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(bincode::serialize(&detached).unwrap(), bytes);
    }
}
