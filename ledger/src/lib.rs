//! DAG block-lattice ledger.
//!
//! Each account has its own chain (like Nano's block-lattice).
//! Transactions are asynchronous — no global ordering.
//! Consensus is only needed for conflict resolution (double-spends).

pub mod account_chain;
pub mod error;
pub mod frontier;
pub mod pruning;
pub mod state_block;

pub use account_chain::AccountChain;
pub use error::LedgerError;
pub use frontier::DagFrontier;
pub use state_block::{StateBlock, BlockType};
