use burst_store::StoreError;
use burst_store_lmdb::block_undo::{UndoChange, UndoTable};
use burst_types::{Timestamp, TxHash};
use std::collections::BTreeMap;

pub(super) fn mint_rows(
    token: TxHash,
    at: Timestamp,
    expiry_secs: u64,
) -> [(UndoTable, Vec<u8>, Vec<u8>); 3] {
    let mut origin = token.as_bytes().to_vec();
    origin.extend_from_slice(token.as_bytes());
    let expiry = at.as_secs().saturating_add(expiry_secs).to_be_bytes();
    let mut expiry_key = expiry.to_vec();
    expiry_key.extend_from_slice(token.as_bytes());
    let mut reverse = token.as_bytes().to_vec();
    reverse.extend_from_slice(&expiry);
    [
        (UndoTable::TrstOrigins, origin, Vec::new()),
        (UndoTable::TrstExpiry, expiry_key, Vec::new()),
        (UndoTable::TrstReverse, token.as_bytes().to_vec(), reverse),
    ]
}

/// Net row changes for one private transition, not authenticated history or
/// rollback authority. Limits count 65,536 rows and 64 MiB of key/value payload;
/// map overhead, duplicate key storage and temporary allocations are excluded.
#[derive(Clone, Default)]
pub(crate) struct Mutations {
    rows: BTreeMap<(UndoTable, Vec<u8>), UndoChange>,
    bytes: usize,
}

impl Mutations {
    pub(super) fn record_mint(
        &mut self,
        token: TxHash,
        before: Option<Timestamp>,
        after: Timestamp,
        expiry_secs: u64,
    ) -> Result<(), StoreError> {
        let old = before.map(|at| mint_rows(token, at, expiry_secs));
        let new = mint_rows(token, after, expiry_secs);
        for (table, key, value) in old.iter().flatten() {
            let after = new
                .iter()
                .find(|(t, k, _)| t == table && k == key)
                .map(|(_, _, value)| value.clone());
            self.record(*table, key.clone(), Some(value.clone()), after)?;
        }
        for (table, key, value) in new {
            if !old
                .iter()
                .flatten()
                .any(|(t, k, _)| *t == table && *k == key)
            {
                self.record(table, key, None, Some(value))?;
            }
        }
        Ok(())
    }

    /// Preserve the first before-image and latest after-image for a retained row.
    /// Reverting to the initial value removes the row, including its continuity
    /// check. This validates supplied changes, not the database baseline. An
    /// error leaves this recorder unchanged.
    pub(crate) fn record(
        &mut self,
        table: UndoTable,
        key: Vec<u8>,
        before: Option<Vec<u8>>,
        after: Option<Vec<u8>>,
    ) -> Result<(), StoreError> {
        let invalid = |message: &str| StoreError::Corruption(message.into());
        let identity = (table, key.clone());
        let previous = self.rows.get(&identity);
        if previous.is_some_and(|row| row.after != before) {
            return Err(invalid(
                "replay mutation does not extend its preceding value",
            ));
        }
        let first = previous.map_or(before.as_ref(), |row| row.before.as_ref());
        let size = |a: Option<&Vec<u8>>, b: Option<&Vec<u8>>| {
            key.len()
                .checked_add(a.map_or(0, Vec::len))
                .and_then(|bytes| bytes.checked_add(b.map_or(0, Vec::len)))
                .ok_or_else(|| invalid("replay mutation size overflow"))
        };
        let old = previous
            .map(|row| size(row.before.as_ref(), row.after.as_ref()))
            .transpose()?
            .unwrap_or(0);
        let unchanged = first == after.as_ref();
        let new = if unchanged {
            0
        } else {
            size(first, after.as_ref())?
        };
        let bytes = self
            .bytes
            .checked_sub(old)
            .and_then(|bytes| bytes.checked_add(new))
            .filter(|bytes| *bytes <= 64 << 20)
            .ok_or_else(|| invalid("replay mutation byte budget exceeded"))?;
        if previous.is_none() && !unchanged && self.rows.len() >= 65_536 {
            return Err(invalid("replay mutation row budget exceeded"));
        }
        let first = first.cloned();
        if unchanged {
            self.rows.remove(&identity);
        } else {
            self.rows.insert(
                identity,
                UndoChange {
                    table,
                    key,
                    before: first,
                    after,
                },
            );
        }
        self.bytes = bytes;
        Ok(())
    }

    pub(crate) fn changes(&self) -> impl Iterator<Item = &UndoChange> {
        self.rows.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_rows_preserve_storage_keys_and_saturating_expiry() {
        let token = TxHash::new([17; 32]);
        let rows = mint_rows(token, Timestamp::new(u64::MAX - 2), 10);
        assert_eq!(rows[0], (UndoTable::TrstOrigins, vec![17; 64], Vec::new()));
        let mut expiry_key = u64::MAX.to_be_bytes().to_vec();
        expiry_key.extend_from_slice(&[17; 32]);
        assert_eq!(rows[1], (UndoTable::TrstExpiry, expiry_key, Vec::new()));
        let mut reverse = vec![17; 32];
        reverse.extend_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(rows[2], (UndoTable::TrstReverse, vec![17; 32], reverse));
    }

    #[test]
    fn mint_updates_preserve_first_before_and_replace_only_changed_expiry_keys() {
        let token = TxHash::new([18; 32]);
        let mut writes = Mutations::default();
        writes
            .record_mint(token, None, Timestamp::new(10), 20)
            .unwrap();
        writes
            .record_mint(token, Some(Timestamp::new(10)), Timestamp::new(10), 20)
            .unwrap();
        assert_eq!(writes.changes().count(), 3);
        assert!(writes.changes().all(|row| row.before.is_none()));
        let mut replaced = Mutations::default();
        replaced
            .record_mint(token, Some(Timestamp::new(10)), Timestamp::new(11), 20)
            .unwrap();
        let changes: Vec<_> = replaced.changes().collect();
        assert_eq!(changes.len(), 3);
        assert!(!changes
            .iter()
            .any(|row| row.table == UndoTable::TrstOrigins));
        assert_eq!(
            changes
                .iter()
                .filter(|row| row.table == UndoTable::TrstExpiry && row.after.is_none())
                .count(),
            1
        );
        assert_eq!(
            changes
                .iter()
                .filter(|row| row.table == UndoTable::TrstExpiry && row.before.is_none())
                .count(),
            1
        );
        assert_eq!(
            changes
                .iter()
                .filter(|row| row.table == UndoTable::TrstReverse
                    && row.before.is_some()
                    && row.after.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn zero_byte_values_are_not_confused_with_unchanged_rows() {
        let mut writes = Mutations::default();
        writes
            .record(UndoTable::Meta, vec![], None, Some(vec![]))
            .unwrap();
        assert_eq!(writes.changes().count(), 1);
        assert_eq!(writes.changes().next().unwrap().after, Some(vec![]));
        writes
            .record(UndoTable::Meta, vec![], Some(vec![]), None)
            .unwrap();
        assert_eq!(writes.changes().count(), 0);
    }

    #[test]
    fn byte_budget_failure_preserves_preceding_record() {
        let mut writes = Mutations::default();
        writes
            .record(UndoTable::Meta, vec![1], None, Some(vec![2]))
            .unwrap();
        // Simulate other retained rows occupying the rest of the byte budget.
        writes.bytes = 64 << 20;
        assert!(writes
            .record(UndoTable::Meta, vec![1], Some(vec![2]), Some(vec![3, 4]))
            .is_err());
        assert_eq!(writes.bytes, 64 << 20);
        assert_eq!(writes.changes().next().unwrap().after, Some(vec![2]));
    }

    #[test]
    fn mutations_collapse_writes_and_preserve_original_values() {
        let mut writes = Mutations::default();
        let table = UndoTable::Meta;
        writes
            .record(table, b"a".to_vec(), Some(vec![1]), Some(vec![2]))
            .unwrap();
        writes
            .record(table, b"a".to_vec(), Some(vec![2]), Some(vec![3]))
            .unwrap();
        let row = writes.changes().next().unwrap();
        assert_eq!(row.before, Some(vec![1]));
        assert_eq!(row.after, Some(vec![3]));
        assert!(writes
            .record(table, b"a".to_vec(), Some(vec![9]), None)
            .is_err());
        assert_eq!(writes.changes().next().unwrap().after, Some(vec![3]));
        writes
            .record(table, b"a".to_vec(), Some(vec![3]), Some(vec![1]))
            .unwrap();
        writes
            .record(table, b"b".to_vec(), None, Some(vec![4]))
            .unwrap();
        writes
            .record(table, b"b".to_vec(), Some(vec![4]), None)
            .unwrap();
        assert_eq!(writes.changes().count(), 0);
        assert_eq!(writes.bytes, 0);
        writes
            .record(table, b"c".to_vec(), Some(vec![5]), None)
            .unwrap();
        assert_eq!(writes.changes().next().unwrap().before, Some(vec![5]));
    }
}
