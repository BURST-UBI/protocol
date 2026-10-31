//! Ordered committee membership with a shared, derived lookup.
use crate::shared::Shared;
use burst_types::WalletAddress;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

/// Ordered members with a lazily built membership set. Construction and
/// deserialization preserve duplicates; callers enforce committee uniqueness.
/// Clones share storage until mutation, which also invalidates the lookup.
#[derive(Clone, Debug, Default)]
pub struct Committee {
    members: Shared<Vec<WalletAddress>>,
    lookup: Arc<OnceLock<HashSet<WalletAddress>>>,
}
impl Committee {
    pub fn contains(&self, wallet: &WalletAddress) -> bool {
        self.lookup
            .get_or_init(|| self.members.iter().cloned().collect())
            .contains(wallet)
    }
    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        self.members.shares_storage_with(&other.members)
    }
}
impl From<Vec<WalletAddress>> for Committee {
    fn from(members: Vec<WalletAddress>) -> Self {
        Self {
            members: members.into(),
            lookup: Arc::default(),
        }
    }
}
impl FromIterator<WalletAddress> for Committee {
    fn from_iter<I: IntoIterator<Item = WalletAddress>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}
impl Deref for Committee {
    type Target = Vec<WalletAddress>;
    fn deref(&self) -> &Self::Target {
        &self.members
    }
}
impl DerefMut for Committee {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.lookup = Arc::default();
        &mut self.members
    }
}
impl PartialEq for Committee {
    fn eq(&self, other: &Self) -> bool {
        self.members == other.members
    }
}
impl Eq for Committee {}
impl Serialize for Committee {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.members.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Committee {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<WalletAddress>::deserialize(deserializer).map(Self::from)
    }
}
impl<'a> IntoIterator for &'a Committee {
    type Item = &'a WalletAddress;
    type IntoIter = std::slice::Iter<'a, WalletAddress>;
    fn into_iter(self) -> Self::IntoIter {
        self.members.iter()
    }
}
impl IntoIterator for Committee {
    type Item = WalletAddress;
    type IntoIter = std::vec::IntoIter<WalletAddress>;
    fn into_iter(self) -> Self::IntoIter {
        self.members.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wallet(n: usize) -> WalletAddress {
        WalletAddress::new(format!("brst_committee_{n:06}"))
    }
    #[test]
    fn shared_lookup_preserves_order_bytes_duplicates_and_mutation_isolation() {
        for count in [128, 1024, 10_000] {
            let ordered: Vec<_> = (0..count).rev().map(wallet).collect();
            let before: Committee = ordered.clone().into();
            let mut after = before.clone();
            assert!(before.shares_storage_with(&after));
            assert!(Arc::ptr_eq(&before.lookup, &after.lookup));
            assert!(before.lookup.get().is_none());
            assert!(after.contains(&wallet(0)));
            assert!(before.lookup.get().is_some());
            assert!(!after.contains(&wallet(count)));
            assert_eq!(
                bincode::serialize(&before).unwrap(),
                bincode::serialize(&ordered).unwrap()
            );
            let restored: Committee =
                bincode::deserialize(&bincode::serialize(&ordered).unwrap()).unwrap();
            assert_eq!(*restored, ordered);
            assert!(restored.lookup.get().is_none());
            assert!(restored.contains(&wallet(0)));
            after[0] = wallet(count);
            assert!(!Arc::ptr_eq(&before.lookup, &after.lookup));
            assert!(!after.shares_storage_with(&before));
            assert!(!after.contains(&wallet(count - 1)));
            assert!(after.contains(&wallet(count)));
            assert!(before.contains(&wallet(count - 1)));
            assert!(!before.contains(&wallet(count)));
            after.reverse();
            assert!(after.contains(&wallet(count)));
            after.push(wallet(count));
            assert_eq!(after.iter().filter(|v| **v == wallet(count)).count(), 2);
            after.retain(|v| *v != wallet(count));
            assert!(!after.contains(&wallet(count)));
            after.clear();
            assert!(!after.contains(&wallet(0)));
            assert_eq!(*before, ordered);
        }
    }
}
