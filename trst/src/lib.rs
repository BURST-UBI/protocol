//! TRST (Trust) — the transferable currency lifecycle engine.
//!
//! TRST is created when BRN is burned (1:1 ratio). It carries:
//! - `origin`: hash of the original burn transaction (determines expiry)
//! - `link`: hash of the immediately preceding transaction
//!
//! This crate handles the full lifecycle: mint, transfer, split, merge, expiry, revocation.
//! It also maintains the **merger graph** — the forward index enabling O(1) revocation.

pub mod engine;
pub mod error;
pub mod merger_graph;
pub mod token;

pub use engine::{ConsumedProvenance, PendingReturnResult, PendingTokenInfo, TrstEngine, UnRevocationResult, WalletPortfolio};
pub use error::TrstError;
pub use merger_graph::{MergerGraph, UnRevocationEvent};
pub use token::TrstToken;
