//! Action-based JSON-over-HTTP API for the BURST node.
//!
//! Provides endpoints for:
//! - Account info, balances (BRN + TRST), history, and pending
//! - Transaction submission (burn, send, merge)
//! - Block queries (single and batch)
//! - Work generation
//! - Verification status
//! - Governance proposals, voting, and proposal details
//! - Representative listing
//! - Node telemetry

pub mod contracts;
pub mod error;
pub mod governance_snapshot;
#[cfg(feature = "server")]
pub mod handlers;
mod merger_stats;
mod read_executor;
pub use read_executor::ReadExecutor;
mod governance_projection_cache;
pub use governance_projection_cache::GovernanceProjectionCache;
pub mod pagination;
pub use merger_stats::MergerStatsCache;
#[cfg(feature = "server")]
pub mod server;
pub mod state;

#[cfg(feature = "server")]
pub use server::RpcServer;
pub use state::{BlockProcessorCallback, LedgerCacheView, ProcessResult, RateLimiter, RpcState};
#[cfg(feature = "server")]
mod security;

#[cfg(feature = "server")]
mod work_budget;
