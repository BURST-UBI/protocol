//! DAG block-lattice ledger.
//!
//! Each account has its own chain (like Nano's block-lattice).
//! Transactions are asynchronous — no global ordering.
//! Consensus is only needed for conflict resolution (double-spends).

pub mod account_chain;
pub mod error;
pub mod frontier;
pub mod genesis;
pub mod ledger;
pub mod pruning;
pub mod snapshot;
pub mod state_block;

pub use account_chain::AccountChain;
pub use error::LedgerError;
pub use frontier::DagFrontier;
pub use genesis::{GenesisConfig, create_genesis_block, genesis_hash};
pub use ledger::{Ledger, LedgerSummary};
pub use pruning::{LedgerPruner, PruneResult, PruningConfig};
pub use snapshot::{AccountSnapshot, LedgerSnapshot};
pub use state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
