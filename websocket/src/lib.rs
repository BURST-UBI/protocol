//! WebSocket subscriptions for transient node updates.
//!
//! The running node publishes confirmations and provisional account updates.
//! Governance and verification topics exist, but their publishers are not wired
//! into node lifecycle changes. Missed events must be reconciled through RPC.

pub mod state;
#[cfg(feature = "server")]
pub mod server;
pub mod subscriptions;

pub use state::WsState;
#[cfg(feature = "server")]
pub use server::WebSocketServer;
pub use subscriptions::{
    ClientMessage, ClientSubscriptions, ServerMessage, SubscriptionEvent, SubscriptionFilter,
    SubscriptionTopic,
};
