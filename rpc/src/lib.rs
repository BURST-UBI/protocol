//! JSON-RPC server for the BURST node.
//!
//! Provides endpoints for:
//! - Account info, balances (BRN + TRST), history, and pending
//! - Transaction submission (burn, send, split, merge)
//! - Block queries (single and batch)
//! - Work generation
//! - Verification status
//! - Governance proposals, voting, and proposal details
//! - Representative listing
//! - Node telemetry

pub mod error;
pub mod handlers;
pub mod pagination;
pub mod server;

pub use server::{
    BlockProcessorCallback, LedgerCacheView, ProcessResult, RateLimiter, RpcServer, RpcState,
};
