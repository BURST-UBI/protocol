//! Bounded writes into a caller-owned, private page namespace.
//! A completed descriptor must be verified with Reader before live publication.
//! Callers must abort the transaction on error and keep the namespace private
//! until publication or until discard has completed.
use super::{invalid, Commitment, Location, MAGIC, PAGE_BYTES};
use burst_store::StoreError;
use burst_store_lmdb::{read_view::LedgerReadView, LmdbWriteTransaction};

pub(crate) const PAGES_PER_BATCH: usize = 16;
const STATE_MAGIC: &[u8; 8] = b"PGSTAGE1";

struct State { expected: Commitment, written_bytes: usize, removed_pages: usize, deleting: bool }

impl State {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = STATE_MAGIC.to_vec();
        bytes.extend_from_slice(&(self.expected.size as u64).to_be_bytes());
        bytes.extend_from_slice(&self.expected.hash);
        bytes.extend_from_slice(&(self.written_bytes as u64).to_be_bytes());
        bytes.extend_from_slice(&(self.removed_pages as u64).to_be_bytes());
        bytes.push(u8::from(self.deleting));
        bytes
    }

    fn decode(bytes: &[u8], max: usize) -> Result<Self, StoreError> {
        if bytes.len() != 65 || &bytes[..8] != STATE_MAGIC || bytes[64] > 1 {
            return Err(invalid("invalid staged page continuation"));
        }
        let number = |at| usize::try_from(u64::from_be_bytes(bytes[at..at+8].try_into().unwrap())).map_err(invalid);
        let expected = Commitment { size: number(8)?, hash: bytes[16..48].try_into().unwrap() };
        let written_bytes = number(48)?;
        let removed_pages = number(56)?;
        let deleting = bytes[64] == 1;
        if expected.size == 0 || expected.size > max || written_bytes > expected.size
            || (written_bytes != expected.size && written_bytes % PAGE_BYTES != 0)
            || removed_pages > written_bytes.div_ceil(PAGE_BYTES) || (!deleting && removed_pages != 0) {
            return Err(invalid("invalid staged page bounds"));
        }
        Ok(Self { expected, written_bytes, removed_pages, deleting })
    }
}

pub(crate) fn progress(view: &LedgerReadView<'_>, state_key: &str,
    expected: Commitment, max: usize) -> Result<usize, StoreError> {
    let Some(bytes) = view.get_meta(state_key)? else { return Ok(0); };
    let state = State::decode(&bytes, max)?;
    if state.deleting || state.expected != expected { return Err(invalid("staged page input changed")); }
    Ok(state.written_bytes)
}

/// Write up to sixteen pages, with a short page only at the end of the stream.
/// Does not check the digest or commit. The caller must verify the completed
/// stream with a pinned Reader before publishing it.
pub(crate) fn append(location: &impl Location, state_key: &str,
    batch: &mut LmdbWriteTransaction<'_>, expected: Commitment, max: usize,
    offset: usize, bytes: &[u8]) -> Result<usize, StoreError> {
    let end = offset.checked_add(bytes.len()).ok_or_else(|| invalid("staged page offset overflow"))?;
    if state_key == location.descriptor_key() || expected.size == 0 || expected.size > max
        || bytes.is_empty() || bytes.len() > PAGES_PER_BATCH * PAGE_BYTES
        || offset % PAGE_BYTES != 0 || end > expected.size
        || (end != expected.size && bytes.len() % PAGE_BYTES != 0) {
        return Err(invalid("invalid staged page batch"));
    }
    let mut state = match batch.get_meta(state_key)? {
        Some(bytes) => State::decode(&bytes, max)?,
        None => State { expected, written_bytes: 0, removed_pages: 0, deleting: false },
    };
    if state.deleting || state.expected != expected || state.written_bytes != offset
        || batch.get_meta(location.descriptor_key())?.is_some() {
        return Err(invalid("staged page cursor or namespace changed"));
    }
    for (index, page) in bytes.chunks(PAGE_BYTES).enumerate() {
        let key = location.page_key(offset / PAGE_BYTES + index);
        if key == state_key || key == location.descriptor_key() || batch.get_meta(&key)?.is_some() {
            return Err(invalid("staged page namespace is occupied"));
        }
        batch.put_meta(&key, page)?;
    }
    state.written_bytes = end;
    if end == expected.size {
        let mut descriptor = MAGIC.to_vec();
        descriptor.extend_from_slice(&(expected.size as u64).to_be_bytes());
        descriptor.extend_from_slice(&expected.hash);
        batch.put_meta(location.descriptor_key(), &descriptor)?;
    }
    batch.put_meta(state_key, &state.encode())?;
    Ok(end)
}

/// Stage deletion of at most sixteen pages and update the continuation cursor.
/// Returns true when cleanup is complete; the caller still must commit.
/// Only for abandoned private namespaces, never for a published live value.
pub(crate) fn discard(location: &impl Location, state_key: &str,
    batch: &mut LmdbWriteTransaction<'_>, max: usize) -> Result<bool, StoreError> {
    discard_inner(location, state_key, batch, max, false)
}

/// A newer live value has taken over this namespace. Preserve its descriptor
/// and page range while retiring only the abandoned staging tail. The descriptor
/// supplies the protected page count; this does not verify the live stream.
pub(crate) fn discard_preserving_live(location: &impl Location, state_key: &str,
    batch: &mut LmdbWriteTransaction<'_>, max: usize) -> Result<bool, StoreError> {
    discard_inner(location, state_key, batch, max, true)
}

fn discard_inner(location: &impl Location, state_key: &str,
    batch: &mut LmdbWriteTransaction<'_>, max: usize, keep_live: bool) -> Result<bool, StoreError> {
    let Some(bytes) = batch.get_meta(state_key)? else { return Ok(true); };
    let mut state = State::decode(&bytes, max)?;
    let protected = if keep_live {
        let descriptor = batch.get_meta(location.descriptor_key())?
            .ok_or_else(|| invalid("live staged namespace lost its descriptor"))?;
        super::page_count(&descriptor, max)?
    } else { 0 };
    let pages = state.written_bytes.div_ceil(PAGE_BYTES);
    let end = (state.removed_pages + PAGES_PER_BATCH).min(pages);
    for index in state.removed_pages..end {
        if index >= protected { batch.delete_meta(&location.page_key(index))?; }
    }
    if !keep_live { batch.delete_meta(location.descriptor_key())?; }
    if end == pages { batch.delete_meta(state_key)?; Ok(true) }
    else {
        state.deleting = true; state.removed_pages = end;
        batch.put_meta(state_key, &state.encode())?; Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paged_metadata::Reader;
    use burst_store_lmdb::LmdbStore;
    use std::io::Read;

    #[derive(Clone, Copy)]
    struct Pages;
    impl Location for Pages {
        fn descriptor_key(&self) -> &str { "staged-test/descriptor" }
        fn page_key(&self, index: usize) -> String { format!("staged-test/pages/{index:08x}") }
    }
    const STATE: &str = "staged-test/state";

    #[test]
    fn abandoned_partial_stream_preserves_new_live_value_and_removes_tail() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let expected = Commitment { size: PAGE_BYTES * 40, hash: [3; 32] };
        let mut batch = store.tx_begin_write().unwrap();
        append(&Pages, STATE, &mut batch, expected, expected.size, 0,
            &vec![3; PAGE_BYTES * 16]).unwrap();
        batch.commit().unwrap();
        // The partial stream has no descriptor, so Writer cannot know its tail.
        let newer = vec![9; PAGE_BYTES + 17];
        let mut batch = store.tx_begin_write().unwrap();
        crate::paged_metadata::write_bytes_at(Pages, &mut batch, &newer).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(discard_preserving_live(&Pages, STATE, &mut batch, expected.size).unwrap());
        for index in 2..16 { assert!(batch.get_meta(&Pages.page_key(index)).unwrap().is_none()); }
        batch.commit().unwrap();
        let txn = store.tx_begin_read().unwrap();
        let mut reader = Reader::open_at(Pages, |key| store.meta_store().get_meta_txn(&txn, key)).unwrap().unwrap();
        let mut actual = Vec::new(); reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, newer);
    }

    #[test]
    fn batches_resume_after_abort_and_reopen_and_discard_in_bounded_steps() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let bytes = vec![7; PAGE_BYTES * 33 + 17];
        let expected = Commitment { size: bytes.len(), hash: burst_crypto::blake2b_256(&bytes) };
        let mut offset = 0;
        for chunk in bytes.chunks(PAGES_PER_BATCH * PAGE_BYTES) {
            { let mut aborted = store.tx_begin_write().unwrap();
                append(&Pages, STATE, &mut aborted, expected, bytes.len(), offset, chunk).unwrap(); }
            let mut batch = store.tx_begin_write().unwrap();
            offset = append(&Pages, STATE, &mut batch, expected, bytes.len(), offset, chunk).unwrap();
            assert_eq!(batch.get_meta(Pages.descriptor_key()).unwrap().is_some(), offset == bytes.len());
            batch.commit().unwrap(); drop(store);
            store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let reader = store.tx_begin_read().unwrap();
            assert_eq!(progress(&store.read_view(&reader).unwrap(), STATE, expected, bytes.len()).unwrap(), offset);
        }
        let pinned = store.tx_begin_read().unwrap();
        let mut reader = Reader::open_at(Pages, |key| store.meta_store().get_meta_txn(&pinned, key)).unwrap().unwrap();
        let mut actual = Vec::new(); reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes); assert!(reader.commitment().unwrap() == expected);
        for done in [false, false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(discard(&Pages, STATE, &mut batch, bytes.len()).unwrap(), done);
            batch.commit().unwrap();
        }
        // Old readers keep the complete bytes across deletion.
        let mut reader = Reader::open_at(Pages, |key| store.meta_store().get_meta_txn(&pinned, key)).unwrap().unwrap();
        actual.clear(); reader.read_to_end(&mut actual).unwrap(); assert_eq!(actual, bytes);
        let batch = store.tx_begin_write().unwrap();
        assert!(batch.get_meta(STATE).unwrap().is_none());
        for index in 0..34 { assert!(batch.get_meta(&Pages.page_key(index)).unwrap().is_none()); }
    }

    #[test]
    fn partial_pages_and_false_commitments_cannot_be_used_as_verified_streams() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let expected = Commitment { size: PAGE_BYTES + 1, hash: [0; 32] };
        let mut batch = store.tx_begin_write().unwrap();
        assert!(append(&Pages, STATE, &mut batch, expected, expected.size, 0, &[1]).is_err());
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        append(&Pages, STATE, &mut batch, expected, expected.size, 0, &vec![1; expected.size]).unwrap();
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        let mut reader = Reader::open_at(Pages, |key| store.meta_store().get_meta_txn(&pinned, key)).unwrap().unwrap();
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        assert!(reader.commitment().is_err());
    }
}
