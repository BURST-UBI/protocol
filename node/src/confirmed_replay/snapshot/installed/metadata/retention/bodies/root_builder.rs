use super::*;
use crate::confirmed_replay::scratch_file::ScratchFile;
use blake2::{digest::consts::U32, Blake2b, Digest};
use std::{
    collections::VecDeque,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

type Entry = ([u8; 32], u16);
const BATCH_ROWS: usize = 4096;
const ROW_BYTES: u64 = 34;
const BUFFER_BYTES: usize = 16 * 1024;

pub(super) struct Cancellation {
    check: std::sync::Arc<dyn Fn(&'static str) -> bool + Send + Sync>,
    stopped: std::sync::atomic::AtomicBool,
}
impl Cancellation {
    pub(super) fn new(check: std::sync::Arc<dyn Fn(&'static str) -> bool + Send + Sync>) -> Self {
        Self {
            check,
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub(super) fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn poll(&self, stage: &'static str) -> Result<(), StoreError> {
        if self.stopped() || (self.check)(stage) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(invalid("bulk root compaction cancelled"));
        }
        Ok(())
    }
}
fn poll(
    cancel: &Option<std::sync::Arc<Cancellation>>,
    stage: &'static str,
) -> Result<(), StoreError> {
    if let Some(cancel) = cancel {
        cancel.poll(stage)?;
    }
    Ok(())
}

// Merge sorted runs, keeping at most one run per level and unioning duplicate flags.
// Fully consumed chunks are checked and released during merging; a failed merge
// discards its output.
pub(super) struct RootBuilder {
    directory: PathBuf,
    batch: Vec<Entry>,
    levels: Vec<Option<RunFile>>,
    failed: bool,
    pub(super) cancellation: Option<std::sync::Arc<Cancellation>>,
}
impl RootBuilder {
    pub(super) fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_owned(),
            batch: Vec::new(),
            levels: Vec::new(),
            failed: false,
            cancellation: None,
        }
    }
    pub(super) fn push(&mut self, key: [u8; 32], flags: u16) -> Result<(), StoreError> {
        if self.failed {
            return Err(invalid("root builder already failed"));
        }
        self.failed = true;
        poll(&self.cancellation, "root-builder:push")?;
        validate(key, flags)?;
        self.batch.push((key, flags));
        if self.batch.len() == BATCH_ROWS {
            self.flush()?;
        }
        self.failed = false;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), StoreError> {
        if self.batch.is_empty() {
            return Ok(());
        }
        poll(&self.cancellation, "root-builder:flush")?;
        self.batch.sort_unstable_by_key(|entry| entry.0);
        let mut output = RunWriter::new(&self.directory)?;
        let mut previous: Option<Entry> = None;
        for (key, flags) in self.batch.drain(..) {
            poll(&self.cancellation, "root-builder:flush")?;
            match previous.as_mut() {
                Some((old, sources)) if *old == key => *sources |= flags,
                _ => {
                    if let Some(entry) = previous.take() {
                        insert(&mut output, &mut self.levels, entry)?;
                    }
                    previous = Some((key, flags));
                }
            }
        }
        if let Some(entry) = previous {
            insert(&mut output, &mut self.levels, entry)?;
        }
        let mut run = output.finish()?;
        if run.chunks.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        if std::env::var_os("BURST_ROOT_SORT_DIAGNOSTIC").is_some() {
            let levels: Vec<_> = self
                .levels
                .iter()
                .map(|slot| {
                    slot.as_ref().map_or(0, |run| {
                        run.chunks.iter().map(|chunk| chunk.count).sum::<u64>()
                    })
                })
                .collect();
            eprintln!(
                "ROOT_SORT incoming_rows={} level_rows={levels:?}",
                run.chunks.iter().map(|chunk| chunk.count).sum::<u64>()
            );
        }
        for slot in &mut self.levels {
            match slot.take() {
                Some(old) => run = merge(old, run, &self.directory, &self.cancellation)?,
                None => {
                    *slot = Some(run);
                    return Ok(());
                }
            }
        }
        self.levels.push(Some(run));
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<(RootRows, u64), StoreError> {
        if self.failed {
            return Err(invalid("root builder already failed"));
        }
        self.flush()?;
        let mut combined = None;
        for run in self.levels.into_iter().flatten() {
            combined = Some(match combined {
                None => run,
                Some(old) => merge(old, run, &self.directory, &self.cancellation)?,
            });
        }
        let mut pages = root_pages::Builder::new(&self.directory)?;
        let mut count = 0u64;
        if let Some(file) = combined {
            let mut run = RunReader::new(file)?;
            while let Some((key, flags)) = run.next()? {
                poll(&self.cancellation, "root-builder:index")?;
                pages.push(key, flags)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| invalid("root count overflow"))?;
            }
        }
        Ok((
            RootRows {
                index: ScratchIndex::new(&self.directory)?,
                base: Some(pages.finish()?),
                directory: self.directory,
            },
            count,
        ))
    }
}
fn validate(key: [u8; 32], flags: u16) -> Result<(), StoreError> {
    if key == [0; 32] || flags == 0 || flags & !127 != 0 {
        return Err(invalid("invalid sorted root entry"));
    }
    Ok(())
}
// A root already in a run keeps its slot. New source flags live in a private
// one-byte-per-row overlay until the authenticated run is consumed by a merge
// or final publication. The immutable on-disk bytes still get their full digest
// check; a lookup never authenticates or publishes a root.
fn insert(
    output: &mut RunWriter,
    levels: &mut [Option<RunFile>],
    entry: Entry,
) -> Result<(), StoreError> {
    for run in levels.iter_mut().rev().flatten() {
        if run.union(entry)? {
            return Ok(());
        }
    }
    output.push(entry)
}
struct RunFile {
    chunks: VecDeque<Chunk>,
}
impl RunFile {
    fn union(&mut self, (key, flags): Entry) -> Result<bool, StoreError> {
        let mut low = 0;
        let mut high = self.chunks.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.chunks[middle].last < key {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let Some(chunk) = self.chunks.get_mut(low) else {
            return Ok(false);
        };
        let mut low = 0u64;
        let mut high = chunk.count;
        while low < high {
            let middle = low + (high - low) / 2;
            chunk
                .file
                .seek(SeekFrom::Start(middle * ROW_BYTES))
                .map_err(invalid)?;
            let mut bytes = [0; ROW_BYTES as usize];
            chunk.file.read_exact(&mut bytes).map_err(invalid)?;
            let found = <[u8; 32]>::try_from(&bytes[..32]).map_err(invalid)?;
            let stored = u16::from_be_bytes(bytes[32..].try_into().map_err(invalid)?);
            validate(found, stored)?;
            match found.cmp(&key) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    let old = stored
                        | chunk
                            .flags
                            .as_ref()
                            .map_or(0, |values| u16::from(values[middle as usize]));
                    if (old | flags) != old {
                        let values = chunk
                            .flags
                            .get_or_insert_with(|| vec![0; chunk.count as usize]);
                        values[middle as usize] |= flags as u8;
                    }
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
struct RunWriter {
    directory: PathBuf,
    chunks: VecDeque<Chunk>,
    writer: Option<ChunkWriter>,
    previous: Option<[u8; 32]>,
}
impl RunWriter {
    fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            directory: directory.to_owned(),
            chunks: VecDeque::new(),
            writer: None,
            previous: None,
        })
    }
    fn push(&mut self, entry: Entry) -> Result<(), StoreError> {
        if self.previous.is_some_and(|old| entry.0 <= old) {
            return Err(invalid("unordered root run"));
        }
        if self.writer.is_none() {
            self.writer = Some(ChunkWriter::new(&self.directory)?);
        }
        let writer = self.writer.as_mut().unwrap();
        writer.push(entry)?;
        if writer.count == BATCH_ROWS as u64 {
            self.chunks.push_back(self.writer.take().unwrap().finish()?);
        }
        self.previous = Some(entry.0);
        Ok(())
    }
    fn finish(mut self) -> Result<RunFile, StoreError> {
        if let Some(writer) = self.writer.take() {
            self.chunks.push_back(writer.finish()?);
        }
        Ok(RunFile {
            chunks: self.chunks,
        })
    }
}
struct RunReader {
    chunks: VecDeque<Chunk>,
    reader: Option<ChunkReader>,
    previous: Option<[u8; 32]>,
}
impl RunReader {
    fn new(run: RunFile) -> Result<Self, StoreError> {
        Ok(Self {
            chunks: run.chunks,
            reader: None,
            previous: None,
        })
    }
    fn next(&mut self) -> Result<Option<Entry>, StoreError> {
        loop {
            if self.reader.is_none() {
                let Some(chunk) = self.chunks.pop_front() else {
                    return Ok(None);
                };
                self.reader = Some(ChunkReader::new(chunk)?);
            }
            if let Some(entry) = self.reader.as_mut().unwrap().next()? {
                if self.previous.is_some_and(|old| entry.0 <= old) {
                    return Err(invalid("unordered root chunks"));
                }
                self.previous = Some(entry.0);
                return Ok(Some(entry));
            }
            // next() checks length and digest before the last owner is dropped.
            self.reader = None;
        }
    }
}
struct Chunk {
    file: ScratchFile,
    count: u64,
    digest: [u8; 32],
    last: [u8; 32],
    flags: Option<Vec<u8>>,
}
struct ChunkWriter {
    writer: BufWriter<ScratchFile>,
    count: u64,
    digest: Blake2b<U32>,
    previous: Option<[u8; 32]>,
}
impl ChunkWriter {
    fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            writer: BufWriter::with_capacity(
                BUFFER_BYTES,
                ScratchFile::new_in(directory).map_err(invalid)?,
            ),
            count: 0,
            digest: Blake2b::new(),
            previous: None,
        })
    }
    fn push(&mut self, (key, flags): Entry) -> Result<(), StoreError> {
        validate(key, flags)?;
        if self.previous.is_some_and(|old| key <= old) {
            return Err(invalid("unordered root run"));
        }
        let mut bytes = [0; ROW_BYTES as usize];
        bytes[..32].copy_from_slice(&key);
        bytes[32..].copy_from_slice(&flags.to_be_bytes());
        self.writer.write_all(&bytes).map_err(invalid)?;
        self.digest.update(bytes);
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("root run count overflow"))?;
        self.previous = Some(key);
        Ok(())
    }
    fn finish(self) -> Result<Chunk, StoreError> {
        Ok(Chunk {
            file: self.writer.into_inner().map_err(invalid)?,
            count: self.count,
            digest: self.digest.finalize().into(),
            last: self.previous.ok_or_else(|| invalid("empty root chunk"))?,
            flags: None,
        })
    }
}
struct ChunkReader {
    reader: BufReader<ScratchFile>,
    remaining: u64,
    length: u64,
    digest: Blake2b<U32>,
    expected: [u8; 32],
    previous: Option<[u8; 32]>,
    complete: bool,
    flags: Option<Vec<u8>>,
    position: usize,
}
impl ChunkReader {
    fn new(mut run: Chunk) -> Result<Self, StoreError> {
        let length = run
            .count
            .checked_mul(ROW_BYTES)
            .ok_or_else(|| invalid("root run extent overflow"))?;
        if run.file.metadata().map_err(invalid)?.len() != length {
            return Err(invalid("root run extent changed"));
        }
        run.file.rewind().map_err(invalid)?;
        Ok(Self {
            reader: BufReader::with_capacity(BUFFER_BYTES, run.file),
            remaining: run.count,
            length,
            digest: Blake2b::new(),
            expected: run.digest,
            previous: None,
            complete: false,
            flags: run.flags,
            position: 0,
        })
    }
    fn next(&mut self) -> Result<Option<Entry>, StoreError> {
        if self.complete {
            return Ok(None);
        }
        if self.remaining == 0 {
            if self.reader.get_ref().metadata().map_err(invalid)?.len() != self.length
                || <[u8; 32]>::from(self.digest.clone().finalize()) != self.expected
            {
                return Err(invalid("root run integrity changed"));
            }
            self.complete = true;
            return Ok(None);
        }
        let mut bytes = [0; ROW_BYTES as usize];
        self.reader.read_exact(&mut bytes).map_err(invalid)?;
        self.digest.update(bytes);
        let key = bytes[..32].try_into().map_err(invalid)?;
        let flags = u16::from_be_bytes(bytes[32..].try_into().map_err(invalid)?);
        validate(key, flags)?;
        if self.previous.is_some_and(|old| key <= old) {
            return Err(invalid("unordered root run"));
        }
        self.previous = Some(key);
        self.remaining -= 1;
        let flags = flags
            | self
                .flags
                .as_ref()
                .map_or(0, |values| u16::from(values[self.position]));
        self.position += 1;
        Ok(Some((key, flags)))
    }
}
fn merge(
    first: RunFile,
    second: RunFile,
    directory: &Path,
    cancel: &Option<std::sync::Arc<Cancellation>>,
) -> Result<RunFile, StoreError> {
    poll(cancel, "root-builder:merge")?;
    let mut first = RunReader::new(first)?;
    let mut second = RunReader::new(second)?;
    let mut a = first.next()?;
    let mut b = second.next()?;
    let mut output = RunWriter::new(directory)?;
    while a.is_some() || b.is_some() {
        poll(cancel, "root-builder:merge")?;
        match (a, b) {
            (Some((ka, fa)), Some((kb, fb))) if ka == kb => {
                output.push((ka, fa | fb))?;
                a = first.next()?;
                b = second.next()?;
            }
            (Some(left), Some(right)) if left.0 < right.0 => {
                output.push(left)?;
                a = first.next()?;
            }
            (Some(left), None) => {
                output.push(left)?;
                a = first.next()?;
            }
            (_, Some(right)) => {
                output.push(right)?;
                b = second.next()?;
            }
            (None, None) => break,
        }
    }
    output.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::scratch_file::ScratchLimit;
    use std::io::{SeekFrom, Write};

    #[test]
    fn bulk_sources_match_incremental_union_and_rollback_classification() {
        let directory = tempfile::tempdir().unwrap();
        let mut bulk = Roots::new_bulk(directory.path()).unwrap();
        let mut original = Roots::new(directory.path()).unwrap();
        for n in 1..100u8 {
            let key = BlockHash::new([n; 32]);
            bulk.add(key, Source::State).unwrap();
            original.add(key, Source::State).unwrap();
            bulk.source_override = Some(Source::Undo);
            bulk.add(key, Source::Replay).unwrap();
            bulk.source_override = None;
            original.add(key, Source::Undo).unwrap();
        }
        let bulk = bulk.finish_bulk().unwrap();
        assert_eq!(bulk.count, original.count);
        original
            .rows
            .visit(|key, flags| {
                assert_eq!(bulk.rows.get(&key)?, Some(flags));
                assert_eq!(flags, 33);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn bulk_root_union_fits_measured_headroom_and_keeps_pinned_seeds() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 4 << 20).unwrap();
        let keys: Vec<_> = (0u64..32000)
            .map(|n| BlockHash::new(burst_crypto::blake2b_256(&n.to_be_bytes())))
            .collect();
        let mut roots = Roots::new_bulk(directory.path()).unwrap();
        for source in [Source::State, Source::Work, Source::Frontier] {
            for key in keys.iter().rev() {
                roots.add(*key, source).unwrap();
            }
        }
        assert!(roots.visit(|_| panic!("partial roots escaped")).is_err());
        let mut roots = roots.finish_bulk().unwrap();
        assert_eq!(roots.count, 32000);
        assert_eq!(
            limit.used_temporary_bytes().unwrap(),
            32000 * 34 + 500 * 194 - 102
        );
        for key in &keys {
            assert_eq!(roots.rows.get(key.as_bytes()).unwrap(), Some(7));
        }
        let mut seed = roots.rows.cursor().unwrap();
        roots.add(keys[0], Source::Signer).unwrap();
        assert_eq!(roots.rows.get(keys[0].as_bytes()).unwrap(), Some(15));
        let mut count = 0;
        while !seed
            .visit_next(19, |_, value| {
                assert_eq!(RootRows::decode(value)?, 7);
                count += 1;
                Ok(())
            })
            .unwrap()
            .done
        {}
        assert_eq!(count, 32000);
        drop(seed);
        drop(roots);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    #[ignore = "capped-host receipt-only root-sort diagnosis on retained normalized fixture"]
    fn retained_receipt_root_sort_resource_probe() {
        use crate::verification_commit::retention::{self, Reference};
        let source = std::path::PathBuf::from(
            std::env::var_os("BURST_SNAPSHOT_NODE_BASELINE")
                .expect("set immutable migrated fixture"),
        );
        // Only receipt rows are inspected. No recovery inputs are opened or consumed.
        let ledger = tempfile::tempdir().unwrap();
        std::fs::copy(source.join("data.mdb"), ledger.path().join("data.mdb")).unwrap();
        let store = LmdbStore::open(ledger.path(), 64, crate::node::DEFAULT_MAP_SIZE).unwrap();
        assert_eq!(
            store.tx_begin_write().unwrap().ledger_counts().unwrap().1,
            90_115
        );
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 50_266_112).unwrap();
        let mut resident = ScratchFile::new_in(directory.path()).unwrap();
        resident.set_len(41_390_076).unwrap();
        let mut builder = RootBuilder::new(directory.path());
        let mut unique = std::collections::HashSet::new();
        let mut references = 0u64;
        let mut failure = None;
        let started = std::time::Instant::now();
        let view = store.tx_begin_read().unwrap();
        for prefix in retention::PREFIXES {
            let mut after = None;
            let mut rows = 0u64;
            let mut emitted = 0u64;
            let prior_unique = unique.len();
            loop {
                let page = store.meta_store().visit_prefix_page_txn(&view, prefix,
                    after.as_deref(), 100, 64 << 20, |key, bytes| {
                    retention::visit_row(key, bytes, |reference| {
                        if let Reference::Block(hash) = reference {
                            emitted += 1;
                            references += 1;
                            unique.insert(*hash.as_bytes());
                            assert!(unique.len() <= 1_000_000, "diagnostic distinct-root bound");
                            if failure.is_none() {
                                if let Err(error) = builder.push(*hash.as_bytes(), Source::State as u16) {
                                    eprintln!("ROOT_DIAGNOSTIC_FAILURE prefix={prefix} row={key} references={references} unique={} scratch={} error={error}", unique.len(), limit.used_temporary_bytes().unwrap());
                                    assert!(error.to_string().contains("aggregate replay scratch byte budget exceeded"));
                                    failure = Some(error);
                                }
                            }
                        }
                        Ok(())
                    })
                }).unwrap();
                rows += page.rows as u64;
                if page.done {
                    break;
                }
                after = page.last_key;
            }
            eprintln!("ROOT_DIAGNOSTIC_PREFIX prefix={prefix} rows={rows} references={emitted} new_unique={} total_unique={} scratch={}",
                unique.len() - prior_unique, unique.len(), limit.used_temporary_bytes().unwrap());
        }
        let succeeded = failure.is_none();
        if succeeded {
            let (roots, count) = builder.finish().unwrap();
            assert_eq!(count as usize, unique.len());
            drop(roots);
        } else {
            drop(builder);
        }
        drop(resident);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        eprintln!("ROOT_DIAGNOSTIC_COMPLETE sort_succeeded={succeeded} references={references} unique={} elapsed_ms={} scratch_bytes=0 authentication=false closure=false qualification=false", unique.len(), started.elapsed().as_millis());
    }

    #[test]
    fn chunked_root_merge_fits_retained_recovery_headroom() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 50_266_112).unwrap();
        let mut resident = ScratchFile::new_in(directory.path()).unwrap();
        resident.set_len(41_390_076).unwrap();
        let count = 180_229u64;
        let overlap = 90_114u64;
        let key = |n: u64| burst_crypto::blake2b_256(&n.to_be_bytes());
        let mut builder = RootBuilder::new(directory.path());
        for n in (1..=count).rev() {
            builder.push(key(n), 1).unwrap();
        }
        for n in 1..=overlap {
            builder.push(key(n), 2).unwrap();
        }
        for n in 1..=count {
            builder.push(key(n), 4).unwrap();
        }
        let (rows, actual) = builder.finish().unwrap();
        assert_eq!(actual, count);
        assert_eq!(
            limit.used_temporary_bytes().unwrap(),
            41_390_076 + count * 34 + count.div_ceil(64) * 194 - 102
        );
        let overlapping: std::collections::HashSet<_> = (1..=overlap).map(key).collect();
        let mut seen = 0;
        rows.visit(|hash, flags| {
            assert_eq!(flags, if overlapping.contains(&hash) { 7 } else { 5 });
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, count);
        drop(rows);
        drop(resident);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn duplicate_root_lookups_union_flags_without_hiding_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        let key = |n: u64| {
            let mut key = [0; 32];
            key[24..].copy_from_slice(&n.to_be_bytes());
            key
        };
        for corrupt in [false, true] {
            let mut builder = RootBuilder::new(directory.path());
            for n in 1..=BATCH_ROWS as u64 {
                builder.push(key(n), 1).unwrap();
            }
            for flags in [2, 4, 1, 7] {
                for n in 1..=BATCH_ROWS as u64 {
                    builder.push(key(n), flags).unwrap();
                }
                assert_eq!(
                    limit.used_temporary_bytes().unwrap(),
                    BATCH_ROWS as u64 * ROW_BYTES
                );
            }
            if corrupt {
                let chunk = builder.levels[0]
                    .as_mut()
                    .unwrap()
                    .chunks
                    .front_mut()
                    .unwrap();
                chunk.file.seek(SeekFrom::Start(32)).unwrap();
                chunk.file.write_all(&65u16.to_be_bytes()).unwrap();
                for n in 1..=BATCH_ROWS as u64 {
                    builder.push(key(n), 1).unwrap();
                }
                assert!(
                    builder.finish().is_err(),
                    "deduplication must not hide changed run bytes"
                );
            } else {
                let (rows, count) = builder.finish().unwrap();
                assert_eq!(count, BATCH_ROWS as u64);
                rows.visit(|_, flags| {
                    assert_eq!(flags, 7);
                    Ok(())
                })
                .unwrap();
                drop(rows);
            }
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
    }

    #[test]
    fn consumed_root_chunks_release_storage_and_late_corruption_fails() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        for damage in ["digest", "length", "order"] {
            let mut writer = RunWriter::new(directory.path()).unwrap();
            for n in 1..=(3 * BATCH_ROWS) as u64 {
                let mut key = [0; 32];
                key[24..].copy_from_slice(&n.to_be_bytes());
                writer.push((key, 1)).unwrap();
            }
            let mut file = writer.finish().unwrap();
            match damage {
                "digest" => {
                    let tail = &mut file.chunks.back_mut().unwrap().file;
                    tail.seek(SeekFrom::Start(32)).unwrap();
                    tail.write_all(&3u16.to_be_bytes()).unwrap();
                }
                "length" => file
                    .chunks
                    .back_mut()
                    .unwrap()
                    .file
                    .set_len(ROW_BYTES)
                    .unwrap(),
                _ => file.chunks.swap(1, 2),
            }
            let mut run = RunReader::new(file).unwrap();
            for _ in 0..BATCH_ROWS {
                assert!(run.next().unwrap().is_some());
            }
            assert!(run.next().unwrap().is_some());
            assert_eq!(
                limit.used_temporary_bytes().unwrap(),
                2 * BATCH_ROWS as u64 * ROW_BYTES
            );
            let error = loop {
                match run.next() {
                    Ok(Some(_)) => (),
                    Ok(None) => panic!("damaged {damage} run was accepted"),
                    Err(error) => break error,
                }
            };
            assert!(error.to_string().contains("root"));
            drop(run);
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
    }

    #[test]
    fn bulk_runs_reject_changed_flags_truncation_invalid_input_and_budget_failure() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        for truncated in [false, true] {
            let mut builder = RootBuilder::new(directory.path());
            for n in 1..=BATCH_ROWS as u64 {
                builder
                    .push(burst_crypto::blake2b_256(&n.to_be_bytes()), 1)
                    .unwrap();
            }
            let file = &mut builder.levels[0]
                .as_mut()
                .unwrap()
                .chunks
                .front_mut()
                .unwrap()
                .file;
            if truncated {
                file.set_len(ROW_BYTES).unwrap();
            } else {
                file.seek(SeekFrom::Start(32)).unwrap();
                file.write_all(&5u16.to_be_bytes()).unwrap();
            }
            assert!(builder.finish().is_err());
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
        for (key, flags) in [([0; 32], 1), ([1; 32], 0), ([1; 32], 128)] {
            let mut builder = RootBuilder::new(directory.path());
            assert!(builder.push(key, flags).is_err());
            assert!(builder.push([2; 32], 1).is_err());
            assert!(builder.finish().is_err());
        }
        for phase in ["push-merge", "finish-merge", "finish-index"] {
            use std::sync::{
                atomic::{AtomicBool, AtomicUsize, Ordering},
                Arc,
            };
            let armed = Arc::new(AtomicBool::new(false));
            let polls = Arc::new(AtomicUsize::new(0));
            let check_armed = armed.clone();
            let check_polls = polls.clone();
            let stage = if phase == "finish-index" {
                "root-builder:index"
            } else {
                "root-builder:merge"
            };
            let cancel = Arc::new(Cancellation::new(Arc::new(move |at| {
                at == stage
                    && check_armed.load(Ordering::Relaxed)
                    && check_polls.fetch_add(1, Ordering::Relaxed) >= 2
            })));
            let mut builder = RootBuilder::new(directory.path());
            builder.cancellation = Some(cancel.clone());
            let batches = if phase == "finish-merge" { 3 } else { 1 };
            for n in 1..=(batches * BATCH_ROWS) as u64 {
                builder
                    .push(burst_crypto::blake2b_256(&n.to_be_bytes()), 1)
                    .unwrap();
            }
            armed.store(true, Ordering::Relaxed);
            if phase == "push-merge" {
                for n in BATCH_ROWS + 1..2 * BATCH_ROWS {
                    builder
                        .push(burst_crypto::blake2b_256(&(n as u64).to_be_bytes()), 1)
                        .unwrap();
                }
                assert!(builder
                    .push(
                        burst_crypto::blake2b_256(&(2 * BATCH_ROWS as u64).to_be_bytes()),
                        1
                    )
                    .is_err());
                assert!(
                    builder.push([2; 32], 1).is_err(),
                    "cancelled builder must stay poisoned"
                );
            }
            assert!(
                builder.finish().is_err(),
                "partial roots escaped during {phase}"
            );
            assert!(cancel.stopped(), "cancellation not reached during {phase}");
            assert_eq!(polls.load(Ordering::Relaxed), 3);
            assert_eq!(
                limit.used_temporary_bytes().unwrap(),
                0,
                "scratch leaked during {phase}"
            );
        }
        drop(limit);
        let limit = ScratchLimit::acquire(directory.path(), 16).unwrap();
        let mut builder = RootBuilder::new(directory.path());
        builder.push([1; 32], 1).unwrap();
        assert!(builder.finish().is_err());
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }
}
