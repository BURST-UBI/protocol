use super::StakeRelease;
use burst_types::BlockHash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, ops::Index, sync::Arc};

/// Outcome-keyed release rows with derived totals and proof indexes. Only rows
/// are serialized; loading rebuilds the indexes without validating the proofs.
/// Mutations maintain the indexes; callers cannot mutate a row through a reference.
/// Snapshots share ordered roots and immutable proof payloads; a mutation copies
/// tree paths and the changed release, not the retained backlog.
#[derive(Clone, Debug, Default)]
pub struct PendingReleases {
    rows: imbl::OrdMap<BlockHash, Arc<StakeRelease>>,
    total: u128,
    carries: u128,
    unindexed: imbl::OrdSet<BlockHash>,
    with_sources: imbl::OrdMap<BlockHash, Arc<StakeRelease>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    #[test]
    fn retained_release_snapshots_share_unchanged_proofs_and_preserve_encoding() {
        let hash = |id: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&id.to_be_bytes());
            BlockHash::new(bytes)
        };
        let rows: BTreeMap<_, _> = (1..=1024)
            .map(|id| {
                (
                    hash(id),
                    StakeRelease {
                        amount: id as u128,
                        sources: vec![hash(id), hash(2000), hash(2001)],
                        completed_at: Timestamp::new(10),
                    },
                )
            })
            .collect();
        let before = PendingReleases::from(rows.clone());
        let bytes = bincode::serialize(&before).unwrap();
        assert_eq!(bytes, bincode::serialize(&rows).unwrap());
        let mut after = before.clone();
        assert!(before.rows.ptr_eq(&after.rows));
        after.insert(
            hash(1),
            StakeRelease {
                amount: 7,
                sources: vec![hash(2000)],
                completed_at: Timestamp::new(11),
            },
        );
        after.remove(&hash(2));
        for id in 3..=1024 {
            assert!(
                Arc::ptr_eq(&before.rows[&hash(id)], &after.rows[&hash(id)]),
                "unchanged release proof was copied"
            );
        }
        assert_eq!(bincode::serialize(&before).unwrap(), bytes);
        assert_eq!(
            after.total_checked(),
            Some(before.total_checked().unwrap() - 3 + 7)
        );
        assert_eq!(
            after.unindexed_outcomes().collect::<Vec<_>>(),
            vec![hash(1)]
        );
        let restored: PendingReleases =
            bincode::deserialize(&bincode::serialize(&after).unwrap()).unwrap();
        assert_eq!(restored, after);
        assert_eq!(restored.total_checked(), after.total_checked());
        assert_eq!(
            restored.has_more_sources(&[hash(2000)]),
            after.has_more_sources(&[hash(2000)])
        );
        let mut sibling = after.clone();
        assert!(sibling.unindexed.ptr_eq(&after.unindexed));
        sibling.remove(&hash(1));
        assert_eq!(after.unindexed.len(), 1);
        assert!(sibling.unindexed.is_empty());
    }
}

impl PartialEq for PendingReleases {
    fn eq(&self, other: &Self) -> bool {
        self.rows.ptr_eq(&other.rows) || self.rows == other.rows
    }
}
impl Eq for PendingReleases {}

impl Index<&BlockHash> for PendingReleases {
    type Output = StakeRelease;
    fn index(&self, key: &BlockHash) -> &Self::Output {
        &self.rows[key]
    }
}

impl From<BTreeMap<BlockHash, StakeRelease>> for PendingReleases {
    fn from(rows: BTreeMap<BlockHash, StakeRelease>) -> Self {
        let mut result = Self {
            rows: rows
                .into_iter()
                .map(|(key, value)| (key, Arc::new(value)))
                .collect(),
            ..Self::default()
        };
        for (outcome, release) in &result.rows {
            let (total, carry) = result.total.overflowing_add(release.amount);
            result.total = total;
            result.carries += u128::from(carry);
            if !release.sources.is_empty() {
                result.with_sources.insert(*outcome, release.clone());
            }
            if !release.sources.is_empty() && !release.sources.contains(outcome) {
                result.unindexed.insert(*outcome);
            }
        }
        result
    }
}

impl Serialize for PendingReleases {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de> Deserialize<'de> for PendingReleases {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(BTreeMap::<BlockHash, StakeRelease>::deserialize(deserializer)?.into())
    }
}

impl PendingReleases {
    /// Visit changed proofs, skipping unchanged shared tree nodes.
    pub fn visit_changes<E>(
        &self,
        before: &Self,
        mut emit: impl FnMut(&BlockHash, Option<&StakeRelease>, Option<&StakeRelease>) -> Result<(), E>,
    ) -> Result<(), E> {
        use imbl::ordmap::DiffItem;
        for change in before.rows.diff(&self.rows) {
            match change {
                DiffItem::Add(key, value) => emit(key, None, Some(value))?,
                DiffItem::Remove(key, value) => emit(key, Some(value), None)?,
                DiffItem::Update {
                    old: (_, old),
                    new: (key, new),
                } => emit(key, Some(old), Some(new))?,
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn contains_key(&self, key: &BlockHash) -> bool {
        self.rows.contains_key(key)
    }
    pub fn get(&self, key: &BlockHash) -> Option<&StakeRelease> {
        self.rows.get(key).map(Arc::as_ref)
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&BlockHash, &StakeRelease)> {
        self.rows.iter().map(|(key, value)| (key, value.as_ref()))
    }
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &BlockHash> {
        self.rows.keys()
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = &StakeRelease> {
        self.rows.values().map(Arc::as_ref)
    }

    // Index nonempty proofs in outcome order so selection skips unproven rows.
    pub(super) fn selectable_values(&self) -> impl ExactSizeIterator<Item = &StakeRelease> {
        self.with_sources.values().map(Arc::as_ref)
    }

    pub(super) fn insert(&mut self, outcome: BlockHash, release: StakeRelease) {
        self.remove(&outcome);
        let (total, carry) = self.total.overflowing_add(release.amount);
        self.total = total;
        self.carries += u128::from(carry);

        if !release.sources.is_empty() && !release.sources.contains(&outcome) {
            self.unindexed.insert(outcome);
        }
        let release = Arc::new(release);
        if !release.sources.is_empty() {
            self.with_sources.insert(outcome, release.clone());
        }
        self.rows.insert(outcome, release);
    }

    pub(super) fn remove(&mut self, outcome: &BlockHash) {
        if let Some(release) = self.rows.remove(outcome) {
            let (total, borrow) = self.total.overflowing_sub(release.amount);
            self.total = total;
            self.carries -= u128::from(borrow);
            self.unindexed.remove(outcome);
            self.with_sources.remove(outcome);
        }
    }

    pub(super) fn has_more_sources(&self, selected: &[BlockHash]) -> bool {
        if self.with_sources.is_empty() {
            return false;
        }
        let indexed = self.with_sources.len() - self.unindexed.len();
        // Each ordinary proof includes its unique map key. More such proofs
        // than selected hashes cannot all be covered.
        if indexed > selected.len() {
            return true;
        }
        if self.rows.len() <= selected.len() {
            // A scan is bounded by the caller's selected length here and avoids
            // extra tree lookups for small or fully covered backlogs.
            return self.rows.values().any(|release| {
                release
                    .sources
                    .iter()
                    .any(|source| selected.binary_search(source).is_err())
            });
        }
        if indexed != 0 {
            let candidates = if selected.windows(2).all(|pair| pair[0] <= pair[1]) {
                std::borrow::Cow::Borrowed(selected)
            } else {
                let mut sorted = selected.to_vec();
                sorted.sort_unstable();
                std::borrow::Cow::Owned(sorted)
            };
            let mut covered = 0;
            let mut previous = None;
            for outcome in candidates.iter() {
                if previous == Some(outcome) {
                    continue;
                }
                previous = Some(outcome);
                if self.unindexed.contains(outcome) {
                    continue;
                }
                let Some(release) = self.rows.get(outcome) else {
                    continue;
                };
                if release.sources.is_empty() {
                    continue;
                }
                // Membership uses binary search on the original slice; callers
                // must supply sorted hashes for a meaningful coverage result.
                if release
                    .sources
                    .iter()
                    .any(|source| selected.binary_search(source).is_err())
                {
                    return true;
                }
                covered += 1;
            }
            if covered != indexed {
                return true;
            }
        }
        // Raw loaded proofs may omit their outcome key, so inspect those rows
        // separately. Normal indexed proofs need no unrelated-row scan.
        self.unindexed.iter().any(|outcome| {
            self.rows[outcome]
                .sources
                .iter()
                .any(|source| selected.binary_search(source).is_err())
        })
    }

    #[cfg(test)]
    pub(super) fn nonempty_count(&self) -> usize {
        self.with_sources.len()
    }

    /// Return the cached sum if it fits u128. Carries are retained so removal
    /// can bring an overflowing total back into range without rescanning rows.
    pub(super) fn total_checked(&self) -> Option<u128> {
        (self.carries == 0).then_some(self.total)
    }

    pub(super) fn unindexed_outcomes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.unindexed.iter().copied()
    }
}
