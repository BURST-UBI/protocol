use super::*;
use crate::confirmed_replay::snapshot::history::StoredHistoryRoots;

/// Account prefixes available to a recovery job. Memory-backed roots index both
/// account and head; stored roots use database indexes and track consumed accounts.
pub(super) enum AccountRoots {
    Memory {
        accounts: HashMap<WalletAddress, HistoryCursor>,
        heads: HashMap<BlockHash, usize>,
    },
    Stored {
        roots: Arc<StoredHistoryRoots>,
        consumed: HashSet<WalletAddress>,
    },
}
impl Default for AccountRoots {
    fn default() -> Self {
        Self::Memory {
            accounts: HashMap::new(),
            heads: HashMap::new(),
        }
    }
}
impl AccountRoots {
    /// Cursors must have nonempty heads and distinct accounts.
    pub(super) fn memory(cursors: Vec<HistoryCursor>) -> Self {
        let mut accounts = HashMap::with_capacity(cursors.len());
        let mut heads = HashMap::with_capacity(cursors.len());
        for cursor in cursors {
            let head = cursor.next().expect("validated nonempty cursor");
            *heads.entry(head.hash).or_insert(0) += 1;
            accounts.insert(head.account, cursor);
        }
        Self::Memory { accounts, heads }
    }
    pub(super) fn stored(roots: Arc<StoredHistoryRoots>) -> Self {
        Self::Stored {
            roots,
            consumed: HashSet::new(),
        }
    }
    /// Remove an account from this job and return its authenticated cursor.
    pub(super) fn take(
        &mut self,
        account: &WalletAddress,
    ) -> Result<Option<HistoryCursor>, StoreError> {
        match self {
            Self::Memory { accounts, heads } => {
                let cursor = accounts.remove(account);
                if let Some(head) = cursor.as_ref().and_then(HistoryCursor::next) {
                    let count = heads.get_mut(&head.hash).expect("indexed account head");
                    *count -= 1;
                    if *count == 0 {
                        heads.remove(&head.hash);
                    }
                }
                Ok(cursor)
            }
            Self::Stored { roots, consumed } => {
                if consumed.contains(account) {
                    return Ok(None);
                }
                let cursor = roots.cursor(account)?;
                if cursor.is_some() {
                    consumed.insert(account.clone());
                }
                Ok(cursor)
            }
        }
    }
    pub(super) fn contains_head(&self, hash: BlockHash) -> Result<bool, StoreError> {
        match self {
            Self::Memory { heads, .. } => Ok(heads.contains_key(&hash)),
            Self::Stored { roots, consumed } => Ok(roots
                .account_for_head(hash)?
                .is_some_and(|account| !consumed.contains(&account))),
        }
    }
}
