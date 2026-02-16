//! DAG frontier — the set of all account chain tips.

use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;

/// The current frontier of the DAG: one head block per account.
pub struct DagFrontier {
    /// Maps account address → head block hash.
    heads: HashMap<WalletAddress, BlockHash>,
}

impl DagFrontier {
    pub fn new() -> Self {
        Self {
            heads: HashMap::new(),
        }
    }

    pub fn update(&mut self, account: WalletAddress, head: BlockHash) {
        self.heads.insert(account, head);
    }

    pub fn get_head(&self, account: &WalletAddress) -> Option<&BlockHash> {
        self.heads.get(account)
    }

    pub fn account_count(&self) -> usize {
        self.heads.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&WalletAddress, &BlockHash)> {
        self.heads.iter()
    }
}

impl Default for DagFrontier {
    fn default() -> Self {
        Self::new()
    }
}
