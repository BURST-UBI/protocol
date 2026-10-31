//! Shared count permits carried through authentication and dependency recovery.
//! Clones share one permit until the last owner drops it. This bounds retained
//! slots, not payload bytes; callers must acquire and preserve the permit.
use burst_ledger::StateBlock;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const MAX_RETAINED_BLOCKS: usize = 65_536;
pub const INGRESS_RESERVE: usize = 4096;
pub type Slot = Arc<OwnedSemaphorePermit>;

pub struct BlockRetention(Arc<Semaphore>);

impl BlockRetention {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self(Arc::new(Semaphore::new(capacity))))
    }

    /// Try once without waiting; return None when all permits are held.
    pub fn acquire(&self) -> Option<Slot> {
        self.0.clone().try_acquire_owned().ok().map(Arc::new)
    }
}

pub struct RetainedBlock {
    pub block: StateBlock,
    /// Absent before admission or when the caller uses an unbudgeted queue.
    pub slot: Option<Slot>,
}

impl std::ops::Deref for RetainedBlock {
    type Target = StateBlock;
    fn deref(&self) -> &StateBlock {
        &self.block
    }
}
