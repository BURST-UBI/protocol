use imbl::{ordset::DiffItem, OrdSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Ordered persistent set. Clones share tree nodes until mutation.
#[derive(Clone, Debug)]
pub struct SharedSet<K: Ord + Clone>(OrdSet<K>);

impl<K: Ord + Clone> Default for SharedSet<K> {
    fn default() -> Self {
        Self(OrdSet::new())
    }
}
impl<K: Ord + Clone> SharedSet<K> {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn contains(&self, key: &K) -> bool {
        self.0.contains(key)
    }
    pub fn insert(&mut self, key: K) -> bool {
        self.0.insert(key).is_none()
    }
    pub fn remove(&mut self, key: &K) -> bool {
        self.0.remove(key).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = &K> {
        self.0.iter()
    }
    /// Ordered range traversal without walking keys preceding the lower bound.
    pub fn range(&self, bounds: impl std::ops::RangeBounds<K>) -> impl Iterator<Item = &K> {
        self.0.range(bounds)
    }
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
    /// Keys added or removed between these sets, skipping shared tree nodes.
    pub fn changed_keys<'a>(&'a self, before: &'a Self) -> impl Iterator<Item = &'a K> {
        before.0.diff(&self.0).map(|change| match change {
            DiffItem::Add(key) | DiffItem::Remove(key) => key,
        })
    }
}
impl<K: Ord + Clone> Extend<K> for SharedSet<K> {
    fn extend<T: IntoIterator<Item = K>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}
impl<K: Ord + Clone> FromIterator<K> for SharedSet<K> {
    fn from_iter<T: IntoIterator<Item = K>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}
impl<K: Ord + Clone + Serialize> Serialize for SharedSet<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}
impl<'de, K: Ord + Clone + Deserialize<'de>> Deserialize<'de> for SharedSet<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        std::collections::BTreeSet::<K>::deserialize(deserializer)
            .map(|set| set.into_iter().collect())
    }
}

impl<K: Ord + Clone> PartialEq for SharedSet<K> {
    fn eq(&self, other: &Self) -> bool {
        self.shares_storage_with(other)
            || (self.len() == other.len() && self.0.diff(&other.0).next().is_none())
    }
}
impl<K: Ord + Clone> Eq for SharedSet<K> {}

impl<'a, K: Ord + Clone> IntoIterator for &'a SharedSet<K> {
    type Item = &'a K;
    type IntoIter = imbl::ordset::Iter<'a, K, imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_set_changes_and_legacy_sequence_encoding_agree() {
        let before: SharedSet<u64> = (0..10_000).collect();
        let mut after = before.clone();
        assert!(after.shares_storage_with(&before));
        after.remove(&5_000);
        after.insert(10_000);
        assert_eq!(after.len(), before.len());
        assert_ne!(before, after);
        assert_eq!(
            after.changed_keys(&before).copied().collect::<Vec<_>>(),
            vec![5_000, 10_000]
        );
        assert!(before.contains(&5_000));
        let legacy: std::collections::HashSet<u64> = (0..10_000).collect();
        let decoded: SharedSet<u64> =
            bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        assert_eq!(decoded, before);
        let roundtrip: std::collections::HashSet<u64> =
            bincode::deserialize(&bincode::serialize(&before).unwrap()).unwrap();
        assert_eq!(roundtrip, legacy);
        let rebuilt: SharedSet<_> = after.iter().copied().collect();
        assert_eq!(rebuilt, after);
    }
}
