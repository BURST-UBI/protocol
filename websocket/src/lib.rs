//! WebSocket subscriptions for transient node updates.
//!
//! The running node publishes confirmations and provisional account updates.
//! Governance and verification topics exist, but their publishers are not wired
//! into node lifecycle changes. Missed events must be reconciled through RPC.

#[cfg(feature = "server")]
pub mod server;
pub mod state;
pub mod subscriptions;

#[cfg(feature = "server")]
pub use server::WebSocketServer;
pub use state::WsState;
pub use subscriptions::{
    ClientMessage, ClientSubscriptions, ServerMessage, SubscriptionEvent, SubscriptionFilter,
    SubscriptionTopic,
};
