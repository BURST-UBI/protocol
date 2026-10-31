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
#[cfg(feature = "server")]
pub mod handlers;
pub mod governance_snapshot;
mod merger_stats;
mod read_executor;
pub use read_executor::ReadExecutor;
mod governance_projection_cache;
pub use governance_projection_cache::GovernanceProjectionCache;
pub mod pagination;
pub use merger_stats::MergerStatsCache;
pub mod state;
#[cfg(feature = "server")]
pub mod server;

pub use state::{BlockProcessorCallback, LedgerCacheView, ProcessResult, RateLimiter, RpcState};
#[cfg(feature = "server")]
pub use server::RpcServer;
#[cfg(feature = "server")]
mod security;

#[cfg(feature = "server")]
mod work_budget;
