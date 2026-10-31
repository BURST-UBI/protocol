use super::{RootRows, StoreError, invalid};
use crate::confirmed_replay::{
    scratch_index::{ScratchCursor, ScanPage},
    scratch_records::{ScratchRecords, SortedRecordsBuilder, RecordsCursor},
};
use std::path::Path;

type Entry = ([u8; 32], u16);
const PAGE_ROWS: usize = 64;
const PAGE_BYTES: u64 = 8 + PAGE_ROWS as u64 * 34;

// Bulk roots are immutable; newly discovered dependencies keep the existing
// mutable index. Both share the directory's aggregate scratch accounting.
pub(super) struct RootPages { pages: ScratchRecords<Vec<Entry>> }
pub(super) struct Builder {
    pages: SortedRecordsBuilder<Vec<Entry>>,
    pending: Vec<Entry>,
    previous: Option<[u8; 32]>,
    failed: bool,
}
impl Builder {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self { pages: SortedRecordsBuilder::new(directory, usize::MAX, u64::MAX)?,
            pending: Vec::new(), previous: None, failed: false })
    }
    pub(super) fn push(&mut self, key: [u8; 32], flags: u16) -> Result<(), StoreError> {
        if self.failed { return Err(invalid("root page builder already failed")); }
        self.failed = true;
        validate(key, flags)?;
        if self.previous.is_some_and(|previous| previous >= key) {
            return Err(invalid("unordered root page input"));
        }
        self.pending.push((key, flags));
        if self.pending.len() == PAGE_ROWS { self.flush()?; }
        self.previous = Some(key);
        self.failed = false;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), StoreError> {
        if let Some((key, _)) = self.pending.last() {
            self.pages.push(*key, &self.pending)?;
            self.pending.clear();
        }
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<RootPages, StoreError> {
        if self.failed { return Err(invalid("root page builder already failed")); }
        self.flush()?;
        Ok(RootPages { pages: self.pages.finish()? })
    }
}
fn validate(key: [u8; 32], flags: u16) -> Result<(), StoreError> {
    if key == [0; 32] || flags == 0 || flags & !127 != 0 {
        return Err(invalid("invalid paged root"));
    }
    Ok(())
}

struct Pages {
    cursor: RecordsCursor<Vec<Entry>>,
    pending: std::iter::Peekable<std::vec::IntoIter<Entry>>,
    previous: Option<[u8; 32]>,
    done: bool,
}
impl Pages {
    // At most one descriptor node and one bounded page per call. Emitting an
    // individual buffered root is separately charged by RootCursor.
    fn advance(&mut self) -> Result<usize, StoreError> {
        let page = self.cursor.visit_next::<StoreError>(1, PAGE_BYTES, |key, rows| {
            if rows.is_empty() || rows.len() > PAGE_ROWS
                || rows.last().is_none_or(|last| last.0 != key)
                || rows.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                || self.previous.is_some_and(|previous| rows[0].0 <= previous) {
                return Err(invalid("invalid root page order or bounds"));
            }
            for (key, flags) in &rows { validate(*key, *flags)?; }
            self.previous = Some(key);
            self.pending = rows.into_iter().peekable();
            Ok(())
        })?;
        self.done = page.done;
        Ok(page.nodes)
    }
}
impl RootPages {
    fn scan(&self, after: Option<[u8; 32]>) -> Result<Pages, StoreError> {
        let cursor = match after {
            Some(key) => self.pages.cursor_after(key)?,
            None => self.pages.cursor()?,
        };
        Ok(Pages { cursor, pending: Vec::new().into_iter().peekable(), previous: None, done: false })
    }
    pub(super) fn get(&self, key: &[u8; 32]) -> Result<Option<u16>, StoreError> {
        let mut before = *key;
        let after = match before.iter().rposition(|byte| *byte != 0) {
            Some(position) => { before[position] -= 1; before[position + 1..].fill(255); Some(before) }
            None => None,
        };
        let mut scan = self.scan(after)?;
        loop {
            for (found, flags) in scan.pending.by_ref() {
                if found >= *key { return Ok((found == *key).then_some(flags)); }
            }
            if scan.done { return Ok(None); }
            scan.advance()?;
        }
    }
    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) { self.pages.truncate_for_test(); }
}

// A root-pinned sorted union. Immutable base flags and later mutable flags are
// unioned once; neither duplicate keys nor roots added after pinning escape.
pub(super) struct RootCursor {
    base: Option<Pages>,
    added: ScratchCursor,
    pending_added: Option<Entry>,
    added_done: bool,
    failed: bool,
}
impl RootCursor {
    pub(super) fn new(base: Option<&RootPages>, added: ScratchCursor) -> Result<Self, StoreError> {
        Ok(Self { base: base.map(|base| base.scan(None)).transpose()?, added,
            pending_added: None, added_done: false, failed: false })
    }
    pub(super) fn visit_next(&mut self, budget: usize,
        mut visit: impl FnMut([u8; 32], [u8; 32]) -> Result<(), StoreError>,
    ) -> Result<ScanPage, StoreError> {
        if self.failed { return Err(invalid("root cursor already failed")); }
        self.failed = true;
        if budget == 0 { return Err(invalid("zero root cursor budget")); }
        let mut page = ScanPage { nodes: 0, rows: 0, done: false };
        while page.nodes < budget {
            if let Some(base) = &mut self.base {
                if base.pending.peek().is_none() && !base.done {
                    page.nodes += base.advance()?;
                    continue;
                }
            }
            if self.pending_added.is_none() && !self.added_done {
                let next = self.added.visit_next(1, |key, value| {
                    let flags = RootRows::decode(value)?;
                    validate(key, flags)?;
                    self.pending_added = Some((key, flags));
                    Ok(())
                })?;
                page.nodes += next.nodes;
                self.added_done = next.done;
                continue;
            }
            let base = self.base.as_mut().and_then(|base| base.pending.peek().copied());
            let added = self.pending_added;
            let next = match (base, added) {
                (None, None) => { page.done = true; break; }
                (Some(left), Some(right)) if left.0 == right.0 => {
                    self.base.as_mut().unwrap().pending.next();
                    self.pending_added = None;
                    (left.0, left.1 | right.1)
                }
                (Some(left), right) if right.is_none_or(|right| left.0 < right.0) => {
                    self.base.as_mut().unwrap().pending.next(); left
                }
                (_, Some(right)) => { self.pending_added = None; right }
                _ => unreachable!("ordered root union exhausted"),
            };
            let mut value = [0; 32];
            value[..2].copy_from_slice(&next.1.to_be_bytes());
            page.nodes += 1;
            page.rows += 1;
            visit(next.0, value)?;
        }
        self.failed = false;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::scratch_index::ScratchIndex;
    fn key(n: u64) -> [u8; 32] {
        let mut key = [0; 32]; key[24..].copy_from_slice(&n.to_be_bytes()); key
    }
    fn collect(cursor: &mut RootCursor) -> Result<Vec<Entry>, StoreError> {
        let mut rows = Vec::new();
        loop {
            let page = cursor.visit_next(1, |key, value| { rows.push((key, RootRows::decode(value)?)); Ok(()) })?;
            assert!(page.nodes <= 1 && page.rows <= 1);
            if page.done { return Ok(rows); }
        }
    }
    #[test]
    fn paged_roots_merge_mutable_flags_in_order_and_pin_both_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = Builder::new(dir.path()).unwrap();
        for n in 1..=131 { builder.push(key(n * 2), 1).unwrap(); }
        let base = builder.finish().unwrap();
        for n in 0..=264 { assert_eq!(base.get(&key(n)).unwrap(), (n > 0 && n <= 262 && n % 2 == 0).then_some(1)); }
        assert!(base.get(&[255;32]).unwrap().is_none());
        let mut rows = RootRows { base: Some(base), index: ScratchIndex::new(dir.path()).unwrap(), directory: dir.path().to_owned() };
        for n in 1..=262 { assert_eq!(rows.put(key(n), &2, true).unwrap(), n % 2 == 0); }
        let mut pinned = rows.cursor().unwrap();
        rows.put(key(2), &4, true).unwrap();
        rows.put(key(263), &8, true).unwrap();
        assert_eq!(rows.get(&key(2)).unwrap(), Some(5));
        assert_eq!(collect(&mut pinned).unwrap(), (1..=262).map(|n| (key(n), if n%2==0 {3} else {2})).collect::<Vec<_>>());
        let mut seen = Vec::new();
        rows.visit(|key, flags| { seen.push((key,flags)); Ok(()) }).unwrap();
        assert_eq!(seen.len(),263);
        assert!(seen.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(seen[1],(key(2),5));
        assert_eq!(seen[262],(key(263),8));
        let mut failed = rows.cursor().unwrap();
        assert!(failed.visit_next(0, |_,_| Ok(())).is_err());
        assert!(failed.visit_next(1, |_,_| Ok(())).is_err());
        let mut callback = rows.cursor().unwrap();
        while let Ok(page) = callback.visit_next(1, |_,_| Err(invalid("rejected callback"))) { assert!(!page.done); }
        assert!(callback.visit_next(1, |_,_| Ok(())).is_err());
    }
    #[test]
    fn paged_roots_reject_invalid_pages_and_payload_damage() {
        let dir = tempfile::tempdir().unwrap();
        for page in [vec![],vec![(key(1),1)],vec![(key(2),1),(key(1),1)],
            vec![(key(2),1),(key(2),1)],vec![(key(2),0)],vec![(key(2),128)],
            vec![([0;32],1),(key(2),1)],(1..=65).map(|n|(key(n),1)).collect()] {
            let mut pages = SortedRecordsBuilder::new(dir.path(),1,1<<20).unwrap();
            pages.push(key(2), &page).unwrap();
            let base = RootPages { pages: pages.finish().unwrap() };
            assert!(base.get(&key(1)).is_err());
            let index = ScratchIndex::new(dir.path()).unwrap();
            let mut cursor = RootCursor::new(Some(&base),index.cursor().unwrap()).unwrap();
            assert!(collect(&mut cursor).is_err());
            assert!(cursor.visit_next(1,|_,_|Ok(())).is_err());
        }
        let mut builder = Builder::new(dir.path()).unwrap();
        for n in 1..=65 { builder.push(key(n),1).unwrap(); }
        let base = builder.finish().unwrap();
        let index = ScratchIndex::new(dir.path()).unwrap();
        let mut cursor = RootCursor::new(Some(&base),index.cursor().unwrap()).unwrap();
        base.truncate_for_test();
        assert!(base.get(&key(1)).is_err());
        assert!(collect(&mut cursor).is_err());
        for (key, flags) in [([0;32],1),(self::key(1),0),(self::key(1),128)] {
            let mut builder = Builder::new(dir.path()).unwrap();
            assert!(builder.push(key,flags).is_err());
            assert!(builder.finish().is_err());
        }
    }
}
