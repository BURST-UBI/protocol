//! LMDB storage backend for the BURST protocol.
//!
//! Implements storage interfaces from `burst-store` using the `heed` LMDB bindings.
//! Each logical store maps to one or more LMDB databases within a single environment.

pub mod account;
pub mod block;
pub mod block_undo;
pub mod checkpoint_heads;
pub mod brn;
mod confirmation_queue;
pub mod confirmed_authority;
pub mod confirmed_governance;
pub mod environment;
pub mod error;
mod fork_candidate;
pub mod frontier;
pub mod governance;
mod governance_snapshot;
mod graph_position;
pub mod database_checks;
pub mod merger_graph;
pub mod meta;
pub mod schema;
pub mod peer;
pub mod pending;
pub mod rep_weights;
pub mod retention;
pub mod read_view;
mod shared_env;
pub mod transaction;
pub mod trst_index;
pub mod verification;
pub mod write_batch;

/// Named database slots required by the current storage schema.
pub const REQUIRED_DATABASES: u32 = 32;

pub use account::LmdbAccountStore;
pub use block::LmdbBlockStore;
pub use brn::LmdbBrnStore;
pub use environment::LmdbEnvironment;
pub use error::LmdbError;
pub use frontier::LmdbFrontierStore;
pub use governance::LmdbGovernanceStore;
pub use database_checks::{check_data_dir, check_databases, DatabaseCheckReport};
pub use merger_graph::LmdbMergerGraphStore;
pub use meta::LmdbMetaStore;
pub use schema::{check_schema, CURRENT_SCHEMA_VERSION};
pub use peer::LmdbPeerStore;
pub use pending::LmdbPendingStore;
pub use rep_weights::LmdbRepWeightStore;
pub use transaction::LmdbTransactionStore;
pub use trst_index::LmdbTrstIndexStore;
pub use verification::LmdbVerificationStore;
pub use write_batch::{LmdbReadTransaction, LmdbWriteTransaction};
/// The LMDB environment and its logical stores.
pub type LmdbStore = LmdbEnvironment;

mod verified_index;

// Raw persistent recovery input storage; does not confer replay authority.
pub mod recovery_inputs;

pub use fork_candidate::ForkCandidateWriteError;
pub use heed::{FileGrowthLimit, GrowthOutcome, GrowthRetry, GrowthWatch};
