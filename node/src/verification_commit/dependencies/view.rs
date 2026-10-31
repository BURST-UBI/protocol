//! The same dependency checks run against admission writers and vote read snapshots.
use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::{block_undo::BlockUndo, LmdbReadTransaction, LmdbStore, LmdbWriteTransaction};
use burst_types::{BlockHash, WalletAddress};

pub(crate) trait DependencyView {
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError>;
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn get_pending(&self, account: &WalletAddress, source: &burst_types::TxHash) -> Result<burst_store::pending::PendingInfo, StoreError>;
    fn payment_epoch(&self) -> Result<BlockHash, StoreError> {
        burst_store::meta::read_open_payment_epoch(|key| self.get_meta(key))
    }
    fn block_height(&self, account: &WalletAddress, hash: &BlockHash) -> Result<u64, StoreError>;
    fn block_undo(&self, hash: &BlockHash) -> Result<Option<BlockUndo>, StoreError>;
}

impl DependencyView for LmdbWriteTransaction<'_> {
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> { self.get_block(hash) }
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> { self.get_meta(key) }
    fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError> { self.get_account(account) }
    fn get_pending(&self, account: &WalletAddress, source: &burst_types::TxHash) -> Result<burst_store::pending::PendingInfo, StoreError> {
        self.get_pending(account, source)
    }
    fn block_height(&self, account: &WalletAddress, hash: &BlockHash) -> Result<u64, StoreError> { self.block_height(account, hash) }
    fn block_undo(&self, hash: &BlockHash) -> Result<Option<BlockUndo>, StoreError> { self.block_undo(hash) }
}

impl DependencyView for burst_store_lmdb::read_view::LedgerReadView<'_> {
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> { self.get_block(hash) }
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> { self.get_meta(key) }
    fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError> { self.get_account(account) }
    fn get_pending(&self, account: &WalletAddress, source: &burst_types::TxHash) -> Result<burst_store::pending::PendingInfo, StoreError> {
        self.get_pending(account, source)
    }
    fn block_height(&self, account: &WalletAddress, hash: &BlockHash) -> Result<u64, StoreError> { self.block_height(account, hash) }
    fn block_undo(&self, hash: &BlockHash) -> Result<Option<BlockUndo>, StoreError> { self.block_undo(hash) }
}

pub(crate) struct Snapshot<'a> {
    store: &'a LmdbStore,
    txn: LmdbReadTransaction<'a>,
}
impl<'a> Snapshot<'a> {
    pub(crate) fn view(&self) -> &LmdbReadTransaction<'a> { &self.txn }

    pub(crate) fn new(store: &'a LmdbStore) -> Result<Self, StoreError> {
        Ok(Self { store, txn: store.tx_begin_read()? })
    }
}
impl DependencyView for Snapshot<'_> {
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.store.block_store().get_block_txn(&self.txn, hash)
    }
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.store.meta_store().get_meta_txn(&self.txn, key)
    }
    fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.store.account_store().get_account_txn(&self.txn, account)
    }
    fn get_pending(&self, account: &WalletAddress, source: &burst_types::TxHash) -> Result<burst_store::pending::PendingInfo, StoreError> {
        self.store.pending_store().get_pending_txn(&self.txn, account, source)
    }
    fn block_height(&self, account: &WalletAddress, hash: &BlockHash) -> Result<u64, StoreError> {
        let blocks = self.store.block_store();
        let height = blocks.height_of_block_txn(&self.txn, hash)?
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))?;
        if blocks.block_at_height_txn(&self.txn, account, height)? != Some(*hash) {
            return Err(StoreError::Corruption("block is not on the indexed account chain".into()));
        }
        Ok(height)
    }
    fn block_undo(&self, hash: &BlockHash) -> Result<Option<BlockUndo>, StoreError> {
        self.store.meta_store().block_undo_txn(&self.txn, hash)
    }
}
