//! Governance reads either an uncommitted writer or replay-derived facts.

use burst_governance::ProposalContent;
use burst_ledger::StateBlock;
use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{BlockHash, ProtocolParams, WalletAddress};

pub(crate) trait Ledger {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError>;
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError>;
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError>;
    fn verified_account_count(&self) -> Result<u64, StoreError>;
    fn full_content(&self, block: &StateBlock) -> Result<ProposalContent, String>;
    fn bootstrap_phase(&self) -> Result<burst_verification::bootstrap::BootstrapPhase, String>;
    fn observe_bootstrap(&mut self, params: &ProtocolParams) -> Result<(), String>;
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
    fn verified_account_count(&self) -> Result<u64, StoreError> {
        LmdbWriteTransaction::verified_account_count(self)
    }
    fn full_content(&self, block: &StateBlock) -> Result<ProposalContent, String> {
        crate::governance_content::load(self, block)
    }
    fn bootstrap_phase(&self) -> Result<burst_verification::bootstrap::BootstrapPhase, String> {
        crate::bootstrap_phase::read(self)
    }
    fn observe_bootstrap(&mut self, params: &ProtocolParams) -> Result<(), String> {
        crate::bootstrap_phase::observe(self, params).map(|_| ())
    }
}

pub(crate) mod derived;
