//! Standalone account-chain confirmation processor.
//!
//! Advances one account's confirmation height through the supplied ChainWalker.
//! The running node uses causal_cementation to handle cross-account dependencies
//! and confirmed shared state in the same transaction.

use std::collections::VecDeque;
use std::sync::Arc;

use burst_ledger::StateBlock;
use burst_store::account::{AccountInfo, AccountStore};
use burst_store::block::BlockStore;
use burst_types::BlockHash;

/// Outcome of processing a confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CementResult {
    /// Blocks were successfully cemented.
    Cemented {
        /// Number of blocks cemented in this batch.
        blocks_cemented: u64,
        /// New confirmation height after cementing.
        new_height: u64,
    },
    /// The block was already cemented (height already past it).
    AlreadyCemented,
    /// The confirmed block could not be found in the account chain.
    BlockNotFound,
    /// The account is unknown.
    AccountNotFound,
    /// Persistence failed; never report undurable finality as cemented.
    StoreFailed,
}

/// Walks an account chain and cements blocks up to a confirmed block hash.
///
/// The caller must establish consensus finality and stable chain inputs. This
/// helper checks neither votes nor cross-account dependencies.
pub struct ConfirmationProcessor;

/// Trait abstracting the ability to walk an account's block chain.
///
/// Implementations can read from LMDB, in-memory maps, or test fixtures.
pub trait ChainWalker {
    /// Look up an account's info by the block hash (returns the account that
    /// owns the chain containing this block, along with its current state).
    fn account_for_block(&self, hash: &BlockHash) -> Option<AccountInfo>;

    /// Get the block hash at a specific height in an account's chain.
    /// Height 1 is the open block.
    fn block_at_height(&self, account: &AccountInfo, height: u64) -> Option<BlockHash>;

    /// Look up the block height. The store adapter uses the global hash index
    /// and does not independently verify ownership against the supplied account.
    fn height_of_block(&self, account: &AccountInfo, hash: &BlockHash) -> Option<u64>;

    /// Persist the updated confirmation height for an account.
    fn set_confirmation_height(&mut self, account: &mut AccountInfo, new_height: u64) -> bool;
}

impl ConfirmationProcessor {
    /// Cement all blocks from the account's current confirmation height up to
    /// (and including) the block identified by `confirmed_hash`.
    ///
    /// Collects the entire height interval before writing, with no batch or
    /// memory limit. Returns hashes in account-height order after the walker
    /// reports a successful height update.
    pub fn process<W: ChainWalker>(
        &self,
        confirmed_hash: &BlockHash,
        walker: &mut W,
    ) -> (CementResult, Vec<BlockHash>) {
        let mut account = match walker.account_for_block(confirmed_hash) {
            Some(a) => a,
            None => return (CementResult::AccountNotFound, vec![]),
        };

        let confirmed_height = match walker.height_of_block(&account, confirmed_hash) {
            Some(h) => h,
            None => return (CementResult::BlockNotFound, vec![]),
        };

        let current_height = account.confirmation_height;

        if confirmed_height <= current_height {
            return (CementResult::AlreadyCemented, vec![]);
        }

        // Collect the complete account interval before advancing its height.
        let mut cemented: VecDeque<BlockHash> = VecDeque::new();
        for h in (current_height + 1)..=confirmed_height {
            match walker.block_at_height(&account, h) {
                Some(hash) => cemented.push_back(hash),
                None => return (CementResult::BlockNotFound, vec![]),
            }
        }

        let blocks_cemented = cemented.len() as u64;
        let new_height = confirmed_height;

        if !walker.set_confirmation_height(&mut account, new_height) {
            return (CementResult::StoreFailed, vec![]);
        }

        (
            CementResult::Cemented {
                blocks_cemented,
                new_height,
            },
            cemented.into(),
        )
    }
}

/// Adapter over AccountStore and BlockStore with separate reads and writes.
/// It does not pin a shared snapshot. Read/decode failures become None, so
/// missing data and storage errors share the processor's lookup-error results.
pub struct StoreChainWalker {
    account_store: Arc<dyn AccountStore + Send + Sync>,
    block_store: Arc<dyn BlockStore + Send + Sync>,
}

impl StoreChainWalker {
    pub fn new(
        account_store: Arc<dyn AccountStore + Send + Sync>,
        block_store: Arc<dyn BlockStore + Send + Sync>,
    ) -> Self {
        Self {
            account_store,
            block_store,
        }
    }
}

impl ChainWalker for StoreChainWalker {
    fn account_for_block(&self, hash: &BlockHash) -> Option<AccountInfo> {
        let block_bytes = self.block_store.get_block(hash).ok()?;
        let block: StateBlock = bincode::deserialize(&block_bytes).ok()?;
        self.account_store.get_account(&block.account).ok()
    }

    fn block_at_height(&self, account: &AccountInfo, height: u64) -> Option<BlockHash> {
        self.block_store
            .block_at_height(&account.address, height)
            .ok()
            .flatten()
    }

    fn height_of_block(&self, _account: &AccountInfo, hash: &BlockHash) -> Option<u64> {
        self.block_store.height_of_block(hash).ok().flatten()
    }

    fn set_confirmation_height(&mut self, account: &mut AccountInfo, new_height: u64) -> bool {
        if let Err(e) = self
            .account_store
            .advance_confirmation_height(&account.address, new_height)
        {
            tracing::error!(
                address = %account.address,
                height = new_height,
                "failed to persist confirmation height: {e}"
            );
            return false;
        }
        account.confirmation_height = account.confirmation_height.max(new_height);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, WalletAddress, WalletState};
    use std::collections::HashMap;

    /// In-memory chain walker for testing.
    struct MockChainWalker {
        /// account address → AccountInfo
        accounts: HashMap<WalletAddress, AccountInfo>,
        /// account address → vec of block hashes (index 0 = height 1)
        chains: HashMap<WalletAddress, Vec<BlockHash>>,
        /// block hash → account address
        block_to_account: HashMap<BlockHash, WalletAddress>,
    }

    impl MockChainWalker {
        fn new() -> Self {
            Self {
                accounts: HashMap::new(),
                chains: HashMap::new(),
                block_to_account: HashMap::new(),
            }
        }

        fn add_account(&mut self, info: AccountInfo, chain: Vec<BlockHash>) {
            for hash in &chain {
                self.block_to_account.insert(*hash, info.address.clone());
            }
            self.chains.insert(info.address.clone(), chain);
            self.accounts.insert(info.address.clone(), info);
        }
    }

    impl ChainWalker for MockChainWalker {
        fn account_for_block(&self, hash: &BlockHash) -> Option<AccountInfo> {
            let addr = self.block_to_account.get(hash)?;
            self.accounts.get(addr).cloned()
        }

        fn block_at_height(&self, account: &AccountInfo, height: u64) -> Option<BlockHash> {
            let chain = self.chains.get(&account.address)?;
            if height == 0 || height as usize > chain.len() {
                return None;
            }
            Some(chain[(height - 1) as usize])
        }

        fn height_of_block(&self, account: &AccountInfo, hash: &BlockHash) -> Option<u64> {
            let chain = self.chains.get(&account.address)?;
            chain.iter().position(|h| h == hash).map(|i| (i + 1) as u64)
        }

        fn set_confirmation_height(&mut self, account: &mut AccountInfo, new_height: u64) -> bool {
            account.confirmation_height = new_height;
            if let Some(stored) = self.accounts.get_mut(&account.address) {
                stored.confirmation_height = new_height;
            }
            true
        }
    }

    fn test_addr() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_rep() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_account(addr: WalletAddress, head: BlockHash, block_count: u64) -> AccountInfo {
        AccountInfo {
            address: addr,
            state: WalletState::Unverified,
            verified_at: None,
            head,
            block_count,
            confirmation_height: 0,
            representative: test_rep(),
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        }
    }

    #[test]
    fn cement_single_block() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let h1 = make_hash(0x01);
        let account = make_account(test_addr(), h1, 1);
        walker.add_account(account, vec![h1]);

        let (result, cemented) = processor.process(&h1, &mut walker);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 1,
                new_height: 1,
            }
        );
        assert_eq!(cemented, vec![h1]);

        // Account's confirmation height should be updated.
        let updated = walker.accounts.get(&test_addr()).unwrap();
        assert_eq!(updated.confirmation_height, 1);
    }

    #[test]
    fn cement_multiple_blocks() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let h1 = make_hash(0x01);
        let h2 = make_hash(0x02);
        let h3 = make_hash(0x03);
        let account = make_account(test_addr(), h3, 3);
        walker.add_account(account, vec![h1, h2, h3]);

        // Confirm h3 — should cement h1, h2, h3 (all three).
        let (result, cemented) = processor.process(&h3, &mut walker);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 3,
                new_height: 3,
            }
        );
        assert_eq!(cemented, vec![h1, h2, h3]);
    }

    #[test]
    fn cement_incremental() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let h1 = make_hash(0x01);
        let h2 = make_hash(0x02);
        let h3 = make_hash(0x03);
        let account = make_account(test_addr(), h3, 3);
        walker.add_account(account, vec![h1, h2, h3]);

        // First: confirm h1.
        let (result, cemented) = processor.process(&h1, &mut walker);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 1,
                new_height: 1,
            }
        );
        assert_eq!(cemented, vec![h1]);

        // Second: confirm h3 — should cement h2 and h3 only.
        let (result, cemented) = processor.process(&h3, &mut walker);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 2,
                new_height: 3,
            }
        );
        assert_eq!(cemented, vec![h2, h3]);
    }

    #[test]
    fn already_cemented() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let h1 = make_hash(0x01);
        let h2 = make_hash(0x02);
        let account = make_account(test_addr(), h2, 2);
        walker.add_account(account, vec![h1, h2]);

        // Cement up to h2.
        processor.process(&h2, &mut walker);

        // Try to cement h1 again — already cemented.
        let (result, cemented) = processor.process(&h1, &mut walker);
        assert_eq!(result, CementResult::AlreadyCemented);
        assert!(cemented.is_empty());
    }

    #[test]
    fn account_not_found() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let unknown = make_hash(0xFF);
        let (result, cemented) = processor.process(&unknown, &mut walker);
        assert_eq!(result, CementResult::AccountNotFound);
        assert!(cemented.is_empty());
    }

    #[test]
    fn cement_does_not_skip_blocks() {
        let processor = ConfirmationProcessor;
        let mut walker = MockChainWalker::new();

        let h1 = make_hash(0x01);
        let h2 = make_hash(0x02);
        let h3 = make_hash(0x03);
        let h4 = make_hash(0x04);
        let account = make_account(test_addr(), h4, 4);
        walker.add_account(account, vec![h1, h2, h3, h4]);

        // Cement h2 first.
        let (result, cemented) = processor.process(&h2, &mut walker);
        assert_eq!(cemented, vec![h1, h2]);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 2,
                new_height: 2,
            }
        );

        // Now cement h4.
        let (result, cemented) = processor.process(&h4, &mut walker);
        assert_eq!(cemented, vec![h3, h4]);
        assert_eq!(
            result,
            CementResult::Cemented {
                blocks_cemented: 2,
                new_height: 4,
            }
        );
    }
}
