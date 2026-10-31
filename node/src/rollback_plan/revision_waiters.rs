use super::*;

/// Local proposal guards have exact byte equality semantics. Keep a blocked
/// journal out of the ready scan until its required after-image is restored.
/// Aggregate reconciliation and causal readiness retain their existing rules.
#[derive(Default)]
pub(super) struct RevisionWaiters {
    waiting: BTreeMap<(Vec<u8>, Option<Vec<u8>>), BTreeSet<BlockHash>>,
}

impl RevisionWaiters {
    pub(super) fn park(&mut self, hash: BlockHash, change: &UndoChange) -> bool {
        if change.table != UndoTable::Meta || !change.key.starts_with(b"governance_revision/v1/") {
            return false;
        }
        self.waiting.entry((change.key.clone(), change.after.clone())).or_default().insert(hash);
        true
    }

    pub(super) fn wake(&mut self, change: &UndoChange, ready: &mut BTreeSet<BlockHash>) {
        if change.table != UndoTable::Meta || !change.key.starts_with(b"governance_revision/v1/") {
            return;
        }
        if let Some(waiting) = self.waiting.remove(&(change.key.clone(), change.before.clone())) {
            ready.extend(waiting);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(proposal: u8, before: Option<u128>, after: Option<u128>) -> UndoChange {
        UndoChange { table: UndoTable::Meta,
            key: format!("governance_revision/v1/{}", burst_types::TxHash::new([proposal; 32])).into_bytes(),
            before: before.map(|v| v.to_be_bytes().to_vec()),
            after: after.map(|v| v.to_be_bytes().to_vec()) }
    }

    #[test]
    fn revision_waiters_wake_only_matching_proposals_values_and_absence() {
        let mut waiters = RevisionWaiters::default();
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        assert!(waiters.park(first, &guard(1, None, Some(7))));
        assert!(waiters.park(second, &guard(1, Some(1), None)));
        let mut ready = BTreeSet::new();
        for change in [guard(2, Some(7), Some(8)), guard(1, Some(8), Some(9)), guard(1, Some(0), Some(1))] {
            waiters.wake(&change, &mut ready);
            assert!(ready.is_empty());
        }
        waiters.wake(&guard(1, Some(7), Some(8)), &mut ready);
        assert_eq!(ready, BTreeSet::from([first]));
        ready.clear();
        waiters.wake(&guard(1, Some(7), Some(8)), &mut ready);
        assert!(ready.is_empty());
        waiters.wake(&guard(1, None, Some(1)), &mut ready);
        assert_eq!(ready, BTreeSet::from([second]));
        assert!(waiters.waiting.is_empty());
        for (table, key) in [(UndoTable::BrnMeta, b"governance_engine".to_vec()),
            (UndoTable::Meta, b"governance_revision/v2/unknown".to_vec()),
            (UndoTable::Accounts, guard(1, None, None).key)] {
            let change = UndoChange { table, key, before: None, after: Some(vec![1]) };
            assert!(!waiters.park(first, &change));
        }
    }

    #[test]
    fn reverse_hash_revision_chain_has_linear_candidate_attempts() {
        for count in [16u128, 1024] {
            let rows: BTreeMap<_, _> = (1..=count).map(|value| {
                let mut hash = [0; 32]; hash[16..].copy_from_slice(&value.to_be_bytes());
                (BlockHash::new(hash), guard(1, Some(value-1), Some(value)))
            }).collect();
            let mut ready: BTreeSet<_> = rows.keys().copied().collect();
            let mut waiting = RevisionWaiters::default();
            let mut current = count;
            let mut attempts = 0;
            let mut removed = Vec::new();
            while current != 0 {
                let mut deferred = Vec::new();
                let mut selected = None;
                for hash in &ready {
                    attempts += 1;
                    let row = &rows[hash];
                    if row.after.as_deref() == Some(current.to_be_bytes().as_slice()) {
                        selected = Some(*hash); break;
                    }
                    assert!(waiting.park(*hash, row)); deferred.push(*hash);
                }
                for hash in deferred { ready.remove(&hash); }
                let hash = selected.expect("valid chain must make progress");
                ready.remove(&hash);
                waiting.wake(&rows[&hash], &mut ready);
                removed.push(hash); current -= 1;
            }
            assert_eq!(removed, rows.keys().rev().copied().collect::<Vec<_>>());
            assert_eq!(attempts, 2 * count - 1);
            assert!(ready.is_empty() && waiting.waiting.is_empty());
            // The previous scan restarts at the smallest hash each time.
            assert!(count * (count + 1) / 2 > attempts);
        }
    }
}
