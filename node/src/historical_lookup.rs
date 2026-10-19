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

pub(crate) struct Reader {
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
        let (cursor, _guard) = tokio::task::spawn_blocking(move || {
            let cursor = installed::load_base(&store, network, &params)
                .map_err(|error| error.to_string())?
                .map(|base| base.history_cursor(&account))
                .transpose()
                .map_err(|error| error.to_string())?
                .flatten();
            Ok::<_, String>((cursor, guard))
        })
        .await
        .map_err(|error| error.to_string())??;
        let Some(mut cursor) = cursor else {
            return Ok(Vec::new());
        };
        let mut page = Vec::with_capacity(count as usize);
        while let Some(head) = cursor.next() {
            if head.height < first {
                break;
            }
            // Walk authenticated chunks, retaining only the requested page.
            // This never installs recovered bodies or changes account state.
            let blocks = witness::fetch_suffix(&self.registry, &mut cursor, first - 1)
                .await
                .map_err(|error| error.to_string())?;
            for (index, block) in blocks.into_iter().enumerate() {
                let height = head.height - index as u64;
                if height <= last {
                    page.push((height, block));
                }
            }
        }
        page.reverse();
        Ok(page)
    }

    pub async fn read(&self, hash: BlockHash) -> Result<Option<StateBlock>, String> {
        let guard = self.acquire().await?;
        let store = self.store.clone();
        let network = self.network;
        let params = self.initial_params.clone();
        let (prepared, _guard) = tokio::task::spawn_blocking(move || {
            let base = installed::load_base(&store, network, &params)
                .map_err(|error| error.to_string())?;
            let Some(base) = base else {
                return Ok::<_, String>((None, guard));
            };
            let mut cursors = Vec::new();
            for head in base.confirmed_heads() {
                if let Some(cursor) = base
                    .history_cursor(&head.account)
                    .map_err(|error| error.to_string())?
                {
                    cursors.push(cursor);
                }
            }
            let committed = base
                .committed_history_cursors(&[hash])
                .map_err(|error| error.to_string())?;
            Ok((Some((cursors, committed)), guard))
        })
        .await
        .map_err(|error| error.to_string())??;
        let Some((cursors, committed)) = prepared else {
            return Ok(None);
        };
        // The peer's account hint selects a cursor, never its authentication root.
        // Membership does not authorize replay, index writes or balance changes.
        let proofs = witness::request_with_committed(
            &self.registry,
            Roots::Plan(cursors),
            vec![hash],
            committed,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(proofs
            .into_iter()
            .find(|proof| proof.block().hash == hash)
            .map(|proof| proof.into_block()))
    }
}
