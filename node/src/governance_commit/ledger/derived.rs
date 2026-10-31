use super::*;
use std::collections::HashMap;

/// Replay ledger over caller-supplied reconstructed state. Reads do not fall
/// back to the live database. Writes update these maps and optionally record
/// undo mutations; the caller must discard the candidate on replay failure.
pub(crate) struct DerivedLedger<'a> {
    pub mutations: Option<&'a mut crate::confirmed_replay::mutations::Mutations>,
    pub accounts: &'a mut HashMap<WalletAddress, AccountInfo>,
    pub metadata: &'a mut HashMap<String, Vec<u8>>,
    pub applied: &'a dyn crate::confirmed_replay::blocks::AppliedBlocks,
    pub contents: &'a HashMap<BlockHash, Vec<u8>>,
    pub bootstrap: &'a mut burst_verification::bootstrap::BootstrapPhase,
}

impl Ledger for DerivedLedger<'_> {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.accounts
            .get(address)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(address.to_string()))
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
        self.applied.encoded(hash)
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
        self.metadata.insert(key.into(), bytes.into());
        Ok(())
    }
    // Replay counts the supplied account map; there is no persisted count index.
    fn verified_account_count(&self) -> Result<u64, StoreError> {
        Ok(self
            .accounts
            .values()
            .filter(|account| account.state.is_verified())
            .count() as u64)
    }
    fn full_content(&self, block: &StateBlock) -> Result<ProposalContent, String> {
        let bytes = self
            .contents
            .get(&block.hash)
            .ok_or("replayed proposal body missing")?;
        crate::governance_content::authenticate(block)?
            .decode_verified(bytes)
            .map_err(|error| error.to_string())
    }
    fn bootstrap_phase(&self) -> Result<burst_verification::bootstrap::BootstrapPhase, String> {
        Ok(*self.bootstrap)
    }
    fn observe_bootstrap(&mut self, params: &ProtocolParams) -> Result<(), String> {
        self.bootstrap.observe(
            self.verified_account_count()
                .map_err(|error| error.to_string())?,
            params.bootstrap_exit_threshold,
        );
        Ok(())
    }
}
