//! Abstract storage traits for the BURST protocol.
//!
//! Every storage backend (LMDB, RocksDB, in-memory for testing) implements
//! these traits. The rest of the codebase depends only on the traits.

pub mod account;
pub mod block;
pub mod brn;
pub mod delegation;
pub mod error;
pub mod frontier;
pub mod governance;
pub mod merger_graph;
pub mod meta;
pub mod pending;
pub mod rep_weights;
pub mod transaction;
pub mod trst_index;
pub mod verification;

pub use brn::BrnStore;
pub use delegation::{DelegationRecord, DelegationStore};
pub use error::StoreError;
pub use frontier::FrontierStore;
pub use meta::MetaStore;
pub use pending::{PendingInfo, PendingStore};
pub use rep_weights::RepWeightStore;
pub use trst_index::TrstIndexStore;

/// Opaque transaction handle for atomic multi-store operations.
/// Implementations can downcast to their specific transaction type.
pub trait ReadTxn {}
/// Writable transaction handle (extends ReadTxn).
pub trait WriteTxn: ReadTxn {}
