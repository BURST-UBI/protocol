//! Compact ordered reverse edges. Large parents use persistent trees; small
//! parents retain a bounded vector. Snapshots always encode the child sequence.
use burst_types::TxHash;
use imbl::{ordmap::DiffItem, OrdMap};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const SMALL_EDGES: usize = 8;
const INVALID_APPEND: &str =
    "ordinary graph delta removes or reorders ancestry; use checked replacement";

#[derive(Clone, Debug)]
pub(super) struct EdgeList(Repr);

#[derive(Clone, Debug)]
enum Repr {
    Small(Vec<TxHash>),
    Large(Box<TreeEdges>),
}

#[derive(Clone, Debug, Default)]
struct TreeEdges {
    entries: OrdMap<usize, (TxHash, u64)>,
    positions: OrdMap<(TxHash, u64), usize>,
}

impl TreeEdges {
    fn next_position(&self) -> usize {
        self.entries.get_max().map_or(0, |(position, _)| position.checked_add(1)
            .expect("graph edge position space exhausted"))
    }

    fn push(&mut self, child: TxHash) {
        let occurrence = self.positions.range((child, 0)..=(child, u64::MAX))
            .next_back().map_or(0, |((_, occurrence), _)| occurrence.checked_add(1)
                .expect("edge count fits sequence length"));
        let position = self.next_position();
        self.entries.insert(position, (child, occurrence));
        self.positions.insert((child, occurrence), position);
    }
}

impl Default for EdgeList {
    fn default() -> Self {
        Self(Repr::Small(Vec::new()))
    }
}

impl EdgeList {
    pub(super) fn len(&self) -> usize {
        match &self.0 {
            Repr::Small(children) => children.len(),
            Repr::Large(tree) => tree.entries.len(),
        }
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = &TxHash> {
        let (small, large) = match &self.0 {
            Repr::Small(children) => (Some(children.as_slice()), None),
            Repr::Large(tree) => (None, Some(&tree.entries)),
        };
        small.into_iter().flatten().chain(
            large
                .into_iter()
                .flat_map(|entries| entries.values().map(|(child, _)| child)),
        )
    }
    /// Count only this child's indexed occurrences, stopping at the first excess.
    /// Sparse durable positions do not imply sparse occurrence counts.
    pub(super) fn has_multiplicity(&self, child: &TxHash, expected: usize) -> bool {
        match &self.0 {
            Repr::Small(children) => children.iter().filter(|value| *value == child).count() == expected,
            Repr::Large(tree) => tree.positions.range((*child, 0)..=(*child, u64::MAX))
                .take(expected.saturating_add(1)).count() == expected,
        }
    }

    pub(super) fn push(&mut self, child: TxHash) {
        match &mut self.0 {
            Repr::Small(children) if children.len() < SMALL_EDGES => children.push(child),
            Repr::Small(children) => {
                let mut tree = Box::<TreeEdges>::default();
                for child in std::mem::take(children) {
                    tree.push(child);
                }
                tree.push(child);
                self.0 = Repr::Large(tree);
            }
            Repr::Large(tree) => tree.push(child),
        }
    }
    #[cfg(test)]
    pub(super) fn pop(&mut self) -> Option<TxHash> {
        match &mut self.0 {
            Repr::Small(children) => children.pop(),
            Repr::Large(tree) => {
                let position = tree.entries.get_max()?.0;
                let (child, occurrence) = tree.entries.remove(&position)?;
                tree.positions.remove(&(child, occurrence));
                if tree.entries.len() <= SMALL_EDGES {
                    self.0 = Repr::Small(tree.entries.values().map(|(child, _)| *child).collect());
                }
                Some(child)
            }
        }
    }

    /// Remove every occurrence of selected children. Large parents touch only
    /// indexed occurrences and tree paths; retained slots need not be renumbered.
    pub(super) fn remove_children(&mut self, children: &std::collections::HashSet<TxHash>) -> usize {
        let before = self.len();
        match &mut self.0 {
            Repr::Small(edges) => edges.retain(|child| !children.contains(child)),
            Repr::Large(tree) => {
                for child in children {
                    let positions: Vec<_> = tree.positions.range((*child, 0)..=(*child, u64::MAX))
                        .map(|(key, position)| (*key, *position)).collect();
                    for (key, position) in positions {
                        tree.entries.remove(&position);
                        tree.positions.remove(&key);
                    }
                }
                if tree.entries.len() <= SMALL_EDGES {
                    self.0 = Repr::Small(tree.entries.values().map(|(child, _)| *child).collect());
                }
            }
        }
        before - self.len()
    }

    /// Visit appended children after checking the existing prefix. Small prefixes
    /// contain at most eight children. Tree diffs skip shared subtrees; when slot
    /// layouts differ, a full sequence comparison checks the prefix instead.
    /// Callback positions are dense sequence offsets, not internal tree slots.
    /// A callback error stops emission but does not undo earlier callback effects.
    pub(super) fn visit_appended(
        &self,
        before: &Self,
        mut visit: impl FnMut(usize, TxHash, u64) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.len() < before.len() {
            return Err(INVALID_APPEND.into());
        }
        match (&before.0, &self.0) {
            (Repr::Large(old), Repr::Large(new)) => {
                let mut expected = old.next_position();
                let mut appended = Vec::new();
                let mut suffix = true;
                for change in old.entries.diff(&new.entries) {
                    match change {
                        DiffItem::Add(position, (child, occurrence)) if *position == expected => {
                            appended.push((*child, *occurrence));
                            expected = expected.checked_add(1).ok_or("graph edge position space exhausted")?;
                        }
                        _ => { suffix = false; break; }
                    }
                }
                if suffix && appended.len() == self.len() - before.len() {
                    for (offset, (child, occurrence)) in appended.into_iter().enumerate() {
                        visit(before.len() + offset, child, occurrence)?;
                    }
                } else {
                    // Independently restored snapshots compact internal slots.
                    // Compare sequence order when the slot diff is not an append.
                    if !self.iter().take(before.len()).eq(before.iter()) {
                        return Err(INVALID_APPEND.into());
                    }
                    for (position, (child, occurrence)) in new.entries.values().enumerate().skip(before.len()) {
                        visit(position, *child, *occurrence)?;
                    }
                }
            }
            (Repr::Small(old), Repr::Small(new)) => {
                if !new.starts_with(old) {
                    return Err(INVALID_APPEND.into());
                }
                for position in old.len()..new.len() {
                    let child = new[position];
                    let occurrence = new[..position]
                        .iter()
                        .filter(|previous| **previous == child)
                        .count() as u64;
                    visit(position, child, occurrence)?;
                }
            }
            (Repr::Small(old), Repr::Large(new)) => {
                if !self.iter().take(old.len()).eq(old.iter()) {
                    return Err(INVALID_APPEND.into());
                }
                for (position, (child, occurrence)) in new.entries.values().enumerate().skip(old.len()) {
                    visit(position, *child, *occurrence)?;
                }
            }
            (Repr::Large(_), Repr::Small(_)) => return Err(INVALID_APPEND.into()),
        }
        Ok(())
    }
}
impl PartialEq for EdgeList {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        match (&self.0, &other.0) {
            (Repr::Large(a), Repr::Large(b)) => {
                a.entries.ptr_eq(&b.entries) || self.iter().eq(other.iter())
            }
            _ => self.iter().eq(other.iter()),
        }
    }
}
impl Eq for EdgeList {}
impl PartialEq<Vec<TxHash>> for EdgeList {
    fn eq(&self, other: &Vec<TxHash>) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}
impl From<Vec<TxHash>> for EdgeList {
    fn from(mut children: Vec<TxHash>) -> Self {
        if children.len() <= SMALL_EDGES {
            children.shrink_to_fit();
            return Self(Repr::Small(children));
        }
        let mut tree = Box::<TreeEdges>::default();
        for child in children {
            tree.push(child);
        }
        Self(Repr::Large(tree))
    }
}
impl Serialize for EdgeList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for child in self.iter() {
            sequence.serialize_element(child)?;
        }
        sequence.end()
    }
}
impl<'de> Deserialize<'de> for EdgeList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<TxHash>::deserialize(deserializer).map(Self::from)
    }
}
impl crate::snapshot_undo::SnapshotSequence<TxHash> for EdgeList {
    fn sequence_len(&self) -> usize {
        self.len()
    }
    fn sequence<'a>(&'a self) -> impl Iterator<Item = &'a TxHash>
    where
        TxHash: 'a,
    {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn large(edges: &EdgeList) -> &TreeEdges {
        match &edges.0 {
            Repr::Large(tree) => tree,
            _ => panic!("expected large parent"),
        }
    }
    fn tx(id: u64) -> TxHash {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&id.to_le_bytes());
        TxHash::new(bytes)
    }
    #[test]
    fn shared_append_emits_only_suffix_with_exact_repeated_occurrences() {
        let original = EdgeList::from((0..4096).map(tx).collect::<Vec<_>>());
        let mut candidate = original.clone();
        assert!(large(&candidate).entries.ptr_eq(&large(&original).entries));
        assert!(large(&candidate).positions.ptr_eq(&large(&original).positions));
        candidate.push(tx(7));
        candidate.push(tx(7));
        candidate.push(tx(5000));
        let mut emitted = Vec::new();
        candidate
            .visit_appended(&original, |p, c, o| {
                emitted.push((p, c, o));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            emitted,
            vec![(4096, tx(7), 1), (4097, tx(7), 2), (4098, tx(5000), 0)]
        );
        assert_eq!(original.len(), 4096);
        assert_eq!(large(&original).positions.get(&(tx(7), 0)), Some(&7));
        let bytes = bincode::serialize(&candidate).unwrap();
        let legacy: Vec<TxHash> = bincode::deserialize(&bytes).unwrap();
        assert_eq!(legacy, candidate.iter().copied().collect::<Vec<_>>());
        let restored: EdgeList =
            bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        assert_eq!(bincode::serialize(&restored).unwrap(), bytes);
        let mut reloaded_suffix = Vec::new();
        restored
            .visit_appended(&original, |p, c, o| {
                reloaded_suffix.push((p, c, o));
                Ok(())
            })
            .unwrap();
        assert_eq!(emitted, reloaded_suffix);
    }
    #[test]
    fn truncation_reordering_and_replacement_cannot_masquerade_as_append() {
        let before = EdgeList::from(vec![tx(1), tx(2), tx(1)]);
        for values in [
            vec![tx(1), tx(2)],
            vec![tx(2), tx(1), tx(1)],
            vec![tx(1), tx(3), tx(1)],
            vec![],
        ] {
            assert!(EdgeList::from(values)
                .visit_appended(&before, |_, _, _| Ok(()))
                .is_err());
        }
        let mut candidate = before.clone();
        assert_eq!(candidate.pop(), Some(tx(1)));
        candidate.push(tx(1));
        assert_eq!(candidate, before);
        assert!(matches!(candidate.0, Repr::Small(_)));
    }
    #[test]
    fn compact_and_promoted_parents_match_sequence_reference() {
        for length in [0, 1, 7, 8, 9, 31, 4096] {
            let values: Vec<_> = (0..length).map(|n| tx((n % 3) as u64)).collect();
            let before = EdgeList::from(values.clone());
            assert_eq!(matches!(before.0, Repr::Small(_)), length <= SMALL_EDGES);
            let mut after = before.clone();
            let mut reference = values.clone();
            let mut expected = Vec::new();
            for child in [tx(1), tx(1), tx(99)] {
                expected.push((
                    reference.len(),
                    child,
                    reference.iter().filter(|old| **old == child).count() as u64,
                ));
                reference.push(child);
                after.push(child);
            }
            let mut actual = Vec::new();
            after
                .visit_appended(&before, |p, c, o| {
                    actual.push((p, c, o));
                    Ok(())
                })
                .unwrap();
            assert_eq!(actual, expected, "length {length}");
            assert_eq!(before, values);
            assert_eq!(after, reference);
            assert_eq!(
                bincode::serialize(&after).unwrap(),
                bincode::serialize(&reference).unwrap()
            );
            let decoded: EdgeList =
                bincode::deserialize(&bincode::serialize(&reference).unwrap()).unwrap();
            assert_eq!(decoded, after);
            let mut decoded_delta = Vec::new();
            decoded
                .visit_appended(&before, |p, c, o| {
                    decoded_delta.push((p, c, o));
                    Ok(())
                })
                .unwrap();
            assert_eq!(decoded_delta, expected);
        }
    }

    #[test]
    fn promotion_rejects_changed_prefix_and_demotion_keeps_duplicate_counts() {
        let values = vec![tx(1); SMALL_EDGES];
        let before = EdgeList::from(values.clone());
        let mut wrong = values.clone();
        wrong[0] = tx(2);
        wrong.push(tx(1));
        assert!(EdgeList::from(wrong)
            .visit_appended(&before, |_, _, _| Ok(()))
            .is_err());
        let mut promoted = before.clone();
        promoted.push(tx(1));
        assert!(matches!(promoted.0, Repr::Large(_)));
        assert_eq!(promoted.pop(), Some(tx(1)));
        assert!(matches!(promoted.0, Repr::Small(_)));
        assert_eq!(promoted, before);
        promoted.push(tx(1));
        let mut suffix = Vec::new();
        promoted
            .visit_appended(&before, |p, c, o| {
                suffix.push((p, c, o));
                Ok(())
            })
            .unwrap();
        assert_eq!(suffix, vec![(SMALL_EDGES, tx(1), SMALL_EDGES as u64)]);
        assert!(before.visit_appended(&promoted, |_, _, _| Ok(())).is_err());
    }

    #[test]
    fn indexed_removal_preserves_sparse_slots_and_append_occurrences() {
        let values: Vec<_> = (0..4096).map(tx).chain([tx(7), tx(8), tx(7)]).collect();
        let original = EdgeList::from(values.clone());
        let mut removed = original.clone();
        let children = [tx(7), tx(100), tx(3000)].into_iter().collect();
        assert_eq!(removed.remove_children(&children), 5);
        let reference: Vec<_> = values.iter().filter(|child| !children.contains(child)).copied().collect();
        assert_eq!(removed, reference);
        assert_eq!(original, values);
        // Exactly the selected occurrences changed. Remaining entries retain
        // their original slots and persistent subtrees rather than renumbering.
        assert_eq!(large(&original).entries.diff(&large(&removed).entries).count(), 5);
        assert_eq!(large(&original).positions.diff(&large(&removed).positions).count(), 5);
        assert_eq!(large(&removed).positions.get(&(tx(8), 1)), Some(&4097));
        let mut appended = removed.clone();
        appended.push(tx(8));
        appended.push(tx(7));
        let mut suffix = Vec::new();
        appended.visit_appended(&removed, |p, c, o| { suffix.push((p, c, o)); Ok(()) }).unwrap();
        assert_eq!(suffix, vec![(reference.len(), tx(8), 2), (reference.len() + 1, tx(7), 0)]);
        let bytes = bincode::serialize(&appended).unwrap();
        let restored: EdgeList = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored, appended);
        assert_eq!(bincode::serialize(&restored).unwrap(), bytes);
        let mut cold_suffix = Vec::new();
        restored.visit_appended(&removed, |p, c, o| { cold_suffix.push((p, c, o)); Ok(()) }).unwrap();
        assert_eq!(cold_suffix, suffix);
        assert!(removed.visit_appended(&original, |_, _, _| Ok(())).is_err());
    }

    #[test]
    fn repeated_remove_append_and_demotion_match_sequence_reference() {
        let mut reference: Vec<_> = (0..40).map(|n| tx(n % 9)).collect();
        let mut actual = EdgeList::from(reference.clone());
        for round in 0..30 {
            let children: std::collections::HashSet<_> = [tx(round % 9), tx((round + 3) % 9)].into_iter().collect();
            let old = reference.len();
            reference.retain(|child| !children.contains(child));
            assert_eq!(actual.remove_children(&children), old - reference.len());
            assert_eq!(actual, reference);
            let before = actual.clone();
            let mut expected = Vec::new();
            for child in [tx(round % 9), tx(99), tx(round % 9)] {
                expected.push((reference.len(), child, reference.iter().filter(|x| **x == child).count() as u64));
                actual.push(child); reference.push(child);
            }
            let mut suffix = Vec::new();
            actual.visit_appended(&before, |p, c, o| { suffix.push((p, c, o)); Ok(()) }).unwrap();
            assert_eq!(suffix, expected);
            assert_eq!(actual, reference);
            assert_eq!(bincode::serialize(&actual).unwrap(), bincode::serialize(&reference).unwrap());
        }
        let all = reference.iter().copied().collect();
        assert_eq!(actual.remove_children(&all), reference.len());
        assert!(matches!(actual.0, Repr::Small(_)));
        actual.push(tx(1)); actual.push(tx(1));
        assert_eq!(actual, vec![tx(1), tx(1)]);
    }

    #[test]
    fn small_parent_does_not_retain_large_spare_capacity_or_tree_storage() {
        let mut oversized = Vec::with_capacity(4096);
        oversized.push(tx(1));
        let small = EdgeList::from(oversized);
        let Repr::Small(children) = &small.0 else {
            panic!("small parent allocated tree state")
        };
        assert!(children.capacity() <= SMALL_EDGES);
        assert!(
            std::mem::size_of::<EdgeList>()
                <= std::mem::size_of::<Vec<TxHash>>() + 2 * std::mem::size_of::<usize>()
        );
        let clone = small.clone();
        assert_eq!(clone, vec![tx(1)]);
    }
}
