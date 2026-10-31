use super::super::tail::cemented::{AppliedTail, CementedTail};
use super::*;
use burst_types::BlockHash;
use std::sync::{Arc, Mutex, Weak};

struct Entry {
    store: Weak<LmdbStore>,
    network: NetworkId,
    initial: BlockHash,
    epoch: BlockHash,
    projection: Projection,
    replay: Option<Arc<ReplayCache>>,
}

impl Entry {
    fn matches(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        epoch: BlockHash,
    ) -> bool {
        self.store
            .upgrade()
            .is_some_and(|old| Arc::ptr_eq(&old, store))
            && self.network == network
            && self.initial == initial
            && self.epoch == epoch
    }
}

#[derive(Clone)]
pub(super) struct Projection {
    pub cemented: Arc<CementedTail>,
    pub applied: Arc<AppliedTail>,
    pub inventory: Arc<super::retained::RetainedInventory>,
    pub economics: Arc<super::economics::AppliedEconomics>,
}

#[cfg_attr(test, derive(Clone))]
pub(crate) struct CommittedProjection {
    pub(super) network: NetworkId,
    pub(super) initial: BlockHash,
    pub(super) epoch: BlockHash,
    pub(super) projection: Projection,
    pub(super) replay: Arc<ReplayCache>,
}

impl CommittedProjection {
    pub(crate) fn publish(
        self,
        cache: &ProjectionCache,
        store: &Arc<LmdbStore>,
    ) -> Result<(), StoreError> {
        cache.publish_inner(
            store,
            self.network,
            self.initial,
            self.epoch,
            self.projection,
            Some(self.replay),
        )
    }
}

/// One private derived projection shared by recovery consumers. It is never
/// signing authority; each consumer still authenticates history and journals.
#[derive(Default)]
pub(crate) struct ProjectionCache(
    Mutex<Option<Entry>>,
    #[cfg(test)] std::sync::atomic::AtomicUsize,
);

impl ProjectionCache {
    /// Reuse only selection rows for download hints; no replay or authority is handed out.
    pub(crate) fn selection_refresh(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        previous: Option<BlockHash>,
    ) -> Result<Option<(BlockHash, super::SelectionRefresh)>, StoreError> {
        let replay = {
            // Hints are optional. Full projection validation may hold this lock;
            // leave the current cursor intact and retry on a later scheduling turn.
            let cached = match self.0.try_lock() {
                Ok(cached) => cached,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
                Err(error) => return Err(invalid(error)),
            };
            cached.as_ref()
                .filter(|entry| entry.matches(store, network, initial, entry.epoch)
                    && Some(entry.epoch) != previous)
                .and_then(|entry| entry.replay.as_ref().map(|replay| (entry.epoch, replay.clone())))
        };
        replay.map(|(epoch, replay)| Ok((epoch, replay.recovered.selections.refresh_cursor()?))).transpose()
    }

    #[cfg(test)]
    pub(crate) fn replay_handoffs(&self) -> usize {
        self.1.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// A replay starting point only. The caller must run ReplayCache::recover
    /// to recheck durable records before using it for another decision.
    pub(crate) fn replay(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        base: Option<BaseCommitment>,
        epoch: BlockHash,
    ) -> Result<Option<ReplayCache>, StoreError> {
        let replay = {
            let cached = self.0.lock().map_err(invalid)?;
            cached
                .as_ref()
                .filter(|entry| entry.matches(store, network, initial, epoch))
                .and_then(|entry| entry.replay.clone())
                .filter(|replay| replay.base == base)
        };
        let replay = replay
            .map(|replay| replay.fork(store.env().path()))
            .transpose()?;
        #[cfg(test)]
        if replay.is_some() {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(replay)
    }

    pub(super) fn get_view(
        &self, store: &Arc<LmdbStore>, network: NetworkId, initial: BlockHash, epoch: BlockHash,
        continuation: &super::super::SnapshotContinuation,
        view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<Option<Projection>, StoreError> {
        self.get_checked(store, network, initial, epoch, false, |projection| {
            Ok(projection.cemented.verify_view(view).is_ok()
                && projection.applied.verify_view(view).is_ok()
                && projection.inventory.verify_view(view).is_ok()
                && projection.economics.verify_view(view).is_ok()
                && super::order::read_view(view, &projection.applied, continuation)? == projection.economics.ordering)
        })
    }

    fn get_checked(
        &self, store: &Arc<LmdbStore>, network: NetworkId, initial: BlockHash, epoch: BlockHash,
        invalidate: bool, check: impl FnOnce(&Projection) -> Result<bool, StoreError>,
    ) -> Result<Option<Projection>, StoreError> {
        let mut cached = self.0.lock().map_err(invalid)?;
        let Some(entry) = cached.as_ref() else {
            return Ok(None);
        };
        if !entry.matches(store, network, initial, epoch) {
            return Ok(None);
        }
        let projection = &entry.projection;
        // A miss still requires full replay and never authorizes corrupt state.
        if !check(projection)? {
            if invalidate { *cached = None; }
            return Ok(None);
        }
        Ok(Some(projection.clone()))
    }

    pub(super) fn publish(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        epoch: BlockHash,
        projection: Projection,
    ) -> Result<(), StoreError> {
        self.publish_inner(store, network, initial, epoch, projection, None)
    }

    fn publish_inner(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        epoch: BlockHash,
        projection: Projection,
        replay: Option<Arc<ReplayCache>>,
    ) -> Result<(), StoreError> {
        let mut cached = self.0.lock().map_err(invalid)?;
        let replay = replay.or_else(|| {
            cached
                .as_ref()
                .filter(|entry| entry.matches(store, network, initial, epoch))
                .and_then(|entry| entry.replay.clone())
        });
        *cached = Some(Entry {
            store: Arc::downgrade(store),
            network,
            initial,
            epoch,
            projection,
            replay,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_hints_defer_while_projection_validation_holds_cache() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let cache = ProjectionCache::default();
        let guard = cache.0.lock().unwrap();
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let cache = &cache;
            let store = &store;
            let worker = scope.spawn(move || {
                send.send(cache.selection_refresh(
                    store, NetworkId::Dev, ProtocolParams::default().params_hash(), None,
                ).map(|hint| hint.is_none())).unwrap();
            });
            let result = receive.recv_timeout(std::time::Duration::from_secs(1));
            // Release before asserting, so a regression cannot strand the worker.
            drop(guard);
            worker.join().unwrap();
            assert!(result.expect("scheduling hint waited on projection validation").unwrap());
        });
    }
}
