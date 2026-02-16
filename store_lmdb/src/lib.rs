//! LMDB storage backend for the BURST protocol.
//!
//! Implements all storage traits from `burst-store` using the `heed` LMDB bindings.
//! Each logical store maps to one or more LMDB databases within a single environment.

pub mod account;
pub mod block;
pub mod environment;
pub mod error;
pub mod governance;
pub mod merger_graph;
pub mod transaction;
pub mod verification;

pub use environment::LmdbEnvironment;
pub use error::LmdbError;
