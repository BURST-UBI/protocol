//! Borrowed ordered metadata overlay shared by rollback snapshot adapters.
use super::*;
use std::collections::BTreeMap;

impl LmdbWriteTransaction<'_> {
    /// Merge metadata rows with the caller's overlay in key order, without writing.
    /// Overlay values replace stored values; `None` hides a row. Stored-row checks
    /// remain lazy, and overlay values receive no size or encoding validation here.
    pub(in crate::write_batch) fn checkpoint_metadata_with_overlay<'a>(
        &'a self, overlay: &'a BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
        prefix: &'a [u8],
    ) -> Result<impl Iterator<Item = Result<(&'a [u8], &'a [u8]), StoreError>> + 'a, StoreError> {
        let mut base = self.checkpoint_metadata_entries(prefix)?.peekable();
        let mut staged = overlay.range((UndoTable::Meta, prefix.to_vec())..).take_while(move |((table, key), _)|
            *table == UndoTable::Meta && key.starts_with(prefix)).peekable();
        Ok(std::iter::from_fn(move || {
            loop {
            // Merge borrowed sorted streams. Deletions consume the matching base
            // row; no encoded copy of the full state is retained.
            let use_staged = match (base.peek(), staged.peek()) {
                (Some(Err(_)), _) => return base.next(),
                (Some(Ok((key, _))), Some((( _, staged_key), _))) => staged_key.as_slice() <= *key,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                (None, None) => return None,
            };
            let (key, bytes) = if use_staged {
                let ((_, key), value) = staged.next().unwrap();
                if base.peek().is_some_and(|row| row.as_ref().is_ok_and(|(old, _)| *old == key.as_slice())) {
                    base.next();
                }
                let Some(value) = value else { continue; };
                (key.as_slice(), value.as_slice())
            } else { return base.next(); };
            return Some(Ok((key, bytes)));
            }
        }))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_overlay_merges_borrowed_rows_in_order_and_preserves_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for key in ["scope/a", "scope/c", "scope/e", "scope/z", "scope-other/a"] {
            batch.put_meta(key, key.as_bytes()).unwrap();
        }
        let original = batch.checkpoint_metadata(b"scope/").unwrap();
        let mut overlay = BTreeMap::new();
        for (key, value) in [("scope/a", None), ("scope/b", Some(b"insert".to_vec())),
            ("scope/c", Some(b"replace".to_vec())), ("scope/d", None), ("scope/z", None),
            ("scope-other/a", Some(b"excluded".to_vec()))] {
            overlay.insert((UndoTable::Meta, key.as_bytes().to_vec()), value);
        }
        overlay.insert((UndoTable::BrnMeta, b"scope/e".to_vec()), Some(b"wrong table".to_vec()));
        let actual: Vec<_> = batch.checkpoint_metadata_with_overlay(&overlay, b"scope/").unwrap()
            .map(|row| {
                let (key, value) = row.unwrap();
                if key == b"scope/c" {
                    let staged = overlay.get(&(UndoTable::Meta, key.to_vec())).unwrap().as_ref().unwrap();
                    assert!(std::ptr::eq(value.as_ptr(), staged.as_ptr()));
                }
                (key.to_vec(), value.to_vec())
            }).collect();
        assert_eq!(actual, vec![(b"scope/b".to_vec(), b"insert".to_vec()),
            (b"scope/c".to_vec(), b"replace".to_vec()), (b"scope/e".to_vec(), b"scope/e".to_vec())]);
        assert_eq!(batch.checkpoint_metadata(b"scope/").unwrap(), original);
        assert!(batch.checkpoint_metadata_with_overlay(&overlay, b"absent/").unwrap().next().is_none());
        for (key, _) in original { overlay.insert((UndoTable::Meta, key), None); }
        overlay.insert((UndoTable::Meta, b"scope/b".to_vec()), None);
        assert!(batch.checkpoint_metadata_with_overlay(&overlay, b"scope/").unwrap().next().is_none());
    }
}
