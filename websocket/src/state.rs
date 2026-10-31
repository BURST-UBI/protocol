//! Event publication independent of WebSocket listener/session handling.
use crate::subscriptions::{SubscriptionEvent, SubscriptionTopic};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Bounded broadcast channels shared by publishers and WebSocket sessions.
/// Events are not retained for future subscribers; lagging receivers lose events.
pub struct WsState {
    /// Broadcast channel for confirmation events.
    pub confirmation_tx: broadcast::Sender<Arc<SubscriptionEvent>>,
    /// Broadcast channel for account update events.
    pub account_update_tx: broadcast::Sender<Arc<SubscriptionEvent>>,
    /// Broadcast channel for governance events.
    pub governance_tx: broadcast::Sender<Arc<SubscriptionEvent>>,
    /// Broadcast channel for verification events.
    pub verification_tx: broadcast::Sender<Arc<SubscriptionEvent>>,
}

impl WsState {
    /// Create a new `WsState` with the given channel capacity for each topic.
    pub fn new(channel_capacity: usize) -> Self {
        let (confirmation_tx, _) = broadcast::channel(channel_capacity);
        let (account_update_tx, _) = broadcast::channel(channel_capacity);
        let (governance_tx, _) = broadcast::channel(channel_capacity);
        let (verification_tx, _) = broadcast::channel(channel_capacity);

        Self {
            confirmation_tx,
            account_update_tx,
            governance_tx,
            verification_tx,
        }
    }

    /// Get the broadcast sender for a given topic.
    pub fn sender_for(
        &self,
        topic: &SubscriptionTopic,
    ) -> &broadcast::Sender<Arc<SubscriptionEvent>> {
        match topic {
            SubscriptionTopic::Confirmation => &self.confirmation_tx,
            SubscriptionTopic::AccountUpdate => &self.account_update_tx,
            SubscriptionTopic::Governance => &self.governance_tx,
            SubscriptionTopic::Verification => &self.verification_tx,
        }
    }

    /// Publish a confirmation. The node currently supplies the account TRST
    /// balance as `amount`, not the transferred amount.
    pub fn publish_confirmation(&self, account: &str, block_hash: &str, amount: &str) {
        let event = SubscriptionEvent {
            topic: "confirmation".into(),
            data: serde_json::json!({
                "account": account,
                "block_hash": block_hash,
                "amount": amount,
            }),
            timestamp: unix_timestamp_secs(),
        };
        let _ = self.confirmation_tx.send(Arc::new(event));
    }

    /// Publish an account balance update. The node calls this after acceptance,
    /// before confirmation; clients must treat the balance as provisional.
    pub fn publish_account_update(&self, account: &str, balance: &str, change_type: &str) {
        let event = SubscriptionEvent {
            topic: "account_update".into(),
            data: serde_json::json!({
                "account": account,
                "balance": balance,
                "change_type": change_type,
            }),
            timestamp: unix_timestamp_secs(),
        };
        let _ = self.account_update_tx.send(Arc::new(event));
    }

    /// Publish a governance event; node lifecycle changes do not call this helper.
    pub fn publish_governance(&self, event_type: &str, proposal_id: &str, account: &str) {
        let event = SubscriptionEvent {
            topic: "governance".into(),
            data: serde_json::json!({
                "event_type": event_type,
                "proposal_id": proposal_id,
                "account": account,
            }),
            timestamp: unix_timestamp_secs(),
        };
        let _ = self.governance_tx.send(Arc::new(event));
    }

    /// Send a verification event to current subscribers. Node lifecycle
    /// settlement does not currently call this publisher.
    pub fn publish_verification(&self, event_type: &str, subject: &str, account: &str) {
        let event = SubscriptionEvent {
            topic: "verification".into(),
            data: serde_json::json!({
                "event_type": event_type,
                "subject": subject,
                "account": account,
            }),
            timestamp: unix_timestamp_secs(),
        };
        let _ = self.verification_tx.send(Arc::new(event));
    }
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
