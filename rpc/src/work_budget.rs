use crate::error::RpcError;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};

static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn check_difficulty(requested: u64, required: u64) -> Result<(), RpcError> {
    // Expected work is proportional to 1 / (2^64 - threshold), not threshold.
    let space = 1u128 << 64;
    let max = space - ((space - required as u128) / 64).max(1);
    if requested as u128 > max {
        return Err(RpcError::InvalidRequest(
            "work difficulty exceeds 64x protocol requirement".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn generate(
    generator: Arc<burst_work::WorkGenerator>,
    hash: burst_types::BlockHash,
    difficulty: u64,
) -> Result<burst_work::WorkNonce, RpcError> {
    let permit = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| RpcError::Server("work generation busy".into()))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(cancelled.clone());
    let deadline = Instant::now() + Duration::from_secs(15);
    tokio::task::spawn_blocking(move || {
        // The worker owns the slot until computation actually stops, even if its
        // caller is cancelled. A timeout cannot release capacity prematurely.
        let _permit = permit;
        generator.generate_until(&hash, difficulty, &cancelled, deadline)
    })
    .await
    .map_err(|e| RpcError::Server(format!("work generation task failed: {e}")))?
    .map_err(|e| RpcError::WorkError(e.to_string()))
}

// Authoring helpers share the two process-wide work slots and 15-second deadline.
// This blocking path does not receive HTTP cancellation; the deadline still applies.
pub(crate) fn generate_blocking(
    generator: &burst_work::WorkGenerator,
    hash: &burst_types::BlockHash,
    difficulty: u64,
) -> Result<burst_work::WorkNonce, RpcError> {
    let _permit = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| RpcError::Server("work generation busy".into()))?;
    generator
        .generate_until(
            hash,
            difficulty,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(15),
        )
        .map_err(|e| RpcError::WorkError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_extreme_requested_work_and_handles_governed_extremes() {
        assert!(check_difficulty(u64::MAX, 0).is_err());
        assert!(check_difficulty(1000, 1000).is_ok());
        assert!(check_difficulty(u64::MAX, u64::MAX).is_ok());
        let required = u64::MAX - 1023;
        assert!(check_difficulty(u64::MAX - 15, required).is_ok());
        assert!(check_difficulty(u64::MAX - 14, required).is_err());
    }

    #[tokio::test]
    async fn overload_rejects_and_aborted_call_releases_slot_after_worker_stops() {
        let slots = SLOTS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone();
        let held = slots.clone().acquire_many_owned(2).await.unwrap();
        assert!(generate_blocking(
            &burst_work::WorkGenerator,
            &burst_types::BlockHash::ZERO,
            0,
        )
        .unwrap_err()
        .to_string()
        .contains("busy"));
        assert!(generate(
            Arc::new(burst_work::WorkGenerator),
            burst_types::BlockHash::ZERO,
            0
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("busy"));
        drop(held);
        let task = tokio::spawn(generate(
            Arc::new(burst_work::WorkGenerator),
            burst_types::BlockHash::new([19; 32]),
            u64::MAX,
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while slots.available_permits() == 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), async {
            while slots.available_permits() != 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("aborted work must actually stop and release its slot");
    }
}
