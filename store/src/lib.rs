//! Abstract storage traits for the BURST protocol.
//!
//! Backend-independent record interfaces. Atomic production operations use
//! caller-owned transactions in burst-store-lmdb; these traits do not replace
//! that concrete transactional boundary.

pub mod account;
pub mod block;
pub mod brn;
pub mod delegation;
pub mod error;
pub mod frontier;
pub mod governance;
pub mod merger_graph;
pub mod meta;
pub mod peer;
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
pub use peer::PeerStore;
pub use pending::{PendingInfo, PendingStore};
pub use rep_weights::RepWeightStore;
pub use trst_index::TrstIndexStore;

/// Marker for a backend read transaction; concrete backends own its operations.
pub trait ReadTxn {}
/// Marker for writable transactions; this marker alone provides no atomicity.
pub trait WriteTxn: ReadTxn {}
