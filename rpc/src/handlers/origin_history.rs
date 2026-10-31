//! Origin block lookup with per-reader row, serialized-byte and elapsed-time limits.
use crate::{error::RpcError, server::BlockProcessorCallback, RpcState};
use burst_ledger::StateBlock;
use burst_store::{block::BlockStore, StoreError};
use burst_types::BlockHash;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const MAX_ROWS: usize = 8192;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const LOOKUP_TIME: Duration = Duration::from_secs(120);

pub(super) struct Entry {
    pub block: StateBlock,
    /// Retrieved through the node's historical callback rather than the local store.
    pub historical: bool,
}

pub(super) struct Reader {
    store: Arc<dyn BlockStore + Send + Sync>,
    processor: Arc<dyn BlockProcessorCallback>,
    runtime: tokio::runtime::Handle,
    started: Instant,
    bytes: u64,
    entries: HashMap<BlockHash, Entry>,
}

impl Reader {
    pub fn new(state: &RpcState) -> Self {
        Self {
            store: state.block_store.clone(),
            processor: state.block_processor.clone(),
            runtime: tokio::runtime::Handle::current(),
            started: Instant::now(),
            bytes: 0,
            entries: HashMap::new(),
        }
    }

    fn remaining(&self) -> Result<Duration, RpcError> {
        LOOKUP_TIME
            .checked_sub(self.started.elapsed())
            .ok_or_else(|| RpcError::Server("origin history lookup timed out".into()))
    }

    fn insert(
        &mut self,
        hash: BlockHash,
        block: StateBlock,
        historical: bool,
    ) -> Result<(), RpcError> {
        self.remaining()?;
        let bytes =
            bincode::serialized_size(&block).map_err(|e| RpcError::Server(e.to_string()))?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= MAX_BYTES)
            .ok_or_else(|| RpcError::Server("origin history byte limit exceeded".into()))?;
        if block.hash != hash || block.compute_hash() != hash {
            return Err(RpcError::Server(
                "origin history block hash is inconsistent".into(),
            ));
        }
        self.entries.insert(hash, Entry { block, historical });
        Ok(())
    }

    /// Run on a blocking worker: missing bodies await the historical callback.
    /// Cache at most 8,192 blocks and 64 MiB of serialized block bytes. The byte
    /// budget excludes decoded allocations and the callback's returned vector.
    /// Check the 120-second reader deadline between lookups and around the fetch;
    /// synchronous reads/decodes are not interrupted. Local corruption is an error.
    /// Hashes are checked here; historical authentication belongs to the callback.
    pub fn prefetch(&mut self, hashes: Vec<BlockHash>) -> Result<(), RpcError> {
        self.remaining()?;
        if hashes.len() > MAX_ROWS {
            return Err(RpcError::Server("origin history row limit exceeded".into()));
        }
        let wanted: HashSet<_> = hashes
            .into_iter()
            .filter(|hash| !self.entries.contains_key(hash))
            .collect();
        if self.entries.len() + wanted.len() > MAX_ROWS {
            return Err(RpcError::Server("origin history row limit exceeded".into()));
        }
        let mut missing = HashSet::new();
        for hash in wanted {
            self.remaining()?;
            match self.store.get_block(&hash) {
                Ok(bytes) => {
                    if bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
                        return Err(RpcError::Server(
                            "origin history body exceeds wire limit".into(),
                        ));
                    }
                    self.insert(hash, super::deserialize_block(&bytes)?, false)?;
                }
                Err(StoreError::NotFound(_)) => {
                    missing.insert(hash);
                }
                Err(error) => return Err(error.into()),
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let remaining = self.remaining()?;
        let proofs = self.runtime.block_on(async {
            tokio::time::timeout(
                remaining,
                self.processor
                    .historical_blocks(missing.iter().copied().collect()),
            )
            .await
            .map_err(|_| RpcError::Server("origin history lookup timed out".into()))?
            .map_err(RpcError::Node)
        })?;
        for block in proofs {
            let hash = block.hash;
            if !missing.remove(&hash) {
                return Err(RpcError::Server(
                    "unexpected or duplicate historical origin body".into(),
                ));
            }
            self.insert(hash, block, true)?;
        }
        if !missing.is_empty() {
            return Err(RpcError::Node(
                "origin history is unavailable; no complete result".into(),
            ));
        }
        Ok(())
    }

    pub fn get(&mut self, hash: BlockHash) -> Result<&Entry, RpcError> {
        self.prefetch(vec![hash])?;
        self.entries
            .get(&hash)
            .ok_or_else(|| RpcError::Node("origin history is unavailable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
    use std::sync::Mutex;

    struct Store(Result<Vec<u8>, StoreError>);
    impl BlockStore for Store {
        fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
            match &self.0 {
                Ok(bytes) => Ok(bytes.clone()),
                Err(StoreError::NotFound(_)) => Err(StoreError::NotFound(hash.to_string())),
                Err(_) => Err(StoreError::Corruption("local corruption".into())),
            }
        }
        fn put_block(&self, _: &BlockHash, _: &[u8]) -> Result<(), StoreError> {
            panic!("read only")
        }
        fn exists(&self, _: &BlockHash) -> Result<bool, StoreError> {
            unreachable!()
        }
        fn delete_block(&self, _: &BlockHash) -> Result<(), StoreError> {
            panic!("read only")
        }
        fn get_account_blocks(&self, _: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> {
            panic!("no chain scan")
        }
        fn block_count(&self) -> Result<u64, StoreError> {
            unreachable!()
        }
        fn block_at_height(
            &self,
            _: &WalletAddress,
            _: u64,
        ) -> Result<Option<BlockHash>, StoreError> {
            unreachable!()
        }
        fn height_of_block(&self, _: &BlockHash) -> Result<Option<u64>, StoreError> {
            unreachable!()
        }
    }
    struct Processor {
        blocks: Vec<StateBlock>,
        calls: Mutex<Vec<Vec<BlockHash>>>,
    }
    impl BlockProcessorCallback for Processor {
        fn process_block(&self, _: &[u8]) -> Result<crate::server::ProcessResult, String> {
            panic!("no admission")
        }
        fn historical_blocks(
            &self,
            hashes: Vec<BlockHash>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<StateBlock>, String>> + Send + '_>,
        > {
            self.calls.lock().unwrap().push(hashes);
            Box::pin(async { Ok(self.blocks.clone()) })
        }
    }
    fn block(n: u64) -> StateBlock {
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
        let mut block = StateBlock {
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            signing_grant: BlockHash::ZERO,
            payment_epoch: BlockHash::ZERO,
            block_type: burst_ledger::BlockType::Burn,
            account: account.clone(),
            representative: account,
            previous: BlockHash::ZERO,
            brn_spent: 1,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: vec![],
            timestamp: Timestamp::new(n),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 32 * 2]),
            hash: BlockHash::ZERO,
            delegation_payload: vec![],
        };
        block.hash = block.compute_hash();
        block
    }
    fn reader(store: Store, blocks: Vec<StateBlock>) -> (Reader, Arc<Processor>) {
        let processor = Arc::new(Processor {
            blocks,
            calls: Mutex::new(vec![]),
        });
        (
            Reader {
                store: Arc::new(store),
                processor: processor.clone(),
                runtime: tokio::runtime::Handle::current(),
                started: Instant::now(),
                bytes: 0,
                entries: HashMap::new(),
            },
            processor,
        )
    }

    #[tokio::test]
    async fn origin_history_batches_missing_bodies_and_preserves_local_errors() {
        let a = block(1);
        let b = block(2);
        let (mut read, processor) = reader(
            Store(Err(StoreError::NotFound("missing".into()))),
            vec![a.clone(), b.clone()],
        );
        tokio::task::spawn_blocking(move || {
            read.prefetch(vec![a.hash, b.hash, a.hash]).unwrap();
            assert!(read.get(a.hash).unwrap().historical);
            assert_eq!(read.get(b.hash).unwrap().block.hash, b.hash);
            assert_eq!(processor.calls.lock().unwrap().len(), 1);
            assert_eq!(processor.calls.lock().unwrap()[0].len(), 2);
        })
        .await
        .unwrap();
        let a = block(1);
        let (mut read, processor) = reader(Store(Ok(bincode::serialize(&a).unwrap())), vec![]);
        assert!(!read.get(a.hash).unwrap().historical);
        assert!(processor.calls.lock().unwrap().is_empty());
        let (mut read, processor) = reader(
            Store(Err(StoreError::Corruption("bad".into()))),
            vec![a.clone()],
        );
        assert!(read.get(a.hash).is_err());
        assert!(processor.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn origin_history_rejects_incomplete_mismatched_and_over_budget_proofs() {
        for proofs in [vec![], vec![block(2)], vec![block(1), block(1)]] {
            let (mut read, _) = reader(Store(Err(StoreError::NotFound("missing".into()))), proofs);
            tokio::task::spawn_blocking(move || assert!(read.get(block(1).hash).is_err()))
                .await
                .unwrap();
        }
        let mut corrupt = block(1);
        corrupt.brn_spent += 1;
        let (mut read, _) = reader(Store(Ok(bincode::serialize(&corrupt).unwrap())), vec![]);
        assert!(read.get(corrupt.hash).is_err());
        let (mut read, processor) =
            reader(Store(Err(StoreError::NotFound("missing".into()))), vec![]);
        assert!(read.prefetch(vec![BlockHash::ZERO; MAX_ROWS + 1]).is_err());
        read.started = Instant::now() - LOOKUP_TIME;
        assert!(read.get(block(1).hash).is_err());
        assert!(processor.calls.lock().unwrap().is_empty());
        let (mut read, _) = reader(Store(Ok(bincode::serialize(&block(1)).unwrap())), vec![]);
        read.bytes = MAX_BYTES;
        assert!(read.get(block(1).hash).is_err());
    }
}
