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

    pub(super) fn get(
        &self,
        store: &Arc<LmdbStore>,
        network: NetworkId,
        initial: BlockHash,
        epoch: BlockHash,
        continuation: &super::super::SnapshotContinuation,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<Option<Projection>, StoreError> {
        let mut cached = self.0.lock().map_err(invalid)?;
        let Some(entry) = cached.as_ref() else {
            return Ok(None);
        };
        if !entry.matches(store, network, initial, epoch) {
            return Ok(None);
        }
        // A changed projection takes the full replay path. That path must still
        // reject corruption; a cache miss is never permission to publish it.
        let projection = &entry.projection;
        if projection.cemented.verify_unchanged(batch).is_err()
            || projection.applied.verify_unchanged(batch).is_err()
            || projection.inventory.verify_unchanged(batch).is_err()
            || projection.economics.verify_unchanged(batch).is_err()
            || super::order::read(batch, &projection.applied, continuation)?
                != projection.economics.ordering
        {
            *cached = None;
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
