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

    /// Remove an account from the frontier entirely.
    ///
    /// Used during rollback of an open block — the account ceases to exist.
    /// Returns the previous head hash if the account was present.
    pub fn remove(&mut self, account: &WalletAddress) -> Option<BlockHash> {
        self.heads.remove(account)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(&format!("brst_{s}"))
    }

    fn hash(v: u8) -> BlockHash {
        BlockHash::new([v; 32])
    }

    #[test]
    fn new_frontier_is_empty() {
        let f = DagFrontier::new();
        assert_eq!(f.account_count(), 0);
        assert!(f.get_head(&addr("a")).is_none());
    }

    #[test]
    fn update_and_get_head() {
        let mut f = DagFrontier::new();
        f.update(addr("alice"), hash(1));
        assert_eq!(f.get_head(&addr("alice")), Some(&hash(1)));
        assert_eq!(f.account_count(), 1);
    }

    #[test]
    fn update_overwrites_head() {
        let mut f = DagFrontier::new();
        f.update(addr("alice"), hash(1));
        f.update(addr("alice"), hash(2));
        assert_eq!(f.get_head(&addr("alice")), Some(&hash(2)));
        assert_eq!(f.account_count(), 1);
    }

    #[test]
    fn multiple_accounts() {
        let mut f = DagFrontier::new();
        f.update(addr("a"), hash(1));
        f.update(addr("b"), hash(2));
        f.update(addr("c"), hash(3));
        assert_eq!(f.account_count(), 3);
        assert_eq!(f.get_head(&addr("b")), Some(&hash(2)));
    }

    #[test]
    fn remove_existing_account() {
        let mut f = DagFrontier::new();
        f.update(addr("a"), hash(1));
        let removed = f.remove(&addr("a"));
        assert_eq!(removed, Some(hash(1)));
        assert!(f.get_head(&addr("a")).is_none());
        assert_eq!(f.account_count(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut f = DagFrontier::new();
        assert!(f.remove(&addr("x")).is_none());
    }

    #[test]
    fn iter_returns_all() {
        let mut f = DagFrontier::new();
        f.update(addr("a"), hash(1));
        f.update(addr("b"), hash(2));
        let entries: Vec<_> = f.iter().collect();
        assert_eq!(entries.len(), 2);
    }
}
