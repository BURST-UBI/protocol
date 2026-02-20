//! WebSocket server for real-time updates.
//!
//! Clients can subscribe to:
//! - Block confirmations
//! - Account updates (balance changes)
//! - Governance events (new proposals, vote results)
//! - Verification events

pub mod server;
pub mod subscriptions;

pub use server::{WebSocketServer, WsState};
pub use subscriptions::{
    ClientMessage, ClientSubscriptions, ServerMessage, SubscriptionEvent, SubscriptionFilter,
    SubscriptionTopic,
};
