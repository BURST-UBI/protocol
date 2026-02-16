//! Abstract storage traits for the BURST protocol.
//!
//! Every storage backend (LMDB, RocksDB, in-memory for testing) implements
//! these traits. The rest of the codebase depends only on the traits.

pub mod account;
pub mod block;
pub mod error;
pub mod governance;
pub mod merger_graph;
pub mod transaction;
pub mod verification;

pub use error::StoreError;
