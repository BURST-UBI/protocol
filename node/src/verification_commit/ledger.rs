use std::collections::HashMap;

use burst_ledger::StateBlock;
use burst_store::{account::AccountInfo, pending::PendingInfo, StoreError};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{BlockHash, TxHash, WalletAddress};

/// Writes belong to an uncommitted candidate. A failed transition must discard
/// both this candidate and its BRN/TRST/orchestrator state.
pub(crate) trait Ledger {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError>;
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError>;
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError>;
    fn get_pending(
        &self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError>;
    fn put_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
        pending: &PendingInfo,
    ) -> Result<(), StoreError>;
    fn take_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError>;
    fn all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError>;
}

impl Ledger for LmdbWriteTransaction<'_> {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        LmdbWriteTransaction::get_account(self, address)
    }

    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError> {
        self.put_account_info(account, false)
    }

    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        LmdbWriteTransaction::get_block(self, hash)
    }

    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        LmdbWriteTransaction::get_meta(self, key)
    }

    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        LmdbWriteTransaction::put_meta(self, key, bytes)
    }

    fn get_pending(
        &self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        LmdbWriteTransaction::get_pending(self, address, source)
    }

    fn put_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
        pending: &PendingInfo,
    ) -> Result<(), StoreError> {
        self.put_pending(
            address,
            source.as_bytes(),
            &bincode::serialize(pending)
                .map_err(|error| StoreError::Serialization(error.to_string()))?,
        )
    }

    fn all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        LmdbWriteTransaction::all_pending(self)
    }

    fn take_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        let pending = self.get_pending(address, source)?;
        self.delete_pending(address, source.as_bytes())?;
        Ok(pending)
    }
}

/// Only replay-derived maps and already applied signed blocks are visible here.
/// There is deliberately no access to the live store's snapshots or afterimages.
pub(crate) struct ReplayedLedger<'a> {
    pub mutations: Option<&'a mut crate::confirmed_replay::mutations::Mutations>,
    pub accounts: &'a mut HashMap<WalletAddress, AccountInfo>,
    pub pending: &'a mut HashMap<(WalletAddress, TxHash), PendingInfo>,
    pub metadata: &'a mut HashMap<String, Vec<u8>>,
    pub applied: &'a dyn crate::confirmed_replay::blocks::AppliedBlocks,
    pub current: &'a StateBlock,
}

impl Ledger for ReplayedLedger<'_> {
    fn take_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        let key = (address.clone(), *source);
        let pending = self
            .pending
            .get(&key)
            .ok_or_else(|| StoreError::NotFound("replayed pending claim".into()))?;
        if let Some(mutations) = &mut self.mutations {
            let mut key = address.as_str().as_bytes().to_vec();
            key.extend_from_slice(source.as_bytes());
            mutations.record(
                burst_store_lmdb::block_undo::UndoTable::Pending,
                key,
                Some(
                    bincode::serialize(pending)
                        .map_err(|e| StoreError::Serialization(e.to_string()))?,
                ),
                None,
            )?;
        }
        self.pending
            .remove(&(address.clone(), *source))
            .ok_or_else(|| StoreError::NotFound("replayed pending claim".into()))
    }

    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.accounts
            .get(address)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("replayed account {address}")))
    }

    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError> {
        if let Some(mutations) = &mut self.mutations {
            let encode = |value| {
                bincode::serialize(value).map_err(|e| StoreError::Serialization(e.to_string()))
            };
            mutations.record(
                burst_store_lmdb::block_undo::UndoTable::Accounts,
                account.address.as_str().as_bytes().to_vec(),
                self.accounts
                    .get(&account.address)
                    .map(encode)
                    .transpose()?,
                Some(encode(account)?),
            )?;
        }
        self.accounts
            .insert(account.address.clone(), account.clone());
        Ok(())
    }

    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        if *hash == self.current.hash {
            bincode::serialize(self.current)
                .map_err(|error| StoreError::Serialization(error.to_string()))
        } else {
            self.applied.encoded(hash)
        }
    }

    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.metadata.get(key).cloned())
    }

    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if let Some(mutations) = &mut self.mutations {
            mutations.record(
                burst_store_lmdb::block_undo::UndoTable::Meta,
                key.as_bytes().to_vec(),
                self.metadata.get(key).cloned(),
                Some(bytes.to_vec()),
            )?;
        }
        self.metadata.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn get_pending(
        &self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        self.pending
            .get(&(address.clone(), *source))
            .cloned()
            .ok_or_else(|| StoreError::NotFound("replayed pending reward".into()))
    }

    fn put_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
        pending: &PendingInfo,
    ) -> Result<(), StoreError> {
        if let Some(mutations) = &mut self.mutations {
            let mut key = address.as_str().as_bytes().to_vec();
            key.extend_from_slice(source.as_bytes());
            let encode = |value| {
                bincode::serialize(value).map_err(|e| StoreError::Serialization(e.to_string()))
            };
            mutations.record(
                burst_store_lmdb::block_undo::UndoTable::Pending,
                key,
                self.pending
                    .get(&(address.clone(), *source))
                    .map(encode)
                    .transpose()?,
                Some(encode(pending)?),
            )?;
        }
        self.pending
            .insert((address.clone(), *source), pending.clone());
        Ok(())
    }

    fn all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        Ok(self
            .pending
            .iter()
            .map(|((address, hash), pending)| (address.clone(), *hash, pending.clone()))
            .collect())
    }
}
