use crate::Shared;
use imbl::{ordmap::DiffItem, OrdMap};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::HashMap, hash::Hash, ops::Index};

/// Ordered persistent map with individually shared values. Mutation detaches
/// shared tree nodes; mutable access clones a shared value, leaving unrelated
/// values shared. Isolation still depends on each value's Clone implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedMap<K: Ord + Clone, V: Clone>(OrdMap<K, Shared<V>>);

impl<K: Ord + Clone, V: Clone> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self(OrdMap::new())
    }
}

impl<K: Ord + Clone, V: Clone> SharedMap<K, V> {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn contains_key(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.0.get(key).map(|value| &**value)
    }
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.0.get_mut(key).map(|value| &mut **value)
    }
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.0.insert(key, value.into()).map(Shared::into_inner)
    }
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.0.remove(key).map(Shared::into_inner)
    }
    pub fn clear(&mut self) {
        self.0 = OrdMap::new();
    }
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter(self.0.iter())
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.0.keys()
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.values().map(|value| &**value)
    }
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        Entry { map: self, key }
    }
    /// Whether both maps share the same tree root. False does not imply unequal
    /// values; true does not rule out interior mutation within a value.
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl<K: Ord + Clone, V: Clone + PartialEq> SharedMap<K, V> {
    /// Added, removed or unequal keys/values according to PartialEq, skipping
    /// shared subtrees. This is a comparison, not a log of mutation calls.
    pub fn changed_keys<'a>(&'a self, before: &'a Self) -> impl Iterator<Item = &'a K> {
        before.0.diff(&self.0).map(|change| match change {
            DiffItem::Add(key, _) | DiffItem::Remove(key, _) => key,
            DiffItem::Update { new: (key, _), .. } => key,
        })
    }
}

pub struct Entry<'a, K: Ord + Clone, V: Clone> {
    map: &'a mut SharedMap<K, V>,
    key: K,
}
impl<'a, K: Ord + Clone, V: Clone> Entry<'a, K, V> {
    pub fn or_insert_with(self, value: impl FnOnce() -> V) -> &'a mut V {
        if !self.map.contains_key(&self.key) {
            self.map.insert(self.key.clone(), value());
        }
        self.map.get_mut(&self.key).expect("inserted map entry")
    }
    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }
    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}

pub struct Iter<'a, K, V>(imbl::ordmap::Iter<'a, K, Shared<V>, imbl::shared_ptr::DefaultSharedPtr>);
impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, value)| (key, &**value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<'a, K: Ord + Clone, V: Clone> IntoIterator for &'a SharedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<K: Ord + Clone, V: Clone> Index<&K> for SharedMap<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V {
        self.get(key).expect("missing map entry")
    }
}
impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for SharedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(items: T) -> Self {
        Self(
            items
                .into_iter()
                .map(|(key, value)| (key, Shared::from(value)))
                .collect(),
        )
    }
}
impl<K: Ord + Clone + Hash, V: Clone> From<HashMap<K, V>> for SharedMap<K, V> {
    fn from(map: HashMap<K, V>) -> Self {
        map.into_iter().collect()
    }
}
impl<K: Ord + Clone + Serialize, V: Clone + Serialize> Serialize for SharedMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}
impl<'de, K: Ord + Clone + Deserialize<'de>, V: Clone + Deserialize<'de>> Deserialize<'de>
    for SharedMap<K, V>
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        std::collections::BTreeMap::<K, V>::deserialize(deserializer)
            .map(|map| map.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_share_values_and_diff_only_semantic_changes() {
        let before: SharedMap<u64, Vec<u64>> = (0..4096).map(|n| (n, vec![n; 16])).collect();
        let mut after = before.clone();
        assert!(before.shares_storage_with(&after));
        after.get_mut(&10).unwrap().push(99);
        after.remove(&20);
        after.entry(5000).or_default().push(1);
        after.get_mut(&30).unwrap().push(2);
        after.get_mut(&30).unwrap().pop();
        for n in 0..4096 {
            if ![10, 20, 30].contains(&n) {
                assert!(std::ptr::eq(
                    before.get(&n).unwrap(),
                    after.get(&n).unwrap()
                ));
            }
        }
        assert_eq!(before.get(&10).unwrap().len(), 16);
        assert_eq!(
            after.changed_keys(&before).copied().collect::<Vec<_>>(),
            vec![10, 20, 5000]
        );
        let bytes = bincode::serialize(&after).unwrap();
        let legacy: HashMap<u64, Vec<u64>> = bincode::deserialize(&bytes).unwrap();
        let rebuilt: SharedMap<u64, Vec<u64>> = legacy.into();
        assert_eq!(after, rebuilt);
        assert_eq!(
            bincode::deserialize::<SharedMap<u64, Vec<u64>>>(&bytes).unwrap(),
            after
        );
        assert_eq!(
            rebuilt.changed_keys(&before).copied().collect::<Vec<_>>(),
            vec![10, 20, 5000]
        );
    }
}
