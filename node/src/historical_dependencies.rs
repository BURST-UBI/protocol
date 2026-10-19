//! Recover missing dependency bodies from certified snapshot ancestry. These
//! rows were already executed; restoring them must not replay their economics.
use crate::{
    block_processor::BlockProcessor,
    checkpoint_sync::witness::{self, Roots},
    confirmed_replay::snapshot::{history::HistoricalBlock, installed},
    connection_registry::ConnectionRegistry,
    ledger_cache::LedgerCache,
    priority_queue::BlockPriorityQueue,
    shutdown::ShutdownController,
};
use burst_ledger::StateBlock;
use burst_store::{block::BlockStore, meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    processor: Arc<Mutex<BlockProcessor>>,
    queue: Arc<BlockPriorityQueue>,
    counts: Arc<LedgerCache>,
    shutdown: Arc<ShutdownController>,
) -> (mpsc::Sender<StateBlock>, tokio::task::JoinHandle<()>) {
    // One active request and one queued block. Unavailable requests are retried
    // by ordinary bootstrap, without retaining an unbounded dependency graph.
    let (sender, mut receiver) = mpsc::channel::<StateBlock>(1);
    let task = tokio::spawn(async move {
        let mut stop = shutdown.subscribe();
        loop {
            let block = tokio::select! {
                biased;
                _ = stop.recv() => break,
                block = receiver.recv() => match block { Some(block) => block, None => break },
            };
            let result = tokio::select! {
                biased;
                _ = stop.recv() => break,
                result = recover(store.clone(), network, params.clone(), registry.clone(), &block, processor.clone(), counts.clone()) => result,
            };
            match result {
                Ok(ready) => {
                    for block in ready {
                        queue.push_retained(block).await;
                    }
                }
                Err(error) => {
                    tracing::debug!(hash = %block.hash, %error, "historical dependency recovery deferred")
                }
            }
        }
    });
    (sender, task)
}

async fn recover(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    block: &StateBlock,
    processor: Arc<Mutex<BlockProcessor>>,
    counts: Arc<LedgerCache>,
) -> Result<Vec<crate::block_retention::RetainedBlock>, StoreError> {
    let mut wanted = Vec::new();
    for hash in block.dependencies() {
        if !store.block_store().exists(&hash)? {
            wanted.push(hash);
        }
    }
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let source = store.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let epoch = source.meta_store().payment_epoch()?;
        let txn = source.tx_begin_read()?;
        let base = installed::read_base(
            |key| source.meta_store().get_meta_txn(&txn, key),
            network,
            &params,
            source.env().path(),
        )?;
        Ok::<_, StoreError>((epoch, base))
    })
    .await
    .map_err(invalid)??;
    let (epoch, Some((snapshot, commitment))) = prepared else {
        return Ok(Vec::new());
    };
    let roots = snapshot
        .confirmed_heads()
        .iter()
        .map(|head| {
            snapshot
                .history_cursor(&head.account)
                .map_err(invalid)?
                .ok_or_else(|| invalid("snapshot dependency head missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let committed = snapshot
        .committed_history_cursors(&wanted)
        .map_err(invalid)?;
    let proofs =
        witness::request_with_committed(&registry, Roots::Plan(roots), wanted.clone(), committed)
            .await?;
    let context = crate::confirmed_replay::snapshot::history::HistoryContext {
        network,
        genesis: snapshot.anchor().genesis_hash(),
        epoch: snapshot.anchor().hash(),
        state_hash: snapshot.certificate().manifest.state_hash,
    };
    // Serialize index/cache publication with normal commits and checkpoint
    // replacement. The owned guard remains held even if shutdown cancels the wait.
    let mut processor = processor.lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let mut batch = store.tx_begin_write()?;
        if batch.payment_epoch()? != epoch {
            return Err(invalid("dependency authority changed"));
        }
        commitment.verify(|key| batch.get_meta(key))?;
        let inserted = install(&mut batch, context, &wanted, &proofs)?;
        batch.commit()?;
        for _ in 0..inserted {
            counts.inc_block_count();
        }
        let mut ready = Vec::new();
        for hash in wanted {
            ready.extend(processor.release_retained(&hash));
        }
        Ok(ready)
    })
    .await
    .map_err(invalid)?
}

pub(crate) fn install(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    context: crate::confirmed_replay::snapshot::history::HistoryContext,
    wanted: &[BlockHash],
    proofs: &[HistoricalBlock],
) -> Result<usize, StoreError> {
    let mut inserted = 0;
    for hash in wanted {
        let proof = proofs
            .iter()
            .find(|proof| proof.block().hash == *hash)
            .ok_or_else(|| invalid("dependency witness missing"))?;
        let block = proof.block();
        let account = batch.get_account(&block.account)?;
        if proof.context() != context
            || proof.height() == 0
            || proof.height() > account.confirmation_height
        {
            return Err(invalid("dependency is outside the certified prefix"));
        }
        if batch
            .block_at_height(&block.account, proof.height())?
            .is_some_and(|existing| existing != *hash)
        {
            return Err(invalid("dependency conflicts with retained history"));
        }
        match batch.get_block(hash) {
            Ok(bytes) => {
                let existing: StateBlock = bincode::deserialize(&bytes).map_err(invalid)?;
                if existing.compute_hash() != *hash
                    || existing.hash != *hash
                    || batch.block_height(&block.account, hash)? != proof.height()
                {
                    return Err(invalid("stored dependency index mismatch"));
                }
            }
            Err(StoreError::NotFound(_)) => {
                batch.put_block_with_account(
                    hash,
                    &bincode::serialize(block).map_err(invalid)?,
                    &block.account,
                    proof.height(),
                )?;
                inserted += 1;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(inserted)
}
