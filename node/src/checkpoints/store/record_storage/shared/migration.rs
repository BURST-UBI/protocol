//! One durable migration job, scheduled by maintenance when migration is enabled.
//! A cached prepared input avoids decoding all heads again for each writer batch.
use super::{invalid, publication, staging, Envelope, Prepared, MAX_RECORD_BYTES};
use crate::checkpoint_store::record_storage::{self, retirement};
use crate::paged_metadata::{staged, Commitment, PAGE_BYTES};
use crate::shutdown::ShutdownController;
use burst_store::StoreError;
use burst_store_lmdb::{read_view::LedgerReadView, LmdbStore};

const JOB: &str = "checkpoint_heads/migration/v1/job";
const CURSOR: &str = "checkpoint_heads/migration/v1/cursor";
const ARCHIVES: &str = "checkpoint_record_archive/v1/";
const MAGIC: &[u8; 8] = b"HDMIGRA1";
// Use small batches to limit how long maintenance holds the database writer.
pub(super) const HEAD_ROWS_PER_BATCH: usize = 16;

fn poll(shutdown: &ShutdownController) -> Result<(), StoreError> {
    if shutdown.is_requested() {
        Err(StoreError::Backend("archive migration cancelled".into()))
    } else {
        Ok(())
    }
}

fn remains_legacy<'a>(view: &LedgerReadView<'a>, key: &'a str) -> Result<bool, StoreError> {
    let Some(row) = view.checkpoint_metadata_entries(key.as_bytes())?.next() else {
        return Ok(false);
    };
    let (found, bytes) = row?;
    Ok(found == key.as_bytes() && !super::is_shared(bytes)?)
}

struct Job {
    key: String,
    hash: [u8; 32],
    phase: u8,
}
impl Job {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(self.phase);
        bytes.extend_from_slice(&self.hash);
        bytes.extend_from_slice(self.key.as_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() <= 41 || bytes.len() > 297 || &bytes[..8] != MAGIC || bytes[8] > 2 {
            return Err(invalid("invalid migration job"));
        }
        let key = std::str::from_utf8(&bytes[41..])
            .map_err(invalid)?
            .to_owned();
        if !record_storage::is_record_key(&key) {
            return Err(invalid("invalid migration record key"));
        }
        Ok(Self {
            key,
            hash: bytes[9..41].try_into().unwrap(),
            phase: bytes[8],
        })
    }
    fn id(&self) -> [u8; 32] {
        let mut hash = burst_crypto::Blake2b256Hasher::default();
        hash.update(self.key.as_bytes());
        hash.update(&self.hash);
        hash.finalize()
    }
    fn envelope_state(&self) -> String {
        format!(
            "checkpoint_heads/envelope-staging/v1/{}",
            burst_types::BlockHash::new(self.id())
        )
    }
}

#[derive(Default)]
pub(in crate::checkpoints::store::record_storage) struct Worker {
    cached: Option<([u8; 32], Prepared<'static>, Commitment)>,
}

impl Worker {
    /// One active-job batch or one indexed archive candidate. The cursor survives
    /// restart; reaching the end wraps for later archive insertions.
    pub(in crate::checkpoints::store::record_storage) fn tick(
        &mut self,
        store: &LmdbStore,
        shutdown: &ShutdownController,
    ) -> Result<(), StoreError> {
        if shutdown.is_requested() {
            return Ok(());
        }
        let reader = store.tx_begin_read()?;
        if store.meta_store().get_meta_txn(&reader, JOB)?.is_some() {
            drop(reader);
            self.step(store, shutdown)?;
            return Ok(());
        }
        let cursor = store.meta_store().get_meta_txn(&reader, CURSOR)?;
        let after = cursor
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(invalid)?;
        if after.is_some_and(|key| key.len() > 256 || !key.starts_with(ARCHIVES)) {
            return Err(invalid("invalid archive migration cursor"));
        }
        let mut candidate = None;
        let page = store.meta_store().visit_prefix_page_txn(
            &reader,
            ARCHIVES,
            after,
            1,
            MAX_RECORD_BYTES as usize + 256,
            |key, bytes| {
                if record_storage::is_record_key(key) && !super::is_shared(bytes)? {
                    candidate = Some(key.to_owned());
                }
                Ok(())
            },
        )?;
        drop(reader);
        if shutdown.is_requested() {
            return Ok(());
        }
        if let Some(key) = candidate {
            self.select_with_poll(store, &key, &|| poll(shutdown))?;
        }
        if shutdown.is_requested() {
            return Ok(());
        }
        #[cfg(test)]
        let mut timing = super::WriteProbe::new("cursor");
        let mut batch = store.tx_begin_write()?;
        #[cfg(test)]
        timing.acquired();
        if batch.get_meta(CURSOR)? != cursor {
            return Err(invalid("archive migration cursor changed"));
        }
        match page.last_key {
            Some(key) => batch.put_meta(CURSOR, key.as_bytes())?,
            None if cursor.is_some() => batch.delete_meta(CURSOR)?,
            None => return Ok(()),
        }
        batch.commit()
    }

    #[cfg(test)]
    pub(super) fn select(&mut self, store: &LmdbStore, key: &str) -> Result<bool, StoreError> {
        self.select_with_poll(store, key, &|| Ok(()))
    }

    /// Persist the source key and hash before staging. An unfinished job
    /// keeps exclusive ownership of the migration namespace.
    fn select_with_poll(
        &mut self,
        store: &LmdbStore,
        key: &str,
        poll: &dyn Fn() -> Result<(), StoreError>,
    ) -> Result<bool, StoreError> {
        poll()?;
        if key.len() > 256 || !record_storage::is_record_key(key) {
            return Err(invalid("invalid migration selection"));
        }
        let reader = store.tx_begin_read()?;
        if store.meta_store().get_meta_txn(&reader, JOB)?.is_some() {
            return Ok(false);
        }
        let Some(descriptor) = store.meta_store().get_meta_txn(&reader, key)? else {
            return Ok(false);
        };
        if super::is_shared(&descriptor)? {
            return Ok(false);
        }
        let bytes = record_storage::decode_stored(
            key,
            &descriptor,
            |name| {
                poll()?;
                store.meta_store().get_meta_txn(&reader, name)
            },
            |hash| store.read_view(&reader)?.checkpoint_head_node(hash),
            MAX_RECORD_BYTES as usize,
        )?
        .into_owned();
        let Some(prepared) = super::prepare_owned(bytes, poll)? else {
            return Ok(false);
        };
        let job = Job {
            key: key.to_owned(),
            hash: burst_crypto::blake2b_256(&prepared.bytes),
            phase: 0,
        };
        drop(reader);
        #[cfg(test)]
        let mut timing = super::WriteProbe::new("select");
        let mut batch = store.tx_begin_write()?;
        #[cfg(test)]
        timing.acquired();
        poll()?;
        if batch.get_meta(JOB)?.is_some()
            || batch.get_meta(key)?.as_deref() != Some(descriptor.as_slice())
        {
            return Ok(false);
        }
        batch.put_meta(JOB, &job.encode())?;
        batch.commit()?;
        #[cfg(test)]
        timing.finish();
        self.cache(&job, prepared);
        Ok(true)
    }

    fn cache(&mut self, job: &Job, prepared: Prepared<'static>) {
        let mut digest = burst_crypto::Blake2b256Hasher::default();
        digest.update(&prepared.bytes[..prepared.prefix]);
        digest.update(&prepared.bytes[prepared.end..]);
        let envelope = Commitment {
            size: prepared.prefix + prepared.bytes.len() - prepared.end,
            hash: digest.finalize(),
        };
        self.cached = Some((job.id(), prepared, envelope));
    }

    fn set_phase(store: &LmdbStore, job: &mut Job, phase: u8) -> Result<(), StoreError> {
        #[cfg(test)]
        let mut timing = super::WriteProbe::new("phase");
        let mut batch = store.tx_begin_write()?;
        #[cfg(test)]
        timing.acquired();
        if batch.get_meta(JOB)?.as_deref() != Some(job.encode().as_slice()) {
            return Err(invalid("migration job changed"));
        }
        job.phase = phase;
        batch.put_meta(JOB, &job.encode())?;
        batch.commit()
    }

    /// Advance one stage: load source, build heads, copy pages, publish or clean up.
    /// Source loading and publication checks read the full record. Readers are
    /// released before writes and are never retained between calls.
    pub(super) fn step(
        &mut self,
        store: &LmdbStore,
        shutdown: &ShutdownController,
    ) -> Result<bool, StoreError> {
        if shutdown.is_requested() {
            return Ok(false);
        }
        let reader = store.tx_begin_read()?;
        let Some(bytes) = store.meta_store().get_meta_txn(&reader, JOB)? else {
            self.cached = None;
            return Ok(true);
        };
        let mut job = Job::decode(&bytes)?;
        drop(reader);
        if job.phase == 1 {
            if publication::abandon(store, &job.key, &job.id(), &job.envelope_state())? {
                Self::set_phase(store, &mut job, 2)?;
            }
            self.cached = None;
            return Ok(false);
        }
        if job.phase == 2 {
            #[cfg(test)]
            let mut timing = super::WriteProbe::new("retire");
            let mut batch = store.tx_begin_write()?;
            #[cfg(test)]
            timing.acquired();
            let done = retirement::step(&mut batch, &job.key)?;
            if done {
                batch.delete_meta(JOB)?;
            }
            if shutdown.is_requested() {
                return Ok(false);
            }
            batch.commit()?;
            if done {
                self.cached = None;
            }
            return Ok(done);
        }
        if self
            .cached
            .as_ref()
            .is_none_or(|(id, _, _)| *id != job.id())
        {
            let reader = store.tx_begin_read()?;
            let descriptor = store.meta_store().get_meta_txn(&reader, &job.key)?;
            if descriptor
                .as_ref()
                .map(|bytes| super::is_shared(bytes))
                .transpose()?
                .unwrap_or(true)
            {
                drop(reader);
                Self::set_phase(store, &mut job, 1)?;
                return Ok(false);
            }
            let bytes = record_storage::decode_stored(
                &job.key,
                descriptor.as_deref().expect("legacy source checked"),
                |name| {
                    poll(shutdown)?;
                    store.meta_store().get_meta_txn(&reader, name)
                },
                |hash| store.read_view(&reader)?.checkpoint_head_node(hash),
                MAX_RECORD_BYTES as usize,
            )?
            .into_owned();
            drop(reader);
            if burst_crypto::blake2b_256(&bytes) != job.hash {
                Self::set_phase(store, &mut job, 1)?;
                return Ok(false);
            }
            let prepared = super::prepare_owned(bytes, &|| poll(shutdown))?
                .ok_or_else(|| invalid("migration source is no longer canonical"))?;
            self.cache(&job, prepared);
        }
        if shutdown.is_requested() {
            return Ok(false);
        }
        let (_, prepared, envelope) = self.cached.as_ref().expect("prepared above");
        let reader = store.tx_begin_read()?;
        let view = store.read_view(&reader)?;
        if !remains_legacy(&view, &job.key)? {
            drop(reader);
            Self::set_phase(store, &mut job, 1)?;
            self.cached = None;
            return Ok(false);
        }
        let (offset, root) = staging::progress(&view, &job.id(), job.hash, prepared.heads.len())?;
        let copied = staged::progress(
            &view,
            &job.envelope_state(),
            *envelope,
            MAX_RECORD_BYTES as usize,
        )?;
        drop(reader);
        if root.is_none() {
            let end = prepared.heads.len().min(offset + HEAD_ROWS_PER_BATCH);
            #[cfg(test)]
            let mut timing = super::WriteProbe::new("heads");
            let mut batch = store.tx_begin_write()?;
            #[cfg(test)]
            timing.acquired();
            if !remains_legacy(&batch.read_view(), &job.key)? {
                drop(batch);
                Self::set_phase(store, &mut job, 1)?;
                self.cached = None;
                return Ok(false);
            }
            staging::advance(
                &mut batch,
                &job.id(),
                job.hash,
                prepared.heads.len(),
                offset,
                &prepared.heads[offset..end],
            )?;
            if shutdown.is_requested() {
                return Ok(false);
            }
            batch.commit()?;
            return Ok(false);
        }
        if copied < envelope.size {
            let end = envelope
                .size
                .min(copied + staged::PAGES_PER_BATCH * PAGE_BYTES);
            let mut bytes = Vec::with_capacity(end - copied);
            if copied < prepared.prefix {
                bytes.extend_from_slice(&prepared.bytes[copied..end.min(prepared.prefix)]);
            }
            if end > prepared.prefix {
                let start = copied.max(prepared.prefix) - prepared.prefix + prepared.end;
                let stop = end - prepared.prefix + prepared.end;
                bytes.extend_from_slice(&prepared.bytes[start..stop]);
            }
            #[cfg(test)]
            let mut timing = super::WriteProbe::new("envelope");
            let mut batch = store.tx_begin_write()?;
            #[cfg(test)]
            timing.acquired();
            if !remains_legacy(&batch.read_view(), &job.key)? {
                drop(batch);
                Self::set_phase(store, &mut job, 1)?;
                self.cached = None;
                return Ok(false);
            }
            staged::append(
                &Envelope::new(&job.key),
                &job.envelope_state(),
                &mut batch,
                *envelope,
                MAX_RECORD_BYTES as usize,
                copied,
                &bytes,
            )?;
            if shutdown.is_requested() {
                return Ok(false);
            }
            batch.commit()?;
            return Ok(false);
        }
        let verified = publication::Publication::verify_with_poll(
            store,
            &job.key,
            job.id(),
            prepared,
            job.envelope_state(),
            &|| poll(shutdown),
        );
        match verified {
            Ok(publication) => {
                if shutdown.is_requested() {
                    return Ok(false);
                }
                publication.commit(store)?;
                Self::set_phase(store, &mut job, 2)?;
                self.cached = None;
                Ok(false)
            }
            Err(StoreError::StaleRead(_)) => {
                Self::set_phase(store, &mut job, 1)?;
                self.cached = None;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_store::record_storage::RecordLocation;
    const KEY: &str = "checkpoint_record_archive/v1/dev/worker/1";

    fn seed(store: &LmdbStore) -> Vec<u8> {
        let bytes = super::super::tests::record(true);
        let mut batch = store.tx_begin_write().unwrap();
        crate::paged_metadata::write_bytes_at(RecordLocation::new(KEY), &mut batch, &bytes)
            .unwrap();
        batch.commit().unwrap();
        bytes
    }

    #[test]
    fn selection_cursor_skips_one_unsupported_record_and_resumes_after_worker_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        seed(&store);
        let earlier = "checkpoint_record_archive/v1/dev/worker/0";
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(earlier, b"legacy opaque").unwrap();
        batch.commit().unwrap();
        let shutdown = ShutdownController::new();
        Worker::default().tick(&store, &shutdown).unwrap();
        {
            let reader = store.tx_begin_read().unwrap();
            assert_eq!(
                store
                    .meta_store()
                    .get_meta_txn(&reader, CURSOR)
                    .unwrap()
                    .unwrap(),
                earlier.as_bytes()
            );
            assert!(store
                .meta_store()
                .get_meta_txn(&reader, JOB)
                .unwrap()
                .is_none());
        }
        Worker::default().tick(&store, &shutdown).unwrap();
        let reader = store.tx_begin_read().unwrap();
        assert_eq!(
            store
                .meta_store()
                .get_meta_txn(&reader, CURSOR)
                .unwrap()
                .unwrap(),
            KEY.as_bytes()
        );
        assert_eq!(
            Job::decode(
                &store
                    .meta_store()
                    .get_meta_txn(&reader, JOB)
                    .unwrap()
                    .unwrap()
            )
            .unwrap()
            .key,
            KEY
        );
    }

    #[test]
    fn cancellation_during_source_read_does_not_create_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        seed(&store);
        let calls = std::cell::Cell::new(0);
        let before = store.env().info().last_txn_id;
        assert!(Worker::default()
            .select_with_poll(&store, KEY, &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 3 {
                    Err(StoreError::Backend("cancelled during page read".into()))
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert_eq!(calls.get(), 3);
        assert_eq!(store.env().info().last_txn_id, before);
        let reader = store.tx_begin_read().unwrap();
        assert!(store
            .meta_store()
            .get_meta_txn(&reader, JOB)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn scheduled_migration_finishes_and_worker_joins_on_shutdown() {
        use std::{sync::Arc, time::Duration};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
        let original = seed(&store);
        let shutdown = Arc::new(ShutdownController::new());
        let worker = crate::checkpoint_store::record_storage::maintenance::start_with_migration(
            store.clone(),
            shutdown.clone(),
            true,
        );
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let done = {
                    let reader = store.tx_begin_read().unwrap();
                    let descriptor = store
                        .meta_store()
                        .get_meta_txn(&reader, KEY)
                        .unwrap()
                        .unwrap();
                    super::super::is_shared(&descriptor).unwrap()
                        && store
                            .meta_store()
                            .get_meta_txn(&reader, JOB)
                            .unwrap()
                            .is_none()
                };
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_ok(),
            "migration did not finish within the fixture budget"
        );
        let reader = store.tx_begin_read().unwrap();
        assert_eq!(
            record_storage::get_txn(&store.meta_store(), &reader, KEY)
                .unwrap()
                .unwrap(),
            original
        );
    }

    #[tokio::test]
    #[ignore = "capped-host four-minute maintenance contention probe; not signed payment acceptance"]
    async fn scheduled_migration_progresses_with_competing_writes() {
        use std::{
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
            time::{Duration, Instant},
        };
        const TRAFFIC: &str = "migration_probe/competing_writer";
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 64 << 20).unwrap());
        let original = super::super::tests::record_with_heads(true, 4096);
        let mut batch = store.tx_begin_write().unwrap();
        crate::paged_metadata::write_bytes_at(RecordLocation::new(KEY), &mut batch, &original)
            .unwrap();
        batch.commit().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let traffic = {
            let store = store.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut latencies = Vec::new();
                while !stop.load(Ordering::Acquire) {
                    let start = Instant::now();
                    let mut batch = store.tx_begin_write().unwrap();
                    let sequence = (latencies.len() as u64 + 1).to_be_bytes();
                    batch.put_meta(TRAFFIC, &sequence).unwrap();
                    batch.commit().unwrap();
                    latencies.push(start.elapsed().as_micros());
                    std::thread::sleep(Duration::from_millis(100));
                }
                latencies
            })
        };
        let shutdown = Arc::new(ShutdownController::new());
        let started = Instant::now();
        let worker = crate::checkpoint_store::record_storage::maintenance::start_with_migration(
            store.clone(),
            shutdown.clone(),
            true,
        );
        // 256 head ticks plus selection, envelope, publication and old-page cleanup.
        // This fixed fixture budget is separate from production payment latency.
        let result = tokio::time::timeout(Duration::from_secs(360), async {
            loop {
                let done = {
                    let reader = store.tx_begin_read().unwrap();
                    let descriptor = store
                        .meta_store()
                        .get_meta_txn(&reader, KEY)
                        .unwrap()
                        .unwrap();
                    super::super::is_shared(&descriptor).unwrap()
                        && store
                            .meta_store()
                            .get_meta_txn(&reader, JOB)
                            .unwrap()
                            .is_none()
                };
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        shutdown.shutdown();
        stop.store(true, Ordering::Release);
        let joined = tokio::time::timeout(Duration::from_secs(1), worker).await;
        let mut latencies = traffic.join().unwrap();
        assert!(
            result.is_ok(),
            "scheduled migration did not complete under competing writes"
        );
        joined.unwrap().unwrap();
        assert!(
            latencies.len() >= 256,
            "competing writer must make sustained progress"
        );
        let writes = latencies.len();
        latencies.sort_unstable();
        let reader = store.tx_begin_read().unwrap();
        assert_eq!(
            record_storage::get_txn(&store.meta_store(), &reader, KEY)
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            store
                .meta_store()
                .get_meta_txn(&reader, TRAFFIC)
                .unwrap()
                .unwrap(),
            (writes as u64).to_be_bytes()
        );
        println!("HISTORY_MIGRATION_CONTENTION heads=4096 writes={writes} elapsed_ms={} writer_median_us={} writer_p95_us={} writer_max_us={} exact=true scheduler_seconds=1 signed_payments=false release=false",
            started.elapsed().as_millis(), latencies[writes / 2], latencies[(writes * 95).div_ceil(100) - 1], latencies[writes - 1]);
    }

    #[test]
    fn durable_job_resumes_each_phase_after_reopen_and_finishes_retirement() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let bytes = seed(&store);
        let mut worker = Worker::default();
        assert!(worker.select(&store, KEY).unwrap());
        assert!(!worker.select(&store, KEY).unwrap());
        let shutdown = ShutdownController::new();
        let mut completed = false;
        for _ in 0..20 {
            worker = Worker::default();
            drop(store);
            store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            if worker.step(&store, &shutdown).unwrap() {
                completed = true;
                break;
            }
        }
        assert!(completed);
        let batch = store.tx_begin_write().unwrap();
        assert!(batch.get_meta(JOB).unwrap().is_none());
        assert!(super::super::is_shared(&batch.get_meta(KEY).unwrap().unwrap()).unwrap());
        assert_eq!(
            record_storage::get_batch(&batch, KEY).unwrap().unwrap(),
            bytes
        );
    }

    #[test]
    fn cancelled_worker_is_idle_and_newer_shared_source_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        seed(&store);
        let mut worker = Worker::default();
        worker.select(&store, KEY).unwrap();
        let cancelled = ShutdownController::new();
        cancelled.shutdown();
        let txn = store.env().info().last_txn_id;
        assert!(!worker.step(&store, &cancelled).unwrap());
        assert_eq!(txn, store.env().info().last_txn_id);
        let shutdown = ShutdownController::new();
        worker.step(&store, &shutdown).unwrap();
        let newer = super::super::tests::record(false);
        let mut batch = store.tx_begin_write().unwrap();
        record_storage::put(&mut batch, KEY, &newer).unwrap();
        batch.commit().unwrap();
        let mut completed = false;
        for _ in 0..20 {
            if worker.step(&store, &shutdown).unwrap() {
                completed = true;
                break;
            }
        }
        assert!(completed);
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            record_storage::get_batch(&batch, KEY).unwrap().unwrap(),
            newer
        );
        assert!(batch.get_meta(JOB).unwrap().is_none());
    }
}
