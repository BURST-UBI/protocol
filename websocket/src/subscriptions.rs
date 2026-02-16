//! Subscription management for WebSocket clients.

use serde::{Deserialize, Serialize};

/// A subscription request from a client.
#[derive(Clone, Debug, Deserialize)]
pub struct SubscriptionRequest {
    pub topic: SubscriptionTopic,
    pub filter: Option<SubscriptionFilter>,
}

/// Available subscription topics.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum SubscriptionTopic {
    /// Block confirmations.
    Confirmation,
    /// Account balance updates.
    AccountUpdate,
    /// Governance events.
    Governance,
    /// Verification events.
    Verification,
}

/// Optional filter for subscriptions.
#[derive(Clone, Debug, Deserialize)]
pub struct SubscriptionFilter {
    /// Only receive events for these accounts.
    pub accounts: Option<Vec<String>>,
}

/// An event sent to subscribed clients.
#[derive(Clone, Debug, Serialize)]
pub struct SubscriptionEvent {
    pub topic: String,
    pub data: serde_json::Value,
    pub timestamp: u64,
}
