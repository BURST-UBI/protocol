//! Persisted bytes are replayed through an independently authenticated cursor.
//! No saved next-hash, completion bit or local digest becomes proof authority.
use super::{HistoricalBlock, HistoryContext, HistoryCursor, MAX_WITNESS_BLOCKS, MAX_WITNESS_BYTES};
use crate::confirmed_replay::scratch_file::{ScratchLimit, PERSISTENT_INPUTS_DIRECTORY};
use burst_store::StoreError;
use burst_store_lmdb::recovery_inputs::{InputWriteError, Limits, RecoveryInputs};
#[cfg(test)]
use std::path::Path;

const LOCK_ALLOWANCE: u64 = 64 << 10;
mod capacity;
use capacity::retry_capacity;
fn inventory_key() -> [u8; 32] { burst_crypto::blake2b_256(b"BURST/retained-inventory-raw/v1\0") }
fn invalid(error: impl ToString) -> StoreError { StoreError::Corruption(error.to_string()) }

pub(crate) struct DurableHistorySession {
    inputs: RecoveryInputs,
    context: HistoryContext,
    _limit: ScratchLimit,
}
/// Raw inventory bookkeeping, never authenticated recovery progress.
/// Keep the environment alive only while its shared budget lease is retained.
pub(crate) struct DurableInventoryInputs {
    pub(crate) raw: RecoveryInputs,
    _limit: ScratchLimit,
}
impl DurableHistorySession {
    pub(crate) async fn open(directory: std::path::PathBuf, context: HistoryContext,
        map_bytes: usize, limit: ScratchLimit, shutdown: &crate::shutdown::ShutdownController,
    ) -> Result<Option<Self>, StoreError> {
        let encoded = bincode::serialize(&(context.network, context.genesis, context.epoch, context.state_hash)).map_err(invalid)?;
        let expected = burst_crypto::blake2b_256_multi(&[b"BURST/durable-history-session/v1\0", &encoded]);
        let mut retained: Option<RecoveryInputs> = None;
        loop {
            if shutdown.is_requested() { return Ok(None); }
            let directory = directory.clone();
            let worker_limit = limit.clone();
            let (inputs, done) = tokio::task::spawn_blocking(move || {
                let reserved = DurableHistoryInputs::reservation(map_bytes)?;
                worker_limit.check_persistent(&directory, reserved).map_err(invalid)?;
                let inputs = match retained {
                    Some(inputs) => inputs,
                    None => RecoveryInputs::open_retained(&directory.join(PERSISTENT_INPUTS_DIRECTORY), expected,
                        Limits { chunks: 131_072, bytes: map_bytes as u64, map_bytes })?,
                };
                if crate::confirmed_replay::scratch_file::persistent_bytes(&directory).map_err(invalid)? > reserved {
                    return Err(invalid("durable input files exceed reserved capacity"));
                }
                if inputs.root_context() == expected && inputs.reset_target()?.is_none() {
                    return Ok::<_, StoreError>((Some(inputs), true));
                }
                // Finish an interrupted immutable target before selecting the
                // current context. Both are raw-cache cleanup, never recovery.
                if inputs.reset_target()?.is_none() { inputs.begin_reset(expected)?; }
                let complete = inputs.reset_page(128, MAX_WITNESS_BYTES + 64)?.complete;
                Ok((if complete { None } else { Some(inputs) }, false))
            }).await.map_err(invalid)??;
            retained = inputs;
            if done {
                if shutdown.is_requested() { return Ok(None); }
                return Ok(Some(Self { inputs: retained.expect("ready session"), context, _limit: limit }));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn select_inventory(&self) -> Result<DurableInventoryInputs, StoreError> {
        self.select_inventory_checked().map_err(Into::into)
    }

    pub(crate) fn select_inventory_checked(&self) -> Result<DurableInventoryInputs, InputWriteError> {
        Ok(DurableInventoryInputs { raw: self.inputs.select_stream_checked(inventory_key())?, _limit: self._limit.clone() })
    }

    #[cfg(test)]
    pub(crate) fn select(&self, cursor: HistoryCursor) -> Result<DurableHistoryInputs, StoreError> {
        self.select_checked(cursor).map_err(Into::into)
    }

    fn select_checked(&self, cursor: HistoryCursor) -> Result<DurableHistoryInputs, InputWriteError> {
        if cursor.context() != self.context { return Err(invalid("durable stream has foreign recovery authority").into()); }
        // A stable floor lets a later inventory reuse this account's prefix,
        // even when it needs a different subset of retained bodies.
        let key = DurableHistoryInputs::input_context(&cursor, 0)?;
        let inputs = self.inputs.select_stream_checked(key)?;
        if inputs.progress()?.positions > cursor.next().expect("checked cursor").height {
            return Err(invalid("durable stream exceeds authenticated account height").into());
        }
        Ok(DurableHistoryInputs { inputs, cursor, floor: 0, consumed: 0, _limit: self._limit.clone() })
    }
    pub(crate) async fn select_with_capacity(self, cursor: HistoryCursor,
        shutdown: &crate::shutdown::ShutdownController,
    ) -> Result<(Self, Option<DurableHistoryInputs>), StoreError> {
        retry_capacity(self, move |session| session.select_checked(cursor.clone()), shutdown).await
    }

}

pub(crate) struct DurableHistoryInputs {
    inputs: RecoveryInputs,
    cursor: HistoryCursor,
    floor: u64,
    consumed: u64,
    // Drop the environment before releasing the directory-wide budget lease.
    _limit: ScratchLimit,
}
impl DurableHistoryInputs {
    pub(crate) fn input_context(cursor: &HistoryCursor, floor: u64) -> Result<[u8;32], StoreError> {
        let head = cursor.next().ok_or_else(|| invalid("empty durable history cursor"))?;
        if floor >= head.height { return Err(invalid("invalid durable history floor")); }
        let context = cursor.context();
        let bytes = bincode::serialize(&(context.network, context.genesis, context.epoch,
            context.state_hash, head.account, head.hash, head.height, floor)).map_err(invalid)?;
        Ok(burst_crypto::blake2b_256_multi(&[b"BURST/durable-history-inputs/v1\0", &bytes]))
    }

    #[cfg(test)]
    pub(crate) fn open(directory: &Path, cursor: HistoryCursor, floor: u64,
        total_bytes: u64, map_bytes: usize) -> Result<Self, StoreError> {
        let reserved = Self::reservation(map_bytes)?;
        let limit = ScratchLimit::acquire_reserved(directory, total_bytes, reserved).map_err(invalid)?;
        Self::open_with_limit(directory, cursor, floor, map_bytes, limit)
    }

    pub(crate) fn reservation(map_bytes: usize) -> Result<u64, StoreError> {
        (map_bytes as u64).checked_add(LOCK_ALLOWANCE).ok_or_else(|| invalid("durable input reservation overflow"))
    }

    #[cfg(test)]
    pub(crate) fn open_with_limit(directory: &Path, cursor: HistoryCursor, floor: u64,
        map_bytes: usize, limit: ScratchLimit) -> Result<Self, StoreError> {
        let context = Self::input_context(&cursor, floor)?;
        let reserved = Self::reservation(map_bytes)?;
        limit.check_persistent(directory, reserved).map_err(invalid)?;
        let inputs = RecoveryInputs::open(&directory.join(PERSISTENT_INPUTS_DIRECTORY), context,
            Limits { chunks: 131_072, bytes: map_bytes as u64, map_bytes })?;
        if crate::confirmed_replay::scratch_file::persistent_bytes(directory).map_err(invalid)? > reserved {
            return Err(invalid("durable input files exceed reserved capacity"));
        }
        let progress = inputs.progress()?;
        if progress.positions > cursor.next().expect("checked nonempty cursor").height - floor {
            return Err(invalid("durable inputs exceed requested history range"));
        }
        Ok(Self { inputs, cursor, floor, consumed: 0, _limit: limit })
    }

    /// Select another expected account/range in this recovery session without
    /// reopening/rescanning its environment or reserving a second map. The
    /// supplied certified cursor, not the saved stream, defines authority.
    #[cfg(test)]
    pub(crate) fn select_stream(&self, cursor: HistoryCursor, floor: u64) -> Result<Self, StoreError> {
        if cursor.context() != self.cursor.context() {
            return Err(invalid("durable history stream belongs to another recovery context"));
        }
        let context = Self::input_context(&cursor, floor)?;
        let inputs = self.inputs.select_stream(context)?;
        if inputs.progress()?.positions > cursor.next().expect("checked nonempty cursor").height - floor {
            return Err(invalid("durable inputs exceed requested history range"));
        }
        Ok(Self { inputs, cursor, floor, consumed: 0, _limit: self._limit.clone() })
    }

    /// Await this operation through shutdown; only the network request is
    /// cancelled. Each bounded disk worker returns ownership before shutdown is
    /// acknowledged. None means cancellation; an empty page means completion.
    pub(crate) async fn next_page(
        self,
        registry: &std::sync::Arc<tokio::sync::RwLock<crate::connection_registry::ConnectionRegistry>>,
        shutdown: &crate::shutdown::ShutdownController,
    ) -> Result<(Self, Option<Vec<HistoricalBlock>>), StoreError> {
        if shutdown.is_requested() { return Ok((self, None)); }
        let (inputs, page) = tokio::task::spawn_blocking(move || {
            let mut inputs = self;
            let page = inputs.replay_page()?;
            Ok::<_, StoreError>((inputs, page))
        }).await.map_err(invalid)??;
        if shutdown.is_requested() { return Ok((inputs, None)); }
        if !page.is_empty() || Self::height(&inputs.cursor) == inputs.floor {
            return inputs.finish_page(page, shutdown).await;
        }
        let mut cursor = inputs.cursor.clone();
        let mut stop = shutdown.subscribe();
        let blocks = tokio::select! {
            biased;
            _ = stop.recv() => return Ok((inputs, None)),
            blocks = crate::checkpoint_sync::witness::fetch_suffix(registry, &mut cursor, inputs.floor) => blocks?,
        };
        let chunks = tokio::task::spawn_blocking(move || {
            blocks.iter().map(bincode::serialize).collect::<Result<Vec<_>, _>>().map_err(invalid)
        }).await.map_err(invalid)??;
        inputs.append_with_capacity(chunks, shutdown).await
    }

    pub(crate) async fn append_with_capacity(self, chunks: Vec<Vec<u8>>,
        shutdown: &crate::shutdown::ShutdownController,
    ) -> Result<(Self, Option<Vec<HistoricalBlock>>), StoreError> {
        let (inputs, page) = retry_capacity(self, move |inputs| inputs.append_checked(&chunks), shutdown).await?;
        match page {
            Some(page) => inputs.finish_page(page, shutdown).await,
            None => Ok((inputs, None)),
        }
    }

    async fn finish_page(self, page: Vec<HistoricalBlock>, shutdown: &crate::shutdown::ShutdownController)
        -> Result<(Self, Option<Vec<HistoricalBlock>>), StoreError> {
        if shutdown.is_requested() { return Ok((self, None)); }
        if Self::height(&self.cursor) != self.floor { return Ok((self, Some(page))); }
        // Append may already have committed. Retry sealing separately, retaining
        // these authenticated proofs instead of replaying or appending them again.
        let (inputs, sealed) = retry_capacity(self, |inputs| inputs.seal_checked(), shutdown).await?;
        Ok((inputs, sealed.map(|()| page)))
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> &HistoryCursor { &self.cursor }
    fn height(cursor: &HistoryCursor) -> u64 { cursor.next().map_or(0, |head| head.height) }

    /// Authenticate at most one existing witness-sized page. Empty means no
    /// saved input remains, not that the requested history range is complete.
    pub(crate) fn replay_page(&mut self) -> Result<Vec<HistoricalBlock>, StoreError> {
        let progress = self.inputs.progress()?;
        let mut chunks = Vec::new();
        let mut bytes = 0;
        while chunks.len() < MAX_WITNESS_BLOCKS && self.consumed + (chunks.len() as u64) < progress.positions {
            let chunk = self.inputs.read_position(self.consumed + chunks.len() as u64)?
                .ok_or_else(|| invalid("durable history input disappeared"))?;
            if chunk.len() > MAX_WITNESS_BYTES - bytes { break; }
            bytes += chunk.len();
            chunks.push(chunk);
        }
        if chunks.is_empty() {
            if self.consumed != progress.positions { return Err(invalid("durable history chunk exceeds replay budget")); }
            if progress.complete && Self::height(&self.cursor) != self.floor {
                return Err(invalid("sealed history input does not reach requested floor"));
            }
            return Ok(Vec::new());
        }
        let mut cursor = self.cursor.clone();
        let proofs = cursor.accept(&chunks).map_err(invalid)?;
        let consumed = self.consumed + chunks.len() as u64;
        if Self::height(&cursor) < self.floor
            || (progress.complete && consumed == progress.positions && Self::height(&cursor) != self.floor) {
            return Err(invalid("durable history input range differs"));
        }
        self.cursor = cursor;
        self.consumed = consumed;
        Ok(proofs)
    }

    /// Authenticate before persistence; publish cursor progress only after the
    /// whole bounded batch commits. Reopening always starts authentication again
    /// from the supplied certified head, even for an already sealed download.
    #[cfg(test)]
    pub(crate) fn append(&mut self, chunks: &[Vec<u8>]) -> Result<Vec<HistoricalBlock>, StoreError> {
        self.append_checked(chunks).map_err(Into::into)
    }

    fn append_checked(&mut self, chunks: &[Vec<u8>]) -> Result<Vec<HistoricalBlock>, InputWriteError> {
        let progress = self.inputs.progress()?;
        if progress.complete || self.consumed != progress.positions {
            return Err(invalid("saved history must be replayed before append").into());
        }
        let mut cursor = self.cursor.clone();
        let proofs = cursor.accept(chunks).map_err(invalid)?;
        if Self::height(&cursor) < self.floor { return Err(invalid("history append passes requested floor").into()); }
        self.inputs.append_batch_checked(self.consumed, chunks)?;
        self.consumed += chunks.len() as u64;
        self.cursor = cursor;
        Ok(proofs)
    }

    #[cfg(test)]
    pub(crate) fn seal(&self) -> Result<(), StoreError> {
        self.seal_checked().map_err(Into::into)
    }

    fn seal_checked(&self) -> Result<(), InputWriteError> {
        let progress = self.inputs.progress()?;
        if self.consumed != progress.positions || Self::height(&self.cursor) != self.floor {
            return Err(invalid("cannot seal incomplete authenticated history input").into());
        }
        self.inputs.seal_checked(self.consumed)
    }
}
