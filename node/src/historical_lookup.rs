//! Read historical bodies against authenticated checkpoint cursors without
//! installing recovered blocks or changing ledger state.
use crate::{
    checkpoint_sync::witness::{self, Roots},
    confirmed_replay::snapshot::installed,
    connection_registry::ConnectionRegistry,
};
use burst_ledger::StateBlock;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams, WalletAddress};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, RwLock, Semaphore};

pub(crate) mod preparation;
use preparation::{Prepared, Request};

pub(crate) struct Reader {
    pub preparation: Arc<std::sync::Mutex<preparation::Cache>>,
    pub store: Arc<LmdbStore>,
    pub registry: Arc<RwLock<ConnectionRegistry>>,
    pub network: NetworkId,
    pub initial_params: ProtocolParams,
    pub active: Arc<Mutex<()>>,
    pub admitted: Arc<Semaphore>,
}

impl Reader {
    async fn acquire(&self) -> Result<(OwnedSemaphorePermit, OwnedMutexGuard<()>), String> {
        // One lookup runs at a time. Admission also bounds waiting requests, so
        // ordinary navigation can overlap without creating an unlimited queue.
        let permit = self
            .admitted
            .clone()
            .try_acquire_owned()
            .map_err(|_| "historical lookup is busy; retry later".to_string())?;
        let guard = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.active.clone().lock_owned(),
        )
        .await
        .map_err(|_| "historical lookup is busy; retry later".to_string())?;
        Ok((permit, guard))
    }

    /// Return available blocks in ascending height order. The authenticated walk
    /// may start above the requested page, and those ancestors count toward the
    /// work budget. The transfer deadline starts after blocking preparation.
    pub async fn account_page(
        &self,
        account: WalletAddress,
        first: u64,
        count: u32,
    ) -> Result<Vec<(u64, StateBlock)>, String> {
        if first == 0 || count == 0 || count > 1000 {
            return Err("invalid historical page range".into());
        }
        let last = first
            .checked_add(u64::from(count) - 1)
            .ok_or("historical page range overflow")?;
        let guard = self.acquire().await?;
        let store = self.store.clone();
        let network = self.network;
        let params = self.initial_params.clone();
        let preparation = self.preparation.clone();
        let (cursor, _guard) = tokio::task::spawn_blocking(move || {
            let prepared = preparation.lock().map_err(|error| error.to_string())?
                .prepare(&store, network, &params, Request::Account(account))?;
            let Prepared::Account(cursor) = prepared.as_ref() else { unreachable!("account preparation") };
            let cursor = cursor.clone();
            Ok::<_, String>((cursor, guard))
        })
        .await
        .map_err(|error| error.to_string())??;
        let Some(mut cursor) = cursor else {
            return Ok(Vec::new());
        };
        tokio::time::timeout(crate::checkpoint_sync::TRANSFER_TIMEOUT, async {
            let mut page = Vec::with_capacity(count as usize);
            let mut budget = PageWalkBudget::default();
            while let Some(head) = cursor.next() {
                if head.height < first {
                    break;
                }
                let remaining = budget.remaining_blocks();
                if remaining == 0 {
                    return Err("historical page walk limit exceeded".into());
                }
                let floor = (first - 1).max(head.height.saturating_sub(remaining as u64));
                // Charge the whole authenticated walk, including ancestors above
                // the requested page. Each fetch has its own smaller chunk limit.
                let blocks = witness::fetch_suffix(&self.registry, &mut cursor, floor)
                    .await
                    .map_err(|error| error.to_string())?;
                for (index, block) in blocks.into_iter().enumerate() {
                    budget.charge(
                        bincode::serialized_size(&block).map_err(|error| error.to_string())?,
                    )?;
                    let height = head.height - index as u64;
                    if height <= last {
                        page.push((height, block));
                    }
                }
            }
            page.reverse();
            Ok(page)
        })
        .await
        .map_err(|_| "historical page lookup timed out".to_string())?
    }

    pub async fn confirmed_slot(
        &self,
        query: burst_types::payment_recovery::SlotQuery,
    ) -> Result<burst_types::payment_recovery::SlotReview, String> {
        if let Some(review) = crate::payment_exclusion::indexed_slot(&self.store, query.clone())? {
            return Ok(review);
        }
        let account = WalletAddress::new(&query.account);
        // indexed_slot already validated the account and hash encoding.
        let previous = query.previous.parse::<BlockHash>()
            .map_err(|error| error.to_string())?;
        let successor = self.successor(account, previous).await?;
        Ok(burst_types::payment_recovery::SlotReview {
            account: query.account,
            previous: previous.to_string(),
            successor: successor.map(|hash| hash.to_string()),
        })
    }

    /// Prove a successor under one certified account head. No recovered body or
    /// index is installed, and missing peer history never proves exclusion.
    pub(crate) async fn successor(
        &self,
        account: WalletAddress,
        previous: BlockHash,
    ) -> Result<Option<BlockHash>, String> {
        let guard = self.acquire().await?;
        let store = self.store.clone();
        let network = self.network;
        let params = self.initial_params.clone();
        let preparation = self.preparation.clone();
        let (mut cursor, _guard) = tokio::task::spawn_blocking(move || {
            let prepared = preparation.lock().map_err(|error| error.to_string())?
                .prepare(&store, network, &params, Request::Account(account))?;
            let Prepared::Account(cursor) = prepared.as_ref() else { unreachable!("account preparation") };
            let cursor = cursor.clone().ok_or("confirmed slot account history is unavailable")?;
            Ok::<_, String>((cursor, guard))
        })
        .await
        .map_err(|error| error.to_string())??;
        tokio::time::timeout(crate::checkpoint_sync::TRANSFER_TIMEOUT, async {
            let mut walked = 0usize;
            let mut bytes = 0u64;
            while let Some(head) = cursor.next() {
                if head.hash == previous {
                    return Ok(None);
                }
                let remaining = witness::MAX_WALK_BLOCKS.saturating_sub(walked);
                if remaining == 0 {
                    return Err("confirmed slot history walk limit exceeded".into());
                }
                let floor = head.height.saturating_sub(remaining as u64);
                let blocks = witness::fetch_suffix(&self.registry, &mut cursor, floor)
                    .await
                    .map_err(|error| error.to_string())?;
                for block in blocks {
                    walked += 1;
                    bytes = bytes
                        .checked_add(
                            bincode::serialized_size(&block).map_err(|error| error.to_string())?,
                        )
                        .ok_or("confirmed slot history size overflow")?;
                    if bytes > witness::MAX_WALK_BYTES as u64 {
                        return Err("confirmed slot history byte limit exceeded".into());
                    }
                    if block.previous == previous {
                        return Ok(Some(block.hash));
                    }
                }
            }
            Ok(None)
        })
        .await
        .map_err(|_| "confirmed slot history lookup timed out".to_string())?
    }

    pub async fn read(&self, hash: BlockHash) -> Result<Option<StateBlock>, String> {
        Ok(self
            .read_many(vec![hash])
            .await?
            .into_iter()
            .find(|block| block.hash == hash))
    }

    /// Authenticate a batch against installed checkpoint history. A missing
    /// checkpoint returns an empty result; this is not evidence of nonexistence.
    pub async fn read_many(&self, hashes: Vec<BlockHash>) -> Result<Vec<StateBlock>, String> {
        use crate::confirmed_replay::snapshot::tail::MAX_TAIL_BLOCKS;
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        if hashes.len() > MAX_TAIL_BLOCKS
            || hashes.iter().any(BlockHash::is_zero)
            || hashes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != hashes.len()
        {
            return Err("invalid historical block batch".into());
        }
        let wanted = hashes.clone();
        let guard = self.acquire().await?;
        let store = self.store.clone();
        let network = self.network;
        let params = self.initial_params.clone();
        let preparation = self.preparation.clone();
        let (prepared, _guard) = tokio::task::spawn_blocking(move || {
            let prepared = preparation.lock().map_err(|error| error.to_string())?
                .prepare(&store, network, &params, Request::Batch(wanted))?;
            Ok::<_, String>((prepared, guard))
        })
        .await
        .map_err(|error| error.to_string())??;
        let Prepared::Batch(batch) = prepared.as_ref() else { unreachable!("batch preparation") };
        let Some((cursors, committed)) = batch else {
            return Ok(Vec::new());
        };
        // The peer's account hint selects a cursor, never its authentication root.
        // Membership does not authorize replay, index writes or balance changes.
        let proofs = witness::request_with_committed(
            &self.registry,
            Roots::Plan(cursors.clone()),
            hashes,
            committed.clone(),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(proofs.into_iter().map(|proof| proof.into_block()).collect())
    }
}

#[derive(Default)]
struct PageWalkBudget {
    blocks: usize,
    bytes: u64,
}

impl PageWalkBudget {
    fn remaining_blocks(&self) -> usize {
        witness::MAX_WALK_BLOCKS.saturating_sub(self.blocks)
    }

    fn charge(&mut self, bytes: u64) -> Result<(), String> {
        if self.remaining_blocks() == 0 {
            return Err("historical page walk limit exceeded".into());
        }
        self.bytes = self.bytes.checked_add(bytes)
            .filter(|total| *total <= witness::MAX_WALK_BYTES as u64)
            .ok_or("historical page byte limit exceeded")?;
        self.blocks += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_page_budget_enforces_cumulative_block_and_byte_limits() {
        let mut budget = PageWalkBudget::default();
        for _ in 0..witness::MAX_WALK_BLOCKS {
            budget.charge(1).unwrap();
        }
        assert_eq!(budget.remaining_blocks(), 0);
        assert!(budget.charge(1).unwrap_err().contains("walk limit"));

        let mut budget = PageWalkBudget::default();
        budget.charge(witness::MAX_WALK_BYTES as u64 - 1).unwrap();
        budget.charge(1).unwrap();
        assert!(budget.charge(1).unwrap_err().contains("byte limit"));
        assert!(budget.charge(u64::MAX).unwrap_err().contains("byte limit"));
    }
}
