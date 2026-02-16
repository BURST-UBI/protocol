//! JSON-RPC server for the BURST node.
//!
//! Provides endpoints for:
//! - Account info and balances (BRN + TRST)
//! - Transaction submission (burn, send, split, merge)
//! - Block queries
//! - Verification status
//! - Governance proposals and voting
//! - Node telemetry

pub mod error;
pub mod handlers;
pub mod server;

pub use server::RpcServer;
