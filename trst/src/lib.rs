//! TRST holdings, provenance, expiry and revocation.
//!
//! A burn creates TRST at a 1:1 raw-unit ratio. Holdings retain a burn or merge
//! origin, a preceding token identity, and the oldest contributing timestamp
//! for expiry. Transfers and splits preserve that provenance.
//!
//! The merger graph records immediate inputs and indexes their downstream merges.
//! Revocation traverses affected merges and holdings; it is not constant-time.
//! Composition revisions preserve which contributions a later holding contains.
//!
//! These accounting helpers do not verify signatures, authorize burns, or commit
//! account-chain blocks. The node validates and settles those operations.

pub mod engine;
pub mod error;
pub mod merger_graph;
mod comparison;
mod dependency_counts;
mod snapshot_encoding;
mod snapshot_undo;
pub mod token;

pub use engine::{
    ConsumedProvenance, RevocationEvent, TrstEngine, RestorationEvent, WalletPortfolio,
    MAX_MERGE_SOURCES,
};
pub use error::TrstError;
pub use merger_graph::{MergeNode, MergeSource, MergerGraph, MergeRevocation, MergeRestoration};
pub use burst_utils::{Shared, SharedMap, SharedSet};
pub use token::TrstToken;
