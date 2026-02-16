//! WebSocket server for real-time updates.
//!
//! Clients can subscribe to:
//! - Account updates (balance changes)
//! - Block confirmations
//! - Governance events (new proposals, vote results)
//! - Verification events

pub mod server;
pub mod subscriptions;

pub use server::WebSocketServer;
