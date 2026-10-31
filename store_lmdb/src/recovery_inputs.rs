//! Durable, bounded raw recovery inputs in a dedicated LMDB environment.
//! Digests establish byte identity only. Reopened inputs must pass the normal
//! snapshot/history authenticators; this store cannot restore proof authority.
use crate::shared_env::SharedEnv;
use burst_store::StoreError;
use heed::{types::Bytes, Database, EnvOpenOptions, RwTxn};
use std::path::Path;

const MAX_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_BYTES: u64 = 64 << 20;
const MAX_CHUNKS: u64 = 131_072;
const HEADER: &[u8] = b"BURST/recovery-inputs/v5";
// Keep existing raw chunk identities valid during the v4 ownership migration.
const DIGEST_DOMAIN: &[u8] = b"BURST/recovery-inputs/v4";
#[cfg(test)]
mod capacity_tests;
mod stream_reset;
#[cfg(test)]
thread_local! { static DIGEST_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub chunks: u64,
    pub bytes: u64,
    /// Includes LMDB/index overhead, separate from the logical payload budget.
    pub map_bytes: usize,
}

/// Capacity means the transaction was abandoned: a logical limit, or LMDB's
/// MapFull during staging or commit. Other failures remain `Storage` errors.
/// A sync error can occur after commit, so it does not imply that progress was
/// rolled back; inspect the saved state before retrying an append.
#[derive(Debug, thiserror::Error)]
pub enum InputWriteError {
    #[error("recovery input capacity exhausted")]
    Capacity,
    #[error(transparent)]
    Storage(#[from] StoreError),
}
impl From<heed::Error> for InputWriteError {
    fn from(error: heed::Error) -> Self {
        match error {
            heed::Error::Mdb(heed::MdbError::MapFull) => Self::Capacity,
            other => Self::Storage(invalid(other)),
        }
    }
}
impl From<InputWriteError> for StoreError {
    fn from(error: InputWriteError) -> Self {
        match error {
            InputWriteError::Capacity => {
                StoreError::Backend("recovery input capacity exhausted".into())
            }
            InputWriteError::Storage(error) => error,
        }
    }
}
fn commit_write(txn: RwTxn<'_>) -> Result<(), InputWriteError> {
    // Pinned LMDB0.2.5 can exhaust pages while saving DB roots/freelist during
    // commit. Those failures precede metadata publication and abort the txn.
    // Classify only MapFull; I/O/other commit errors remain Storage failures.
    let result = txn.commit().map_err(InputWriteError::from);
    #[cfg(test)]
    if matches!(&result, Err(InputWriteError::Capacity)) {
        COMMIT_CAPACITY.with(|count| count.set(count.get() + 1));
    }
    result
}
#[cfg(test)]
thread_local! { static COMMIT_CAPACITY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

fn capacity_add(current: u64, amount: u64, limit: u64) -> Result<u64, InputWriteError> {
    if current > limit {
        return Err(invalid("recovery input counter exceeds its limit").into());
    }
    let next = current
        .checked_add(amount)
        .ok_or_else(|| invalid("recovery input counter overflow"))?;
    if next > limit {
        return Err(InputWriteError::Capacity);
    }
    Ok(next)
}

/// Download bookkeeping only, never authenticated replay progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub positions: u64,
    pub bytes: u64,
    pub complete: bool,
}

/// Work committed by one bounded raw-input cleanup transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetPage {
    pub rows: usize,
    pub bytes: usize,
    pub complete: bool,
}

pub struct RecoveryInputs {
    env: SharedEnv,
    manifest: Database<Bytes, Bytes>,
    chunks: Database<Bytes, Bytes>,
    context: [u8; 32],
    root_context: [u8; 32],
    generation: u64,
    stream_generation: u64,
    limits: Limits,
}
fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}
fn counter(bytes: Option<&[u8]>) -> Result<u64, StoreError> {
    Ok(u64::from_be_bytes(
        bytes
            .ok_or_else(|| invalid("missing recovery input counter"))?
            .try_into()
            .map_err(invalid)?,
    ))
}
fn stream_key(context: &[u8; 32]) -> [u8; 33] {
    let mut key = [0; 33];
    key[0] = b's';
    key[1..].copy_from_slice(context);
    key
}
fn stream_metadata_key(kind: u8, context: &[u8; 32]) -> [u8; 34] {
    let mut key = [0; 34];
    key[0] = kind;
    key[1] = b'/';
    key[2..].copy_from_slice(context);
    key
}
fn stream_generation(
    manifest: Database<Bytes, Bytes>,
    txn: &heed::RoTxn<'_>,
    context: &[u8; 32],
) -> Result<u64, StoreError> {
    manifest
        .get(txn, &stream_metadata_key(b'g', context))
        .map_err(invalid)?
        .map(|bytes| counter(Some(bytes)))
        .transpose()
        .map(|value| value.unwrap_or(0))
}
fn owned_chunk_key(context: &[u8; 32], digest: &[u8; 32]) -> [u8; 66] {
    let mut key = [0; 66];
    key[..2].copy_from_slice(b"o/");
    key[2..34].copy_from_slice(context);
    key[34..].copy_from_slice(digest);
    key
}
fn position_key(context: &[u8; 32], position: u64) -> [u8; 49] {
    let mut key = [0; 49];
    key[..9].copy_from_slice(b"position/");
    key[9..41].copy_from_slice(context);
    key[41..].copy_from_slice(&position.to_be_bytes());
    key
}
fn decode_progress(bytes: &[u8]) -> Result<Progress, StoreError> {
    if bytes.len() != 17 || bytes[16] > 1 {
        return Err(invalid("invalid recovery stream progress"));
    }
    Ok(Progress {
        positions: counter(Some(&bytes[..8]))?,
        bytes: counter(Some(&bytes[8..16]))?,
        complete: bytes[16] == 1,
    })
}
fn put_progress(
    manifest: Database<Bytes, Bytes>,
    txn: &mut RwTxn<'_>,
    context: &[u8; 32],
    state: Progress,
) -> Result<(), StoreError> {
    write_progress(manifest, txn, context, state).map_err(Into::into)
}
fn write_progress(
    manifest: Database<Bytes, Bytes>,
    txn: &mut RwTxn<'_>,
    context: &[u8; 32],
    state: Progress,
) -> Result<(), InputWriteError> {
    let mut bytes = [0; 17];
    bytes[..8].copy_from_slice(&state.positions.to_be_bytes());
    bytes[8..16].copy_from_slice(&state.bytes.to_be_bytes());
    bytes[16] = u8::from(state.complete);
    manifest
        .put(txn, &stream_key(context), &bytes)
        .map_err(Into::into)
}
fn progress(
    manifest: Database<Bytes, Bytes>,
    txn: &heed::RoTxn<'_>,
    context: &[u8; 32],
) -> Result<Progress, StoreError> {
    decode_progress(
        manifest
            .get(txn, &stream_key(context))
            .map_err(invalid)?
            .ok_or_else(|| invalid("missing recovery stream"))?,
    )
}

impl RecoveryInputs {
    /// `directory` must be dedicated to recovery inputs, never the live ledger.
    /// Context is an expected input identity supplied by the caller, not a trust
    /// token. Limits cannot change silently on reopen. No relaxed sync flags.
    pub fn open(directory: &Path, context: [u8; 32], limits: Limits) -> Result<Self, StoreError> {
        Self::open_inner(directory, context, limits, false)
    }

    /// Open a retained session using its saved root, even if it differs from
    /// `expected`. If none exists, create one with `expected` as its root.
    /// Format, limits and accounting are checked; payload digests are checked
    /// when read. The saved root identifies cached inputs, not trusted state.
    pub fn open_retained(
        directory: &Path,
        expected: [u8; 32],
        limits: Limits,
    ) -> Result<Self, StoreError> {
        Self::open_inner(directory, expected, limits, true)
    }

    pub fn root_context(&self) -> [u8; 32] {
        self.root_context
    }

    pub fn reset_target(&self) -> Result<Option<[u8; 32]>, StoreError> {
        let txn = self.env.read_txn().map_err(invalid)?;
        self.check_generation(&txn)?;
        self.manifest
            .get(&txn, b"reset")
            .map_err(invalid)?
            .map(|bytes| bytes.try_into().map_err(invalid))
            .transpose()
    }

    fn open_inner(
        directory: &Path,
        mut context: [u8; 32],
        limits: Limits,
        retained: bool,
    ) -> Result<Self, StoreError> {
        if limits.chunks == 0
            || limits.chunks > MAX_CHUNKS
            || limits.bytes == 0
            || limits.bytes > MAX_BYTES
            || limits.map_bytes < 1 << 20
            || limits.map_bytes as u64 > MAX_BYTES
            || limits.map_bytes % 4096 != 0
            || limits.bytes > limits.map_bytes as u64
        {
            return Err(invalid("invalid durable recovery input limits"));
        }
        std::fs::create_dir_all(directory).map_err(invalid)?;
        let mut options = EnvOpenOptions::new();
        options.max_dbs(2).map_size(limits.map_bytes);
        let env = SharedEnv::open(directory, options).map_err(invalid)?;
        let mut txn = env.write_txn().map_err(invalid)?;
        let manifest: Database<Bytes, Bytes> = env
            .create_database(&mut txn, Some("recovery_input_manifest"))
            .map_err(invalid)?;
        let chunks: Database<Bytes, Bytes> = env
            .create_database(&mut txn, Some("recovery_input_chunks"))
            .map_err(invalid)?;
        let mut header = HEADER.to_vec();
        header.extend_from_slice(&context);
        for value in [limits.chunks, limits.bytes, limits.map_bytes as u64] {
            header.extend_from_slice(&value.to_be_bytes());
        }
        match manifest.get(&txn, b"header").map_err(invalid)? {
            Some(stored) if stored != header => {
                let start = HEADER.len();
                if stored.len() != header.len()
                    || !(stored.starts_with(HEADER) || stored.starts_with(DIGEST_DOMAIN))
                    || (!retained && stored[start..start + 32] != context)
                    || stored[start + 32..] != header[start + 32..]
                {
                    return Err(invalid("recovery input context or limits changed"));
                }
                context.copy_from_slice(&stored[start..start + 32]);
                if stored.starts_with(DIGEST_DOMAIN) {
                    header[start..start + 32].copy_from_slice(&context);
                    manifest
                        .put(&mut txn, b"header", &header)
                        .map_err(invalid)?;
                }
            }
            Some(_) => {}
            None => {
                if chunks.len(&txn).map_err(invalid)? != 0
                    || manifest.len(&txn).map_err(invalid)? != 0
                {
                    return Err(invalid("recovery input manifest missing"));
                }
                manifest
                    .put(&mut txn, b"header", &header)
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"count", &0u64.to_be_bytes())
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"bytes", &0u64.to_be_bytes())
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"positions", &0u64.to_be_bytes())
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"stream_bytes", &0u64.to_be_bytes())
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"streams", &1u64.to_be_bytes())
                    .map_err(invalid)?;
                manifest
                    .put(&mut txn, b"generation", &1u64.to_be_bytes())
                    .map_err(invalid)?;
                put_progress(
                    manifest,
                    &mut txn,
                    &context,
                    Progress {
                        positions: 0,
                        bytes: 0,
                        complete: false,
                    },
                )?;
            }
        }
        let generation = counter(manifest.get(&txn, b"generation").map_err(invalid)?)?;
        if generation == 0 {
            return Err(invalid("invalid recovery generation"));
        }
        if let Some(target) = manifest.get(&txn, b"reset").map_err(invalid)? {
            if target.len() != 32 {
                return Err(invalid("invalid recovery reset target"));
            }
            // Partial cleanup intentionally removes accounting rows. It grants
            // cleanup access only; every ordinary operation checks the marker.
            let stream_generation = stream_generation(manifest, &txn, &context)?;
            txn.commit().map_err(invalid)?;
            env.sync_after_commit().map_err(invalid)?;
            return Ok(Self {
                env,
                manifest,
                chunks,
                context,
                root_context: context,
                generation,
                stream_generation,
                limits,
            });
        }
        // Reconcile persisted accounting before any further allocation. This
        // walks key/length metadata without copying or decoding input payloads.
        let mut count = 0u64;
        let mut bytes = 0u64;
        for row in chunks.iter(&txn).map_err(invalid)? {
            let (key, value) = row.map_err(invalid)?;
            if key.len() != 32 || value.is_empty() || value.len() > MAX_CHUNK_BYTES {
                return Err(invalid("invalid durable recovery chunk extent"));
            }
            count = count
                .checked_add(1)
                .filter(|n| *n <= limits.chunks)
                .ok_or_else(|| invalid("recovery chunk count exceeded"))?;
            bytes = bytes
                .checked_add(value.len() as u64)
                .filter(|n| *n <= limits.bytes)
                .ok_or_else(|| invalid("recovery byte budget exceeded"))?;
        }
        if count != counter(manifest.get(&txn, b"count").map_err(invalid)?)?
            || bytes != counter(manifest.get(&txn, b"bytes").map_err(invalid)?)?
        {
            return Err(invalid("recovery input accounting differs from chunks"));
        }
        let mut streams = 0u64;
        let mut total_positions = 0u64;
        let mut total_bytes = 0u64;
        for entry in manifest.prefix_iter(&txn, b"s").map_err(invalid)? {
            let (key, value) = entry.map_err(invalid)?;
            // Global counters share this prefix, but have fixed names/lengths.
            if key == b"streams" || key == b"stream_bytes" {
                continue;
            }
            if key.len() != 33 {
                return Err(invalid("invalid recovery stream key"));
            }
            let stream: [u8; 32] = key[1..].try_into().map_err(invalid)?;
            let state = decode_progress(value)?;
            if state.positions > limits.chunks
                || state.bytes > limits.bytes
                || (state.complete && state.positions == 0)
            {
                return Err(invalid("recovery stream progress exceeds limits"));
            }
            streams = streams
                .checked_add(1)
                .filter(|n| *n <= limits.chunks)
                .ok_or_else(|| invalid("recovery stream count exceeded"))?;
            let first = manifest
                .get(&txn, &stream_metadata_key(b'r', &stream))
                .map_err(invalid)?
                .map(|bytes| counter(Some(bytes)))
                .transpose()?
                .unwrap_or(0);
            if first > state.positions {
                return Err(invalid("recovery stream reset exceeds positions"));
            }
            let owned_required = stream_generation(manifest, &txn, &stream)? != 0;
            let mut positions = first;
            let mut stream_bytes = 0u64;
            let prefix = position_key(&stream, 0);
            for row in manifest.prefix_iter(&txn, &prefix[..41]).map_err(invalid)? {
                let (key, value) = row.map_err(invalid)?;
                if positions >= state.positions
                    || key != position_key(&stream, positions)
                    || value.len() != 32
                {
                    return Err(invalid(
                        "recovery input positions are incomplete or unordered",
                    ));
                }
                let digest = value.try_into().map_err(invalid)?;
                if owned_required
                    && manifest
                        .get(&txn, &owned_chunk_key(&stream, &digest))
                        .map_err(invalid)?
                        != Some(&[0][..])
                {
                    return Err(invalid("positioned recovery input has no owner"));
                }
                let size = chunks
                    .get(&txn, value)
                    .map_err(invalid)?
                    .ok_or_else(|| invalid("missing positioned input"))?
                    .len();
                stream_bytes = stream_bytes
                    .checked_add(size as u64)
                    .filter(|n| *n <= limits.bytes)
                    .ok_or_else(|| invalid("recovery input stream byte budget exceeded"))?;
                positions += 1;
            }
            if positions != state.positions || stream_bytes != state.bytes {
                return Err(invalid("recovery stream accounting differs"));
            }
            total_positions = total_positions
                .checked_add(positions - first)
                .filter(|n| *n <= limits.chunks)
                .ok_or_else(|| invalid("aggregate recovery positions exceeded"))?;
            total_bytes = total_bytes
                .checked_add(stream_bytes)
                .filter(|n| *n <= limits.bytes)
                .ok_or_else(|| invalid("aggregate recovery stream bytes exceeded"))?;
        }
        let mut extra = 0u64;
        for kind in [b'g', b'r'] {
            for row in manifest.prefix_iter(&txn, &[kind, b'/']).map_err(invalid)? {
                let (key, value) = row.map_err(invalid)?;
                if key.len() != 34 {
                    return Err(invalid("invalid recovery stream metadata key"));
                }
                let context = key[2..].try_into().map_err(invalid)?;
                let state = progress(manifest, &txn, &context)?;
                let number = counter(Some(value))?;
                if (kind == b'g' && number == 0) || (kind == b'r' && number > state.positions) {
                    return Err(invalid("invalid recovery stream metadata"));
                }
                extra = extra
                    .checked_add(1)
                    .ok_or_else(|| invalid("recovery metadata count overflow"))?;
            }
        }
        for row in manifest.prefix_iter(&txn, b"o/").map_err(invalid)? {
            let (key, value) = row.map_err(invalid)?;
            if key.len() != 66 || value != [0] {
                return Err(invalid("invalid recovery chunk owner"));
            }
            let context = key[2..34].try_into().map_err(invalid)?;
            let state = progress(manifest, &txn, &context)?;
            if chunks.get(&txn, &key[34..]).map_err(invalid)?.is_none() {
                let next = manifest
                    .get(&txn, &stream_metadata_key(b'r', &context))
                    .map_err(invalid)?
                    .map(|bytes| counter(Some(bytes)))
                    .transpose()?;
                if next != Some(state.positions) || state.bytes != 0 {
                    return Err(invalid("owned recovery chunk missing outside cleanup"));
                }
            }
            extra = extra
                .checked_add(1)
                .ok_or_else(|| invalid("recovery metadata count overflow"))?;
        }
        if streams != counter(manifest.get(&txn, b"streams").map_err(invalid)?)?
            || total_positions != counter(manifest.get(&txn, b"positions").map_err(invalid)?)?
            || total_bytes != counter(manifest.get(&txn, b"stream_bytes").map_err(invalid)?)?
            || manifest.len(&txn).map_err(invalid)? != 7 + streams + total_positions + extra
        {
            return Err(invalid("aggregate recovery manifest differs"));
        }
        progress(manifest, &txn, &context)?;
        let stream_generation = stream_generation(manifest, &txn, &context)?;
        txn.commit().map_err(invalid)?;
        env.sync_after_commit().map_err(invalid)?;
        Ok(Self {
            env,
            manifest,
            chunks,
            context,
            root_context: context,
            generation,
            stream_generation,
            limits,
        })
    }

    /// Select/create an independent stream by expected identity. Uses indexed
    /// lookups in the already validated environment, never a whole-cache scan.
    /// Stream metadata, unique chunks and all logical positions share the caps.
    pub fn select_stream(&self, context: [u8; 32]) -> Result<Self, StoreError> {
        self.select_stream_checked(context).map_err(Into::into)
    }

    pub fn select_stream_checked(&self, context: [u8; 32]) -> Result<Self, InputWriteError> {
        let selected = |stream_generation| Self {
            env: self.env.clone(),
            manifest: self.manifest,
            chunks: self.chunks,
            context,
            root_context: self.root_context,
            generation: self.generation,
            stream_generation,
            limits: self.limits,
        };
        {
            let txn = self.env.read_txn().map_err(invalid)?;
            self.check_active(&txn)?;
            if let Some(bytes) = self
                .manifest
                .get(&txn, &stream_key(&context))
                .map_err(invalid)?
            {
                decode_progress(bytes)?;
                return Ok(selected(stream_generation(self.manifest, &txn, &context)?));
            }
        }
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_active(&txn)?;
        match self
            .manifest
            .get(&txn, &stream_key(&context))
            .map_err(invalid)?
        {
            Some(bytes) => {
                decode_progress(bytes)?;
            }
            None => {
                let count = capacity_add(
                    counter(self.manifest.get(&txn, b"streams").map_err(invalid)?)?,
                    1,
                    self.limits.chunks,
                )?;
                write_progress(
                    self.manifest,
                    &mut txn,
                    &context,
                    Progress {
                        positions: 0,
                        bytes: 0,
                        complete: false,
                    },
                )?;
                self.manifest
                    .put(&mut txn, b"streams", &count.to_be_bytes())
                    .map_err(InputWriteError::from)?;
            }
        }
        let generation = stream_generation(self.manifest, &txn, &context)?;
        commit_write(txn)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(selected(generation))
    }

    fn check_generation(&self, txn: &heed::RoTxn<'_>) -> Result<(), StoreError> {
        if counter(self.manifest.get(txn, b"generation").map_err(invalid)?)? != self.generation {
            return Err(invalid("stale recovery input generation"));
        }
        Ok(())
    }

    fn check_active(&self, txn: &heed::RoTxn<'_>) -> Result<(), StoreError> {
        self.check_generation(txn)?;
        if stream_generation(self.manifest, txn, &self.context)? != self.stream_generation {
            return Err(invalid("stale recovery stream generation"));
        }
        if self
            .manifest
            .get(txn, &stream_metadata_key(b'r', &self.context))
            .map_err(invalid)?
            .is_some()
        {
            return Err(invalid("recovery input stream is being reset"));
        }
        if self.manifest.get(txn, b"reset").map_err(invalid)?.is_some() {
            return Err(invalid("recovery input session is being reset"));
        }
        Ok(())
    }

    /// Explicitly discard the entire raw-input session, not a live ledger.
    /// Existing streams become unavailable atomically. The target is immutable
    /// until cleanup finishes; reopening with the old root can resume cleanup.
    pub fn begin_reset(&self, next_context: [u8; 32]) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_generation(&txn)?;
        if let Some(target) = self.manifest.get(&txn, b"reset").map_err(invalid)? {
            if target != next_context {
                return Err(invalid("recovery reset target changed"));
            }
            return Ok(());
        }
        // Partial whole-session cleanup may already have removed stream
        // generations. Repeating its immutable target above is still a no-op.
        if stream_generation(self.manifest, &txn, &self.context)? != self.stream_generation {
            return Err(invalid("stale recovery stream generation"));
        }
        self.generation
            .checked_add(1)
            .ok_or_else(|| invalid("recovery generation exhausted"))?;
        self.manifest
            .put(&mut txn, b"reset", &next_context)
            .map_err(invalid)?;
        txn.commit().map_err(invalid)?;
        self.env.sync_after_commit().map_err(invalid)
    }

    /// Delete at most 128 rows and 1 MiB + 64 key/value bytes per call. These
    /// limits exclude bookkeeping writes. No raw payload is copied.
    /// Finalization replaces the header and increments
    /// its generation, invalidating even old handles for the same root context.
    /// Logical deletion reuses map pages; it does not promise file truncation.
    pub fn reset_page(&self, max_rows: usize, max_bytes: usize) -> Result<ResetPage, StoreError> {
        if max_rows == 0 || max_rows > 128 || max_bytes == 0 || max_bytes > MAX_CHUNK_BYTES + 64 {
            return Err(invalid("invalid recovery reset page budget"));
        }
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_generation(&txn)?;
        let target: [u8; 32] = self
            .manifest
            .get(&txn, b"reset")
            .map_err(invalid)?
            .ok_or_else(|| invalid("recovery reset not started"))?
            .try_into()
            .map_err(invalid)?;
        let mut page = ResetPage {
            rows: 0,
            bytes: 0,
            complete: false,
        };
        for (is_manifest, database) in [(false, self.chunks), (true, self.manifest)] {
            let mut keys = Vec::new();
            for row in database.iter(&txn).map_err(invalid)? {
                let (key, value) = row.map_err(invalid)?;
                if is_manifest && [b"header".as_slice(), b"generation", b"reset"].contains(&key) {
                    continue;
                }
                let size = key
                    .len()
                    .checked_add(value.len())
                    .ok_or_else(|| invalid("recovery reset extent overflow"))?;
                if page.rows == max_rows || size > max_bytes - page.bytes {
                    break;
                }
                keys.push(key.to_vec());
                page.rows += 1;
                page.bytes += size;
            }
            for key in keys {
                database.delete(&mut txn, &key).map_err(invalid)?;
            }
            // Delete chunks before metadata; partial state is never readable.
            if self.chunks.len(&txn).map_err(invalid)? != 0 {
                break;
            }
        }
        if self.chunks.len(&txn).map_err(invalid)? == 0
            && self.manifest.len(&txn).map_err(invalid)? == 3
        {
            let generation = self
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid("recovery generation exhausted"))?;
            let mut header = HEADER.to_vec();
            header.extend_from_slice(&target);
            for value in [
                self.limits.chunks,
                self.limits.bytes,
                self.limits.map_bytes as u64,
            ] {
                header.extend_from_slice(&value.to_be_bytes());
            }
            self.manifest
                .put(&mut txn, b"header", &header)
                .map_err(invalid)?;
            self.manifest
                .put(&mut txn, b"generation", &generation.to_be_bytes())
                .map_err(invalid)?;
            for key in [b"count".as_slice(), b"bytes", b"positions", b"stream_bytes"] {
                self.manifest
                    .put(&mut txn, key, &0u64.to_be_bytes())
                    .map_err(invalid)?;
            }
            self.manifest
                .put(&mut txn, b"streams", &1u64.to_be_bytes())
                .map_err(invalid)?;
            put_progress(
                self.manifest,
                &mut txn,
                &target,
                Progress {
                    positions: 0,
                    bytes: 0,
                    complete: false,
                },
            )?;
            self.manifest.delete(&mut txn, b"reset").map_err(invalid)?;
            page.complete = true;
        } else if page.rows == 0 {
            return Err(invalid("recovery reset page cannot fit next row"));
        }
        txn.commit().map_err(invalid)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(page)
    }

    fn digest(&self, bytes: &[u8]) -> [u8; 32] {
        #[cfg(test)]
        DIGEST_BYTES.with(|count| count.set(count.get() + bytes.len() as u64));
        burst_crypto::blake2b_256_multi(&[DIGEST_DOMAIN, &self.root_context, &self.context, bytes])
    }

    fn stage(&self, txn: &mut RwTxn<'_>, bytes: &[u8]) -> Result<[u8; 32], InputWriteError> {
        self.check_active(txn)?;
        if progress(self.manifest, txn, &self.context)?.complete {
            return Err(invalid("recovery input stream is sealed").into());
        }
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err(invalid("invalid recovery input chunk size").into());
        }
        let key = self.digest(bytes);
        if let Some(old) = self.chunks.get(txn, &key).map_err(invalid)? {
            if old != bytes {
                return Err(invalid("existing recovery input differs from digest").into());
            }
            self.manifest
                .put(txn, &owned_chunk_key(&self.context, &key), &[0])
                .map_err(InputWriteError::from)?;
            return Ok(key);
        }
        let count = capacity_add(
            counter(self.manifest.get(txn, b"count").map_err(invalid)?)?,
            1,
            self.limits.chunks,
        )?;
        let size = capacity_add(
            counter(self.manifest.get(txn, b"bytes").map_err(invalid)?)?,
            bytes.len() as u64,
            self.limits.bytes,
        )?;
        self.chunks
            .put(txn, &key, bytes)
            .map_err(InputWriteError::from)?;
        self.manifest
            .put(txn, &owned_chunk_key(&self.context, &key), &[0])
            .map_err(InputWriteError::from)?;
        self.manifest
            .put(txn, b"count", &count.to_be_bytes())
            .map_err(InputWriteError::from)?;
        self.manifest
            .put(txn, b"bytes", &size.to_be_bytes())
            .map_err(InputWriteError::from)?;
        Ok(key)
    }

    pub fn insert(&self, bytes: &[u8]) -> Result<[u8; 32], StoreError> {
        let mut txn = self.env.write_txn().map_err(invalid)?;
        let key = self.stage(&mut txn, bytes)?;
        txn.commit().map_err(invalid)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(key)
    }

    fn stage_append(
        &self,
        txn: &mut RwTxn<'_>,
        expected: u64,
        bytes: &[u8],
    ) -> Result<[u8; 32], InputWriteError> {
        self.check_active(txn)?;
        let state = progress(self.manifest, txn, &self.context)?;
        if state.complete || expected != state.positions {
            return Err(invalid("recovery input append is stale or sealed").into());
        }
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err(invalid("invalid recovery input chunk size").into());
        }
        let total_positions = counter(self.manifest.get(txn, b"positions").map_err(invalid)?)?;
        let total_bytes = counter(self.manifest.get(txn, b"stream_bytes").map_err(invalid)?)?;
        for (current, limit) in [
            (state.positions, self.limits.chunks),
            (state.bytes, self.limits.bytes),
            (total_positions, self.limits.chunks),
            (total_bytes, self.limits.bytes),
        ] {
            if current > limit {
                return Err(invalid("recovery input counter exceeds its limit").into());
            }
        }
        let position = position_key(&self.context, expected);
        if self
            .manifest
            .get(txn, &position)
            .map_err(invalid)?
            .is_some()
        {
            return Err(invalid("recovery input position already exists").into());
        }
        capacity_add(state.positions, 1, self.limits.chunks)?;
        let size = capacity_add(state.bytes, bytes.len() as u64, self.limits.bytes)?;
        let total_positions = capacity_add(total_positions, 1, self.limits.chunks)?;
        let total_bytes = capacity_add(total_bytes, bytes.len() as u64, self.limits.bytes)?;
        let key = self.stage(txn, bytes)?;
        self.manifest
            .put(txn, &position, &key)
            .map_err(InputWriteError::from)?;
        self.manifest
            .put(txn, b"positions", &total_positions.to_be_bytes())
            .map_err(InputWriteError::from)?;
        self.manifest
            .put(txn, b"stream_bytes", &total_bytes.to_be_bytes())
            .map_err(InputWriteError::from)?;
        write_progress(
            self.manifest,
            txn,
            &self.context,
            Progress {
                positions: expected + 1,
                bytes: size,
                complete: false,
            },
        )?;
        Ok(key)
    }

    /// Append bytes and advance the caller's expected position atomically.
    /// Repeated bytes within a stream share chunk storage but still consume a
    /// position slot and their full length in the stream byte budget.
    pub fn append(&self, expected: u64, bytes: &[u8]) -> Result<[u8; 32], StoreError> {
        self.append_checked(expected, bytes).map_err(Into::into)
    }

    pub fn append_checked(&self, expected: u64, bytes: &[u8]) -> Result<[u8; 32], InputWriteError> {
        let mut txn = self.env.write_txn().map_err(invalid)?;
        let key = self.stage_append(&mut txn, expected, bytes)?;
        commit_write(txn)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(key)
    }

    /// One bounded input batch: a failed later chunk aborts all earlier chunks
    /// and progress changes in this transaction.
    pub fn append_batch(&self, expected: u64, chunks: &[Vec<u8>]) -> Result<(), StoreError> {
        self.append_batch_checked(expected, chunks)
            .map_err(Into::into)
    }

    pub fn append_batch_checked(
        &self,
        expected: u64,
        chunks: &[Vec<u8>],
    ) -> Result<(), InputWriteError> {
        if chunks.is_empty() || chunks.len() > 128 {
            return Err(invalid("invalid recovery input batch count").into());
        }
        let mut bytes = 0usize;
        for chunk in chunks {
            if chunk.is_empty() || chunk.len() > MAX_CHUNK_BYTES {
                return Err(invalid("invalid recovery input chunk size").into());
            }
            bytes = bytes
                .checked_add(chunk.len())
                .filter(|n| *n <= MAX_CHUNK_BYTES)
                .ok_or_else(|| invalid("recovery input batch byte budget exceeded"))?;
        }
        let mut txn = self.env.write_txn().map_err(invalid)?;
        for (index, chunk) in chunks.iter().enumerate() {
            let position = expected
                .checked_add(index as u64)
                .ok_or_else(|| invalid("recovery input position overflow"))?;
            self.stage_append(&mut txn, position, chunk)?;
        }
        commit_write(txn)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(())
    }

    pub fn progress(&self) -> Result<Progress, StoreError> {
        let txn = self.env.read_txn().map_err(invalid)?;
        self.check_active(&txn)?;
        progress(self.manifest, &txn, &self.context)
    }

    /// Mark download completion, not verification or installation. A complete
    /// stream rejects inserts and appends until explicitly reset. An empty
    /// stream cannot be sealed.
    pub fn seal(&self, expected: u64) -> Result<(), StoreError> {
        self.seal_checked(expected).map_err(Into::into)
    }

    pub fn seal_checked(&self, expected: u64) -> Result<(), InputWriteError> {
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_active(&txn)?;
        let state = progress(self.manifest, &txn, &self.context)?;
        if expected == 0 || state.positions != expected || expected > self.limits.chunks {
            return Err(invalid("recovery input completion count differs").into());
        }
        if state.complete {
            return Ok(());
        }
        write_progress(
            self.manifest,
            &mut txn,
            &self.context,
            Progress {
                complete: true,
                ..state
            },
        )?;
        commit_write(txn)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(())
    }

    pub fn read_position(&self, position: u64) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(invalid)?;
        self.check_active(&txn)?;
        let state = progress(self.manifest, &txn, &self.context)?;
        if state.positions > self.limits.chunks || state.bytes > self.limits.bytes {
            return Err(invalid("recovery input progress exceeds budget"));
        }
        if position >= state.positions {
            return Ok(None);
        }
        let key: [u8; 32] = self
            .manifest
            .get(&txn, &position_key(&self.context, position))
            .map_err(invalid)?
            .ok_or_else(|| invalid("missing recovery input position"))?
            .try_into()
            .map_err(invalid)?;
        let bytes = self
            .chunks
            .get(&txn, &key)
            .map_err(invalid)?
            .ok_or_else(|| invalid("missing positioned recovery input"))?;
        self.checked_bytes(&key, bytes).map(Some)
    }

    /// Read raw bytes under their expected digest. This never returns an
    /// authenticated block/cursor or grants installation/deletion authority.
    pub fn get(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(invalid)?;
        self.check_active(&txn)?;
        let Some(bytes) = self.chunks.get(&txn, key).map_err(invalid)? else {
            return Ok(None);
        };
        let bytes = self.checked_bytes(key, bytes)?;
        if self.stream_generation != 0
            && self
                .manifest
                .get(&txn, &owned_chunk_key(&self.context, key))
                .map_err(invalid)?
                != Some(&[0][..])
        {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    fn checked_bytes(&self, key: &[u8; 32], bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        if bytes.is_empty()
            || bytes.len() > MAX_CHUNK_BYTES
            || bytes.len() as u64 > self.limits.bytes
            || self.digest(bytes) != *key
        {
            return Err(invalid("durable recovery input digest or extent differs"));
        }
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> Limits {
        Limits {
            chunks: 2,
            bytes: 8,
            map_bytes: 1 << 20,
        }
    }
    #[test]
    fn paged_session_reset_resumes_and_invalidates_every_old_stream() {
        for target in [[1; 32], [9; 32]] {
            let dir = tempfile::tempdir().unwrap();
            let root = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
            let second = root.select_stream([2; 32]).unwrap();
            let key = root.append(0, b"one").unwrap();
            second.append(0, b"two").unwrap();
            root.seal(1).unwrap();
            root.begin_reset(target).unwrap();
            root.begin_reset(target).unwrap();
            assert!(root.begin_reset([7; 32]).is_err());
            let mut complete = false;
            for _ in 0..32 {
                let resumed = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
                assert!(resumed.progress().is_err());
                assert!(second.read_position(0).is_err());
                assert!(second.append(1, b"x").is_err());
                assert!(resumed.select_stream([3; 32]).is_err());
                let page = resumed.reset_page(1, 256).unwrap();
                assert!(page.rows <= 1 && page.bytes <= 256);
                if page.complete {
                    complete = true;
                    break;
                }
            }
            assert!(complete);
            let fresh = RecoveryInputs::open(dir.path(), target, limits()).unwrap();
            assert_eq!(
                fresh.progress().unwrap(),
                Progress {
                    positions: 0,
                    bytes: 0,
                    complete: false
                }
            );
            assert!(root.progress().is_err());
            assert!(root.get(&key).is_err());
            assert!(root.insert(b"x").is_err());
            assert!(root.append(0, b"x").is_err());
            assert!(root.seal(1).is_err());
            assert!(root.select_stream([2; 32]).is_err());
            assert!(root.begin_reset(target).is_err());
            assert!(root.reset_page(1, 256).is_err());
            assert!(second.insert(b"x").is_err());
            fresh.append(0, b"new").unwrap();
            fresh
                .select_stream([2; 32])
                .unwrap()
                .append(0, b"other")
                .unwrap();
            drop(fresh);
            drop(second);
            drop(root);
            let fresh = RecoveryInputs::open(dir.path(), target, limits()).unwrap();
            assert_eq!(fresh.read_position(0).unwrap(), Some(b"new".to_vec()));
            if target != [1; 32] {
                assert!(RecoveryInputs::open(dir.path(), [1; 32], limits()).is_err());
            }
        }
    }

    #[test]
    fn reset_rejects_unbounded_or_insufficient_pages_without_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        inputs.insert(b"12345678").unwrap();
        assert!(inputs.reset_page(1, 256).is_err());
        inputs.begin_reset([2; 32]).unwrap();
        for (rows, bytes) in [
            (0, 256),
            (129, 256),
            (1, 0),
            (1, MAX_CHUNK_BYTES + 65),
            (1, 39),
        ] {
            assert!(inputs.reset_page(rows, bytes).is_err());
            assert_eq!(
                inputs.chunks.len(&inputs.env.read_txn().unwrap()).unwrap(),
                1
            );
        }
        let page = inputs.reset_page(1, 40).unwrap();
        assert_eq!(
            page,
            ResetPage {
                rows: 1,
                bytes: 40,
                complete: false
            }
        );
        assert!(inputs.progress().is_err());
    }

    #[test]
    fn reset_generation_exhaustion_and_malformed_target_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        let mut txn = inputs.env.write_txn().unwrap();
        inputs
            .manifest
            .put(&mut txn, b"generation", &u64::MAX.to_be_bytes())
            .unwrap();
        txn.commit().unwrap();
        inputs.generation = u64::MAX;
        assert!(inputs.begin_reset([1; 32]).is_err());
        let mut txn = inputs.env.write_txn().unwrap();
        inputs.manifest.put(&mut txn, b"reset", &[0]).unwrap();
        txn.commit().unwrap();
        assert!(inputs.progress().is_err());
        assert!(inputs.reset_page(1, 256).is_err());
        drop(inputs);
        assert!(RecoveryInputs::open(dir.path(), [1; 32], limits()).is_err());
    }

    #[test]
    fn streams_keep_independent_progress_with_shared_limits_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            chunks: 4,
            ..limits()
        };
        let root = RecoveryInputs::open(dir.path(), [1; 32], limits).unwrap();
        let second = root.select_stream([2; 32]).unwrap();
        let first_key = root.append(0, b"one").unwrap();
        let second_key = second.append(0, b"two").unwrap();
        assert!(second.get(&first_key).is_err());
        assert!(root.get(&second_key).is_err());
        root.seal(1).unwrap();
        assert!(root.append(1, b"x").is_err());
        second
            .append_batch(1, &[b"x".to_vec(), b"y".to_vec()])
            .unwrap();
        assert!(second.append(3, b"z").is_err());
        assert!(second.insert(b"z").is_err());
        root.select_stream([3; 32]).unwrap();
        root.select_stream([4; 32]).unwrap();
        assert!(root.select_stream([5; 32]).is_err());
        assert_eq!(
            root.select_stream([2; 32])
                .unwrap()
                .progress()
                .unwrap()
                .positions,
            3
        );
        drop(second);
        drop(root);
        assert!(RecoveryInputs::open(dir.path(), [2; 32], limits).is_err());
        let root = RecoveryInputs::open(dir.path(), [1; 32], limits).unwrap();
        let second = root.select_stream([2; 32]).unwrap();
        assert_eq!(
            root.progress().unwrap(),
            Progress {
                positions: 1,
                bytes: 3,
                complete: true
            }
        );
        assert_eq!(
            second.progress().unwrap(),
            Progress {
                positions: 3,
                bytes: 5,
                complete: false
            }
        );
        assert_eq!(root.read_position(0).unwrap(), Some(b"one".to_vec()));
        assert_eq!(second.read_position(0).unwrap(), Some(b"two".to_vec()));
        second.seal(3).unwrap();
    }

    #[test]
    fn stream_batch_failure_rolls_back_global_and_local_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(
            dir.path(),
            [1; 32],
            Limits {
                chunks: 4,
                bytes: 5,
                ..limits()
            },
        )
        .unwrap();
        let second = root.select_stream([2; 32]).unwrap();
        root.append(0, b"one").unwrap();
        assert!(second
            .append_batch(0, &[b"x".to_vec(), b"xx".to_vec()])
            .is_err());
        assert_eq!(second.progress().unwrap().positions, 0);
        assert_eq!(second.get(&second.digest(b"x")).unwrap(), None);
        second.append(0, b"xx").unwrap();
        assert!(root.append(1, b"z").is_err());
        drop(second);
        drop(root);
        let root = RecoveryInputs::open(
            dir.path(),
            [1; 32],
            Limits {
                chunks: 4,
                bytes: 5,
                ..limits()
            },
        )
        .unwrap();
        assert_eq!(
            root.select_stream([2; 32])
                .unwrap()
                .read_position(0)
                .unwrap(),
            Some(b"xx".to_vec())
        );
    }

    #[test]
    fn competing_streams_cannot_overcommit_shared_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(
            dir.path(),
            [1; 32],
            Limits {
                bytes: 1,
                ..limits()
            },
        )
        .unwrap();
        let second = root.select_stream([2; 32]).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                root.append(0, b"a")
            });
            let other = scope.spawn(|| {
                barrier.wait();
                second.append(0, b"b")
            });
            let results = [first.join().unwrap(), other.join().unwrap()];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        });
        assert_eq!(
            root.progress().unwrap().positions + second.progress().unwrap().positions,
            1
        );
        drop(second);
        drop(root);
        RecoveryInputs::open(
            dir.path(),
            [1; 32],
            Limits {
                bytes: 1,
                ..limits()
            },
        )
        .unwrap();
    }

    #[test]
    fn stream_manifest_corruption_is_rejected_on_open() {
        for mode in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let root = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
            let second = root.select_stream([2; 32]).unwrap();
            second.append(0, b"one").unwrap();
            let mut txn = root.env.write_txn().unwrap();
            match mode {
                0 => {
                    root.manifest
                        .put(&mut txn, b"streams", &1u64.to_be_bytes())
                        .unwrap();
                }
                1 => {
                    root.manifest
                        .put(&mut txn, b"positions", &0u64.to_be_bytes())
                        .unwrap();
                }
                2 => {
                    root.manifest
                        .delete(&mut txn, &stream_key(&[2; 32]))
                        .unwrap();
                }
                _ => {
                    put_progress(
                        root.manifest,
                        &mut txn,
                        &[2; 32],
                        Progress {
                            positions: 0,
                            bytes: 0,
                            complete: true,
                        },
                    )
                    .unwrap();
                }
            }
            txn.commit().unwrap();
            drop(second);
            drop(root);
            assert!(RecoveryInputs::open(dir.path(), [1; 32], limits()).is_err());
        }
    }

    #[test]
    fn ordered_progress_and_payload_commit_together_and_seal_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert!(inputs.seal(0).is_err());
        inputs.append(0, b"one").unwrap();
        let abandoned = {
            let mut txn = inputs.env.write_txn().unwrap();
            inputs.stage_append(&mut txn, 1, b"lost").unwrap()
        };
        assert_eq!(
            inputs.progress().unwrap(),
            Progress {
                positions: 1,
                bytes: 3,
                complete: false
            }
        );
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(inputs.read_position(0).unwrap(), Some(b"one".to_vec()));
        assert_eq!(inputs.read_position(1).unwrap(), None);
        assert_eq!(inputs.get(&abandoned).unwrap(), None);
        assert!(inputs.append(0, b"stale").is_err());
        assert!(inputs.append(2, b"gap").is_err());
        assert!(inputs.seal(2).is_err());
        inputs.append(1, b"one").unwrap();
        assert!(
            inputs.append(2, b"one").is_err(),
            "deduplication cannot evade position limit"
        );
        inputs.seal(2).unwrap();
        inputs.seal(2).unwrap();
        assert!(inputs.insert(b"new").is_err());
        assert!(inputs.append(2, b"new").is_err());
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(
            inputs.progress().unwrap(),
            Progress {
                positions: 2,
                bytes: 6,
                complete: true
            }
        );
        for index in 0..2 {
            assert_eq!(inputs.read_position(index).unwrap(), Some(b"one".to_vec()));
        }
        let limited_dir = tempfile::tempdir().unwrap();
        let limited = RecoveryInputs::open(
            limited_dir.path(),
            [1; 32],
            Limits {
                chunks: 3,
                bytes: 4,
                ..limits()
            },
        )
        .unwrap();
        limited.append(0, b"one").unwrap();
        assert!(
            limited.append(1, b"one").is_err(),
            "deduplication cannot evade stream byte limit"
        );
        assert_eq!(
            limited.progress().unwrap(),
            Progress {
                positions: 1,
                bytes: 3,
                complete: false
            }
        );
    }

    #[test]
    fn corrupt_progress_missing_references_and_old_format_are_rejected() {
        for mode in 0..7 {
            let dir = tempfile::tempdir().unwrap();
            let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
            inputs.append(0, b"one").unwrap();
            let mut txn = inputs.env.write_txn().unwrap();
            match mode {
                0 => {
                    inputs
                        .manifest
                        .delete(&mut txn, &position_key(&inputs.context, 0))
                        .unwrap();
                }
                1 => {
                    inputs
                        .manifest
                        .put(&mut txn, &position_key(&inputs.context, 0), &[9; 32])
                        .unwrap();
                }
                2 => {
                    inputs
                        .manifest
                        .put(&mut txn, &stream_key(&inputs.context), &[2])
                        .unwrap();
                }
                3 => {
                    inputs.manifest.put(&mut txn, b"unexpected", &[0]).unwrap();
                }
                4 => {
                    inputs
                        .manifest
                        .put(&mut txn, b"header", b"BURST/recovery-inputs/v1")
                        .unwrap();
                }
                5 => {
                    inputs
                        .manifest
                        .put(&mut txn, b"header", b"BURST/recovery-inputs/v2")
                        .unwrap();
                }
                _ => {
                    inputs
                        .manifest
                        .put(&mut txn, b"header", b"BURST/recovery-inputs/v3")
                        .unwrap();
                }
            }
            txn.commit().unwrap();
            if mode <= 2 {
                assert!(inputs.read_position(0).is_err());
            }
            drop(inputs);
            assert!(RecoveryInputs::open(dir.path(), [1; 32], limits()).is_err());
        }
    }

    #[test]
    fn concurrent_appenders_cannot_overwrite_the_same_position() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                inputs.append(0, b"one")
            });
            let second = scope.spawn(|| {
                barrier.wait();
                inputs.append(0, b"two")
            });
            let results = [first.join().unwrap(), second.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        });
        assert_eq!(inputs.progress().unwrap().positions, 1);
        let first = inputs.read_position(0).unwrap().unwrap();
        assert!(first == b"one" || first == b"two");
        inputs.append(1, b"next").unwrap();
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(inputs.read_position(0).unwrap(), Some(first));
        assert_eq!(inputs.read_position(1).unwrap(), Some(b"next".to_vec()));
    }

    #[test]
    fn batch_failure_does_not_publish_an_earlier_chunk_or_progress() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert!(inputs
            .append_batch(0, &[b"one".to_vec(), b"too-large".to_vec()])
            .is_err());
        assert_eq!(
            inputs.progress().unwrap(),
            Progress {
                positions: 0,
                bytes: 0,
                complete: false
            }
        );
        assert_eq!(inputs.get(&inputs.digest(b"one")).unwrap(), None);
        assert!(inputs.append_batch(0, &[]).is_err());
        inputs
            .append_batch(0, &[b"one".to_vec(), b"two".to_vec()])
            .unwrap();
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(
            inputs.progress().unwrap(),
            Progress {
                positions: 2,
                bytes: 6,
                complete: false
            }
        );
        assert_eq!(inputs.read_position(1).unwrap(), Some(b"two".to_vec()));
    }

    #[test]
    fn committed_inputs_reopen_but_abandoned_transactions_publish_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        let kept = inputs.insert(b"kept").unwrap();
        assert_eq!(inputs.insert(b"kept").unwrap(), kept);
        let abandoned = {
            let mut txn = inputs.env.write_txn().unwrap();
            inputs.stage(&mut txn, b"lost").unwrap()
        };
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(inputs.get(&kept).unwrap(), Some(b"kept".to_vec()));
        assert_eq!(inputs.get(&abandoned).unwrap(), None);
        assert!(inputs.insert(b"four").is_ok());
        assert!(inputs.insert(b"x").is_err());
        assert_eq!(inputs.get(&kept).unwrap(), Some(b"kept".to_vec()));
    }
    #[test]
    fn foreign_context_limits_corrupt_bytes_and_counters_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        let key = inputs.insert(b"good").unwrap();
        drop(inputs);
        assert!(RecoveryInputs::open(dir.path(), [2; 32], limits()).is_err());
        assert!(RecoveryInputs::open(
            dir.path(),
            [1; 32],
            Limits {
                bytes: 7,
                ..limits()
            }
        )
        .is_err());
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        let mut txn = inputs.env.write_txn().unwrap();
        inputs.chunks.put(&mut txn, &key, b"evil").unwrap();
        txn.commit().unwrap();
        assert!(inputs.get(&key).is_err());
        assert!(inputs.insert(b"good").is_err());
        let mut txn = inputs.env.write_txn().unwrap();
        inputs
            .manifest
            .put(&mut txn, b"count", &0u64.to_be_bytes())
            .unwrap();
        txn.commit().unwrap();
        drop(inputs);
        assert!(RecoveryInputs::open(dir.path(), [1; 32], limits()).is_err());
    }
    #[test]
    fn map_exhaustion_aborts_payload_and_accounting_together() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            chunks: 2,
            bytes: 1 << 20,
            map_bytes: 1 << 20,
        };
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits).unwrap();
        let first = inputs.insert(&vec![1; 512 << 10]).unwrap();
        let second = vec![2; 512 << 10];
        let missing = inputs.digest(&second);
        assert!(inputs.insert(&second).is_err());
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits).unwrap();
        assert_eq!(inputs.get(&first).unwrap().unwrap().len(), 512 << 10);
        assert_eq!(inputs.get(&missing).unwrap(), None);
    }

    #[test]
    fn invalid_chunks_and_byte_budget_do_not_consume_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert!(inputs.insert(&[]).is_err());
        assert!(inputs.insert(&vec![0; MAX_CHUNK_BYTES + 1]).is_err());
        assert!(inputs.insert(b"too large").is_err());
        let first = inputs.insert(b"1234567").unwrap();
        assert!(inputs.insert(b"xx").is_err());
        let second = inputs.insert(b"x").unwrap();
        drop(inputs);
        let inputs = RecoveryInputs::open(dir.path(), [1; 32], limits()).unwrap();
        assert_eq!(inputs.get(&first).unwrap().unwrap().len(), 7);
        assert_eq!(inputs.get(&second).unwrap(), Some(b"x".to_vec()));
    }
}
