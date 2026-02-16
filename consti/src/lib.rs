//! The Consti — BURST's on-chain constitution.
//!
//! A separate ledger for governance that can't be reduced to numbers.
//! Defines: what it means to be a legitimate participant, what constitutes fraud,
//! what standards of evidence are acceptable, and participant rights/responsibilities.
//!
//! Uses the same 4-phase governance mechanism but can have its own separate
//! supermajority threshold.

pub mod amendment;
pub mod document;
pub mod engine;
pub mod error;

pub use amendment::Amendment;
pub use document::ConstiDocument;
pub use engine::ConstiEngine;
pub use error::ConstiError;
