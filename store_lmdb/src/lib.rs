//! LMDB storage backend for the BURST protocol.
//!
//! Implements all storage traits from `burst-store` using the `heed` LMDB bindings.
//! Each logical store maps to one or more LMDB databases within a single environment.

pub mod account;
pub mod block;
pub mod brn;
pub mod environment;
pub mod error;
pub mod frontier;
pub mod governance;
pub mod integrity;
pub mod merger_graph;
pub mod meta;
pub mod migration;
pub mod peer;
pub mod pending;
pub mod rep_weights;
pub mod transaction;
pub mod trst_index;
pub mod verification;
pub mod write_batch;

pub use account::LmdbAccountStore;
pub use block::LmdbBlockStore;
pub use brn::LmdbBrnStore;
pub use environment::LmdbEnvironment;
pub use error::LmdbError;
pub use frontier::LmdbFrontierStore;
pub use governance::LmdbGovernanceStore;
pub use integrity::{check_data_dir, check_integrity, IntegrityReport};
pub use merger_graph::LmdbMergerGraphStore;
pub use meta::LmdbMetaStore;
pub use migration::{Migrator, CURRENT_SCHEMA_VERSION};
pub use peer::LmdbPeerStore;
pub use pending::LmdbPendingStore;
pub use rep_weights::LmdbRepWeightStore;
pub use transaction::LmdbTransactionStore;
pub use trst_index::LmdbTrstIndexStore;
pub use verification::LmdbVerificationStore;
pub use write_batch::WriteBatch;
/// Convenience alias — the unified LMDB store wrapping all sub-stores.
pub type LmdbStore = LmdbEnvironment;
