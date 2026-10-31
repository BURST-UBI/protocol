use super::*;
use crate::confirmed_replay::snapshot::installed::{CandidateDiscovery, RecoveredSnapshot};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_store_lmdb::{meta::VoteDeferral, LmdbStore};
use burst_types::WalletAddress;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

struct Preparation {
    representative: WalletAddress,
    budget: Duration,
    cancelled: AtomicBool,
    guard: Mutex<Option<VoteDeferral>>,
}

impl Preparation {
    fn defer(&self, store: &LmdbStore, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(StoreError::Backend(
                "checkpoint preparation cancelled".into(),
            ));
        }
        if anchor.roster().weight(&self.representative) == 0 {
            return Err(StoreError::Corruption(
                "checkpoint preparation signer is not in the roster".into(),
            ));
        }
        let mut guard = self.guard.lock().expect("preparation guard lock");
        if guard.is_none() {
            let acquired = store.meta_store().defer_new_soft_votes(
                &anchor.hash(),
                &self.representative,
                Instant::now() + self.budget,
            )?;
            if self.cancelled.load(Ordering::Acquire) {
                return Err(StoreError::Backend(
                    "checkpoint preparation cancelled".into(),
                ));
            }
            *guard = Some(acquired);
        }
        Ok(())
    }

    fn deadline(&self) -> Option<Instant> {
        self.guard
            .lock()
            .expect("preparation guard lock")
            .as_ref()
            .map(VoteDeferral::deadline)
    }
}

struct Owner(Arc<Preparation>);

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Release);
        if let Ok(mut guard) = self.0.guard.try_lock() {
            guard.take();
        }
    }
}

/// Replay first to establish a locally authenticated pending event. Only then
/// defer new local soft votes, recapture an advanced cut, and retain the same lease
/// through startup. Every attempt still checks all confirmation/signing guards.
pub(super) async fn discover(
    base: Arc<RecoveredSnapshot>,
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    clock: SigningClock,
    representative: WalletAddress,
    budget: Duration,
) -> Result<
    (
        Option<crate::checkpoint_agreement::CompactReplayCandidate>,
        Option<VoteDeferral>,
    ),
    StoreError,
> {
    if budget.is_zero() || budget > Duration::from_secs(60) {
        return Err(StoreError::Corruption(
            "invalid checkpoint preparation budget".into(),
        ));
    }
    let preparation = Arc::new(Preparation {
        representative,
        budget,
        cancelled: AtomicBool::new(false),
        guard: Mutex::new(None),
    });
    let _owner = Owner(preparation.clone());
    loop {
        let owner = preparation.clone();
        let attempt = async {
            let result = base
                .clone()
                .discover_cemented_candidate_attempt(
                    store.clone(),
                    registry.clone(),
                    clock,
                    move |store, anchor| owner.defer(store, anchor),
                )
                .await?;
            let CandidateDiscovery::Ready(Some(candidate)) = result else {
                return Ok(result);
            };
            let source = store.clone();
            let signer = preparation.representative.clone();
            let (candidate, covered) = tokio::task::spawn_blocking(move || {
                let batch = source.tx_begin_write()?;
                let covered = candidate.input().covers_local_promises(&batch, &signer)?;
                Ok::<_, StoreError>((candidate, covered))
            })
            .await
            .map_err(|error| StoreError::Corruption(error.to_string()))??;
            if covered {
                return Ok(CandidateDiscovery::Ready(Some(candidate)));
            }
            drop(candidate);
            let deadline = preparation.deadline().ok_or_else(|| {
                StoreError::Backend("promised replay requires an active preparation lease".into())
            })?;
            tokio::time::timeout_at(
                deadline.into(),
                base.clone().discover_promised_candidate_from_peers(
                    store.clone(),
                    registry.clone(),
                    clock,
                    preparation.representative.clone(),
                ),
            )
            .await
            .map_err(|_| StoreError::Backend("checkpoint preparation budget elapsed".into()))?
        };
        let result = match preparation.deadline() {
            Some(deadline) => tokio::time::timeout_at(deadline.into(), attempt)
                .await
                .map_err(|_| StoreError::Backend("checkpoint preparation budget elapsed".into()))?,
            None => attempt.await,
        }?;
        match result {
            CandidateDiscovery::Ready(candidate) => {
                let guard = preparation
                    .guard
                    .lock()
                    .expect("preparation guard lock")
                    .take();
                if candidate.is_none() {
                    return Ok((None, None));
                }
                return Ok((candidate, guard));
            }
            CandidateDiscovery::Advanced => {
                if preparation
                    .deadline()
                    .is_none_or(|deadline| Instant::now() >= deadline)
                {
                    // A changing payment cut does not invalidate recovered authority.
                    // StaleRead is reserved here for changed authority records;
                    // the height worker must retry discovery without recovering them.
                    return Err(StoreError::Backend(
                        "confirmed cut advanced; retry compact discovery".into(),
                    ));
                }
                tokio::task::yield_now().await;
            }
        }
    }
}
