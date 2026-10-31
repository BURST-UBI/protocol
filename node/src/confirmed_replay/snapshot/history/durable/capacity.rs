use super::*;
use crate::shutdown::ShutdownController;

pub(super) trait CapacityOwner: Send + Sized + 'static {
    fn parts(&self) -> (&RecoveryInputs, &ScratchLimit);
}
impl CapacityOwner for DurableHistorySession {
    fn parts(&self) -> (&RecoveryInputs, &ScratchLimit) { (&self.inputs, &self._limit) }
}
impl CapacityOwner for DurableHistoryInputs {
    fn parts(&self) -> (&RecoveryInputs, &ScratchLimit) { (&self.inputs, &self._limit) }
}

fn inventory_to_reclaim(owner: &impl CapacityOwner) -> Result<Option<DurableInventoryInputs>, StoreError> {
    let (inputs, limit) = owner.parts();
    let raw = match inputs.select_stream_checked(inventory_key()) {
        Ok(raw) => raw,
        Err(InputWriteError::Capacity) => return Ok(None),
        Err(InputWriteError::Storage(error)) => return Err(error),
    };
    if !raw.stream_reset_pending()? && raw.progress()?.positions == 0 { return Ok(None); }
    Ok(Some(DurableInventoryInputs { raw, _limit: limit.clone() }))
}

async fn reclaim(mut inputs: DurableInventoryInputs, shutdown: &ShutdownController) -> Result<bool, StoreError> {
    loop {
        if shutdown.is_requested() { return Ok(false); }
        let (next, done) = tokio::task::spawn_blocking(move || {
            if !inputs.raw.stream_reset_pending()? { inputs.raw.begin_stream_reset()?; }
            #[cfg(test)]
            pressure_crash_probe("pressure-reset-start");
            let done = inputs.raw.reset_stream_page(128, MAX_WITNESS_BYTES + 64)?.complete;
            #[cfg(test)]
            pressure_crash_probe(if done { "pressure-reset-done" } else { "pressure-reset-page" });
            Ok::<_, StoreError>((inputs, done))
        }).await.map_err(invalid)??;
        inputs = next;
        if shutdown.is_requested() { return Ok(false); }
        if done { return Ok(true); }
    }
}

/// Retry the failed operation once after clearing the raw inventory in bounded
/// pages. History streams are retained. Shutdown is checked between disk jobs;
/// dropping this future does not interrupt a running blocking job.
pub(super) async fn retry_capacity<T, R, F>(mut owner: T, mut operation: F, shutdown: &ShutdownController)
    -> Result<(T, Option<R>), StoreError>
where T: CapacityOwner, R: Send + 'static,
    F: FnMut(&mut T) -> Result<R, InputWriteError> + Send + 'static,
{
    for attempt in 0..2 {
        if shutdown.is_requested() { return Ok((owner, None)); }
        let (next, next_operation, result) = tokio::task::spawn_blocking(move || {
            let result = operation(&mut owner);
            (owner, operation, result)
        }).await.map_err(invalid)?;
        owner = next;
        operation = next_operation;
        match result {
            Ok(value) => return Ok((owner, if shutdown.is_requested() { None } else { Some(value) })),
            Err(InputWriteError::Storage(error)) => return Err(error),
            Err(InputWriteError::Capacity) if attempt == 0 => {
                if shutdown.is_requested() { return Ok((owner, None)); }
                let (next, inventory) = tokio::task::spawn_blocking(move || {
                    let inventory = inventory_to_reclaim(&owner)?;
                    Ok::<_, StoreError>((owner, inventory))
                }).await.map_err(invalid)??;
                owner = next;
                let Some(inventory) = inventory else { return Err(InputWriteError::Capacity.into()); };
                if !reclaim(inventory, shutdown).await? { return Ok((owner, None)); }
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("capacity operation has one retry")
}

#[cfg(test)]
fn pressure_crash_probe(stage: &str) {
    if std::env::var("BURST_COMPACT_CRASH_KIND").as_deref() == Ok("pressure-inputs")
        && std::env::var("BURST_COMPACT_CRASH_STAGE").as_deref() == Ok(stage)
    {
        use std::io::Write;
        println!("BURST_COMPACT_PUBLICATION_READY");
        std::io::stdout().flush().unwrap();
        loop { std::thread::park(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, NetworkId};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    async fn session(directory: &Path) -> DurableHistorySession {
        let limit = ScratchLimit::acquire_reserved(directory, 4 << 20, DurableHistoryInputs::reservation(1 << 20).unwrap()).unwrap();
        let context = HistoryContext { network: NetworkId::Dev, genesis: BlockHash::new([1;32]),
            epoch: BlockHash::new([2;32]), state_hash: BlockHash::new([3;32]) };
        DurableHistorySession::open(directory.to_path_buf(), context, 1 << 20, limit,
            &ShutdownController::new()).await.unwrap().unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn retry_is_once_and_storage_errors_never_trigger_eviction() {
        for corruption in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let session = session(directory.path()).await;
            let inventory = session.select_inventory().unwrap();
            inventory.raw.append(0, b"inventory").unwrap();
            let history = inventory.raw.select_stream([9;32]).unwrap();
            history.append(0, b"preserve").unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let result = retry_capacity(session, move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(if corruption { InputWriteError::Storage(invalid("bad input")) } else { InputWriteError::Capacity })
            }, &ShutdownController::new()).await;
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), if corruption { 1 } else { 2 });
            assert_eq!(history.read_position(0).unwrap().unwrap(), b"preserve");
            if corruption { assert_eq!(inventory.raw.read_position(0).unwrap().unwrap(), b"inventory"); }
            else { assert!(inventory.raw.progress().is_err()); }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn cancelled_pending_inventory_cleanup_resumes_without_touching_history() {
        let directory = tempfile::tempdir().unwrap();
        let session = session(directory.path()).await;
        let inventory = session.select_inventory().unwrap();
        for n in 0..140u64 { inventory.raw.append(n, &n.to_be_bytes()).unwrap(); }
        let history = inventory.raw.select_stream([9;32]).unwrap();
        history.append(0, b"preserve").unwrap();
        inventory.raw.begin_stream_reset().unwrap();
        assert!(!inventory.raw.reset_stream_page(1, MAX_WITNESS_BYTES + 64).unwrap().complete);
        let shutdown = ShutdownController::new();
        shutdown.shutdown();
        assert!(!reclaim(inventory, &shutdown).await.unwrap());
        let inventory = session.select_inventory().unwrap();
        assert!(inventory.raw.stream_reset_pending().unwrap());
        assert_eq!(history.read_position(0).unwrap().unwrap(), b"preserve");
        assert!(reclaim(inventory, &ShutdownController::new()).await.unwrap());
        assert_eq!(session.select_inventory().unwrap().raw.progress().unwrap().positions, 0);
        assert_eq!(history.read_position(0).unwrap().unwrap(), b"preserve");
    }
}
