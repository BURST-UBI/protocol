//! One slot follows a queued block through authentication and dependency recovery.
use std::sync::Arc;
use burst_ledger::StateBlock;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const MAX_RETAINED_BLOCKS: usize = 65_536;
pub(crate) const INGRESS_RESERVE: usize = 4096;
pub(crate) type Slot = Arc<OwnedSemaphorePermit>;

pub(crate) struct BlockRetention(Arc<Semaphore>);

impl BlockRetention {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self(Arc::new(Semaphore::new(capacity))))
    }

    pub(crate) fn acquire(&self) -> Option<Slot> {
        self.0.clone().try_acquire_owned().ok().map(Arc::new)
    }
}

pub(crate) struct RetainedBlock {
    pub(crate) block: StateBlock,
    pub(crate) slot: Option<Slot>,
}

impl std::ops::Deref for RetainedBlock {
    type Target = StateBlock;
    fn deref(&self) -> &StateBlock { &self.block }
}
