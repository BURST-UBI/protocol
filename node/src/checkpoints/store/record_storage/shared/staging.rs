//! Build shared head trees in bounded transactions, retaining unfinished roots.
//! Callers must abort the writer on any error and supply already validated source rows.
use super::{invalid, node};
use crate::ordered_radix::{Builder, Checkpoint};
use burst_store::StoreError;
use burst_store_lmdb::{
    checkpoint_heads::NodeHash, read_view::LedgerReadView, LmdbWriteTransaction,
};

type Row = ([u8; 32], [u8; 40]);
const MAGIC: &[u8; 8] = b"HDBUILD1";
pub(super) const ROWS_PER_BATCH: usize = 128;
const MAX_HEADS: usize = crate::confirmed_history::MAX_BLOCKS as usize;

struct State {
    source: NodeHash,
    total: usize,
    frontier: Checkpoint<NodeHash>,
    root: Option<NodeHash>,
}

fn key(id: &NodeHash) -> String {
    format!(
        "checkpoint_heads/build/v1/{}",
        burst_types::BlockHash::new(*id)
    )
}

impl State {
    fn roots(&self) -> Vec<NodeHash> {
        if let Some(root) = self.root {
            return vec![root];
        }
        self.frontier
            .branches
            .iter()
            .map(|(_, hash)| *hash)
            .chain(self.frontier.pending)
            .collect()
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&self.source);
        out.extend_from_slice(&(self.total as u64).to_be_bytes());
        out.extend_from_slice(&(self.frontier.count as u64).to_be_bytes());
        out.push(u8::from(self.root.is_some()));
        if let Some(root) = self.root {
            out.extend_from_slice(&root);
        } else if self.frontier.count > 0 {
            out.extend_from_slice(&self.frontier.previous.expect("validated previous"));
            out.extend_from_slice(&self.frontier.pending.expect("validated pending"));
            out.extend_from_slice(&(self.frontier.branches.len() as u16).to_be_bytes());
            for (bit, hash) in &self.frontier.branches {
                out.extend_from_slice(&bit.to_be_bytes());
                out.extend_from_slice(hash);
            }
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() < 57 || &bytes[..8] != MAGIC {
            return Err(invalid("invalid head build continuation"));
        }
        let number = |at| {
            usize::try_from(u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap()))
                .map_err(invalid)
        };
        let total = number(40)?;
        let count = number(48)?;
        if total == 0 || total > MAX_HEADS || count > total {
            return Err(invalid("invalid head build count"));
        }
        let source = bytes[8..40].try_into().unwrap();
        let (root, previous, pending, branches) = match bytes[56] {
            1 if count == total && bytes.len() == 89 => {
                (Some(bytes[57..89].try_into().unwrap()), None, None, vec![])
            }
            0 if count == 0 && bytes.len() == 57 => (None, None, None, vec![]),
            0 if count > 0 && count < total && bytes.len() >= 123 => {
                let n = u16::from_be_bytes(bytes[121..123].try_into().unwrap()) as usize;
                if n > 256 || bytes.len() != 123 + n * 34 {
                    return Err(invalid("invalid head build frontier size"));
                }
                let branches = bytes[123..]
                    .chunks_exact(34)
                    .map(|entry| {
                        (
                            u16::from_be_bytes(entry[..2].try_into().unwrap()),
                            entry[2..].try_into().unwrap(),
                        )
                    })
                    .collect();
                (
                    None,
                    Some(bytes[57..89].try_into().unwrap()),
                    Some(bytes[89..121].try_into().unwrap()),
                    branches,
                )
            }
            _ => return Err(invalid("invalid head build phase")),
        };
        let frontier = Checkpoint {
            previous,
            pending,
            branches,
            count,
        };
        if root.is_none() {
            Builder::<[u8; 40], _>::resume(frontier.clone())?;
        }
        Ok(Self {
            source,
            total,
            frontier,
            root,
        })
    }
}

/// Recover the next offset without taking a writer. Pins and continuation must
/// be read from the same snapshot; a completed root still belongs to this job.
pub(super) fn progress(
    view: &LedgerReadView<'_>,
    id: &NodeHash,
    source: NodeHash,
    total: usize,
) -> Result<(usize, Option<NodeHash>), StoreError> {
    let Some(bytes) = view.get_meta(&key(id))? else {
        if !view.checkpoint_head_staging(id)?.is_empty() {
            return Err(invalid("head build ownership without continuation"));
        }
        return Ok((0, None));
    };
    let state = State::decode(&bytes)?;
    if state.source != source
        || state.total != total
        || view.checkpoint_head_staging(id)? != state.roots()
    {
        return Err(invalid("head build input or ownership changed"));
    }
    Ok((state.frontier.count, state.root))
}

/// Returns the next source-row offset and a completed (still staging-owned) root.
/// Process at most 128 rows and emit at most 511 nodes per call.
/// The unfinished tree retains at most 257 roots.
pub(super) fn advance(
    batch: &mut LmdbWriteTransaction<'_>,
    id: &NodeHash,
    source: NodeHash,
    total: usize,
    offset: usize,
    rows: &[Row],
) -> Result<(usize, Option<NodeHash>), StoreError> {
    if total == 0
        || total > MAX_HEADS
        || rows.is_empty()
        || rows.len() > ROWS_PER_BATCH
        || offset.checked_add(rows.len()).is_none_or(|end| end > total)
    {
        return Err(invalid("invalid head build batch budget"));
    }
    let stored = batch.get_meta(&key(id))?;
    let mut state = match stored {
        Some(bytes) => State::decode(&bytes)?,
        None => State {
            source,
            total,
            frontier: Checkpoint {
                previous: None,
                pending: None,
                branches: vec![],
                count: 0,
            },
            root: None,
        },
    };
    if state.source != source
        || state.total != total
        || state.frontier.count != offset
        || state.root.is_some()
    {
        return Err(invalid("head build input or cursor changed"));
    }
    if batch.read_view().checkpoint_head_staging(id)? != state.roots() {
        return Err(invalid("head build frontier ownership differs"));
    }
    let mut builder = Builder::resume(state.frontier)?;
    for (account, value) in rows {
        builder.push(*account, *value, |item| {
            batch.intern_checkpoint_head(&node(item))
        })?;
    }
    state.frontier = builder.checkpoint()?;
    if state.frontier.count == total {
        let (root, count) = builder.finish(|item| batch.intern_checkpoint_head(&node(item)))?;
        if count != total {
            return Err(invalid("head build final count differs"));
        }
        state.root = Some(root.ok_or_else(|| invalid("head build root missing"))?);
    }
    batch.replace_checkpoint_head_staging(id, &state.roots())?;
    batch.put_meta(&key(id), &state.encode())?;
    Ok((state.frontier.count, state.root))
}

pub(super) fn discard(
    batch: &mut LmdbWriteTransaction<'_>,
    id: &NodeHash,
) -> Result<(), StoreError> {
    batch.replace_checkpoint_head_staging(id, &[])?;
    batch.delete_meta(&key(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store_lmdb::LmdbStore;

    fn rows() -> Vec<Row> {
        (0u64..300)
            .map(|i| {
                let mut account = [0; 32];
                account[24..].copy_from_slice(&i.to_be_bytes());
                let mut value = [7; 40];
                value[..8].copy_from_slice(&1u64.to_be_bytes());
                (account, value)
            })
            .collect()
    }

    #[test]
    fn staged_builder_resumes_with_cleanup_and_abort_and_matches_original_builder() {
        let dir = tempfile::tempdir().unwrap();
        let id = [1; 32];
        let source = [2; 32];
        let rows = rows();
        let mut reference = Builder::new();
        for (k, v) in &rows {
            reference.push(*k, *v, |item| node(item).digest()).unwrap();
        }
        let expected = reference
            .finish(|item| node(item).digest())
            .unwrap()
            .0
            .unwrap();
        let mut store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut offset = 0;
        for chunk in rows.chunks(ROWS_PER_BATCH) {
            {
                let mut aborted = store.tx_begin_write().unwrap();
                advance(&mut aborted, &id, source, rows.len(), offset, chunk).unwrap();
            }
            let mut batch = store.tx_begin_write().unwrap();
            let (next, root) = advance(&mut batch, &id, source, rows.len(), offset, chunk).unwrap();
            if next == rows.len() {
                assert_eq!(root, Some(expected));
            }
            batch.commit().unwrap();
            offset = next;
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
            batch.commit().unwrap();
            drop(store);
            store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let reader = store.tx_begin_read().unwrap();
            assert_eq!(
                progress(&store.read_view(&reader).unwrap(), &id, source, rows.len()).unwrap(),
                (offset, (offset == rows.len()).then_some(expected))
            );
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.read_view().checkpoint_head_staging(&id).unwrap(),
            vec![expected]
        );
        batch.retain_checkpoint_head(&expected).unwrap();
        discard(&mut batch, &id).unwrap();
        assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
        batch.release_checkpoint_head(&expected).unwrap();
        batch.commit().unwrap();
        let mut reclaimed = 0;
        loop {
            let mut batch = store.tx_begin_write().unwrap();
            let page = batch.reclaim_checkpoint_heads(256).unwrap();
            reclaimed += page.nodes;
            batch.commit().unwrap();
            if page.complete {
                break;
            }
        }
        assert_eq!(reclaimed, 599);
    }

    #[test]
    fn staging_rejects_changed_source_cursor_budget_and_corrupt_continuation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let id = [1; 32];
        let source = [2; 32];
        let rows = rows();
        let mut batch = store.tx_begin_write().unwrap();
        advance(&mut batch, &id, source, 300, 0, &rows[..128]).unwrap();
        batch.commit().unwrap();
        for mode in 0..4 {
            let mut batch = store.tx_begin_write().unwrap();
            if mode == 3 {
                batch.put_meta(&key(&id), b"broken").unwrap();
            }
            assert!(advance(
                &mut batch,
                &id,
                if mode == 0 { [9; 32] } else { source },
                300,
                if mode == 1 { 0 } else { 128 },
                if mode == 2 {
                    &rows[128..258]
                } else {
                    &rows[128..256]
                }
            )
            .is_err());
        }
    }
}
