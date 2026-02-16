//! BURST full node — orchestrates all protocol engines.
//!
//! The node is the central coordinator that:
//! - Processes incoming blocks/transactions
//! - Computes BRN balances
//! - Manages the TRST lifecycle and merger graph
//! - Coordinates humanity verification
//! - Handles governance proposals and voting
//! - Maintains clock synchronization
//! - Participates in consensus (representative voting for conflict resolution)

pub mod block_processor;
pub mod config;
pub mod error;
pub mod node;

pub use config::NodeConfig;
pub use error::NodeError;
pub use node::BurstNode;
