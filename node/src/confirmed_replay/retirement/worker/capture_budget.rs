//! Capture budgets: native file-growth enforcement and cooperative time checks.
//! A time limit is observed at polling points; it does not interrupt blocking I/O.
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) struct CaptureLimits {
    pub duration: Duration,
    pub growth_bytes: u64,
    pub headroom_bytes: u64,
}

pub(crate) struct CaptureBudget {
    started: Instant,
    initial_bytes: u64,
    limits: CaptureLimits,
}

impl CaptureBudget {
    pub fn start(store: &LmdbStore, limits: CaptureLimits) -> Result<Self, StoreError> {
        Ok(Self {
            started: Instant::now(),
            initial_bytes: Self::extent(store)?,
            limits,
        })
    }

    fn extent(store: &LmdbStore) -> Result<u64, StoreError> {
        store
            .env()
            .real_disk_size()
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    /// Compute the file-extent ceiling to install under the capture writer before
    /// opening its reader. The ceiling does not bound dirty-page RAM or physical
    /// filesystem allocation.
    pub fn file_ceiling(&self, store: &LmdbStore) -> usize {
        let map = store.env().info().map_size;
        self.initial_bytes
            .saturating_add(self.limits.growth_bytes)
            .min((map as u64).saturating_sub(self.limits.headroom_bytes)) as usize
    }

    pub fn exceeded(&self, store: &LmdbStore) -> Result<Option<&'static str>, StoreError> {
        self.exceeded_at(store, Instant::now())
    }

    fn exceeded_at(
        &self,
        store: &LmdbStore,
        now: Instant,
    ) -> Result<Option<&'static str>, StoreError> {
        if now.saturating_duration_since(self.started) >= self.limits.duration {
            return Ok(Some("capture lifetime"));
        }
        let bytes = Self::extent(store)?;
        if bytes.saturating_sub(self.initial_bytes) >= self.limits.growth_bytes {
            return Ok(Some("capture file growth"));
        }
        // Conservative: high-water extent includes reusable pages. Do not call
        // it live-page usage or filesystem allocation, or open a second reader.
        let map = store.env().info().map_size as u64;
        if map.saturating_sub(bytes) < self.limits.headroom_bytes {
            return Ok(Some("capture map headroom"));
        }
        Ok(None)
    }
}

/// A synchronous reader owner keeps this lease until all its readers drop. Filesystem
/// checks are rate limited; cancellation is latched and storage errors retained.
pub(crate) struct CapturePoll<'a> {
    store: &'a LmdbStore,
    budget: CaptureBudget,
    growth: Option<(
        burst_store_lmdb::FileGrowthLimit,
        burst_store_lmdb::GrowthWatch,
    )>,
    next: std::cell::Cell<Instant>,
    stopped: std::cell::Cell<bool>,
    error: std::cell::RefCell<Option<StoreError>>,
}

impl<'a> CapturePoll<'a> {
    pub fn start(store: &'a LmdbStore, limits: CaptureLimits) -> Result<Self, StoreError> {
        let mut writer = store.tx_begin_write()?;
        let budget = CaptureBudget::start(store, limits)?;
        let growth = if budget.exceeded(store)?.is_some() {
            None
        } else {
            writer
                .try_limit_file_extent(budget.file_ceiling(store))?
                .map(|lease| {
                    let watch = lease.watch();
                    (lease, watch)
                })
        };
        let stopped = growth.is_none();
        drop(writer);
        Ok(Self {
            store,
            budget,
            growth,
            next: std::cell::Cell::new(Instant::now()),
            stopped: std::cell::Cell::new(stopped),
            error: std::cell::RefCell::new(None),
        })
    }

    pub fn cancelled(&self) -> bool {
        if self.stopped.get() {
            return true;
        }
        if self
            .growth
            .as_ref()
            .is_some_and(|(_, watch)| watch.was_exceeded())
        {
            self.stopped.set(true);
            return true;
        }
        let now = Instant::now();
        if now < self.next.get() {
            return false;
        }
        self.next.set(now + Duration::from_millis(10));
        match self.budget.exceeded_at(self.store, now) {
            Ok(None) => false,
            Ok(Some(reason)) => {
                tracing::debug!(reason, "retirement reconciliation budget exhausted");
                self.stopped.set(true);
                true
            }
            Err(error) => {
                *self.error.borrow_mut() = Some(error);
                self.stopped.set(true);
                true
            }
        }
    }

    pub fn finish(self) -> Result<bool, StoreError> {
        let stopped = self.stopped.get()
            || self
                .growth
                .as_ref()
                .is_some_and(|(_, watch)| watch.was_exceeded());
        match self.error.into_inner() {
            Some(error) => Err(error),
            None => Ok(stopped || self.budget.exceeded(self.store)?.is_some()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_lease_latches_native_pressure_and_defers_to_retry() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let limits = CaptureLimits {
            duration: Duration::from_secs(1),
            growth_bytes: 1 << 20,
            headroom_bytes: 1 << 20,
        };
        let poll = CapturePoll::start(&store, limits).unwrap();
        assert!(!poll.cancelled());
        let reader = store.tx_begin_read().unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        let outcome = writer.growth_outcome().unwrap();
        let retry = writer.reserve_growth_retry().unwrap().unwrap();
        assert!(matches!(
            writer.put_meta("pressure", &vec![0; 2 << 20]),
            Err(StoreError::GrowthLimit(_))
        ));
        drop(writer);
        assert!(
            poll.cancelled(),
            "native pressure must bypass the filesystem poll throttle"
        );
        assert!(!outcome.watch().wait_released(Duration::ZERO));
        drop(reader);
        assert!(poll.finish().unwrap());
        assert!(outcome.watch().wait_released(Duration::ZERO));
        let deferred = CapturePoll::start(&store, limits).unwrap();
        assert!(deferred.cancelled());
        assert!(deferred.finish().unwrap());
        drop(retry);
        let resumed = CapturePoll::start(&store, limits).unwrap();
        assert!(!resumed.cancelled());
        assert!(!resumed.finish().unwrap());
        let mut expired = CapturePoll::start(&store, limits).unwrap();
        expired.budget.started -= Duration::from_secs(2);
        assert!(
            expired.finish().unwrap(),
            "finish must check time after the final read callback"
        );
    }

    #[test]
    fn capture_limits_stop_growth_and_deadline_without_changing_the_map() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let limits = CaptureLimits {
            duration: Duration::from_secs(1),
            growth_bytes: 1 << 20,
            headroom_bytes: 1 << 20,
        };
        let budget = CaptureBudget::start(&store, limits).unwrap();
        assert_eq!(budget.exceeded_at(&store, budget.started).unwrap(), None);
        assert_eq!(
            budget.file_ceiling(&store) as u64,
            budget.initial_bytes + limits.growth_bytes
        );
        let capped = CaptureBudget::start(
            &store,
            CaptureLimits {
                growth_bytes: u64::MAX,
                ..limits
            },
        )
        .unwrap();
        assert_eq!(capped.file_ceiling(&store), (8 << 20) - (1 << 20));
        assert_eq!(
            budget
                .exceeded_at(&store, budget.started + limits.duration)
                .unwrap(),
            Some("capture lifetime")
        );
        let reader = store.tx_begin_read().unwrap();
        let mut stopped = false;
        for n in 0..32 {
            let mut writer = store.tx_begin_write().unwrap();
            writer
                .put_meta("capture-growth", &vec![n; 128 << 10])
                .unwrap();
            writer.commit().unwrap();
            if budget.exceeded_at(&store, budget.started).unwrap() == Some("capture file growth") {
                stopped = true;
                break;
            }
        }
        assert!(stopped);
        assert_eq!(store.env().info().map_size, 8 << 20);
        drop(reader);
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta("capture-growth", &[99]).unwrap();
        writer.commit().unwrap();
        let budget = CaptureBudget::start(
            &store,
            CaptureLimits {
                headroom_bytes: 8 << 20,
                ..limits
            },
        )
        .unwrap();
        assert_eq!(
            budget.exceeded_at(&store, budget.started).unwrap(),
            Some("capture map headroom")
        );
        let poll = CapturePoll::start(
            &store,
            CaptureLimits {
                headroom_bytes: 8 << 20,
                ..limits
            },
        )
        .unwrap();
        assert!(poll.cancelled());
        assert!(poll.cancelled(), "budget cancellation must remain latched");
        assert!(poll.finish().unwrap());
        let poll = CapturePoll::start(&store, limits).unwrap();
        assert!(!poll.cancelled());
        assert!(!poll.finish().unwrap());
    }
}
