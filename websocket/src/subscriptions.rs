//! Subscription management for WebSocket clients.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Client-to-server message envelope.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Subscribe to a topic with an optional filter.
    Subscribe {
        topic: SubscriptionTopic,
        filter: Option<SubscriptionFilter>,
    },
    /// Unsubscribe from a topic.
    Unsubscribe { topic: SubscriptionTopic },
    /// Ping to keep the connection alive.
    Ping,
}

/// Available subscription topics.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl std::fmt::Display for SubscriptionTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Confirmation => write!(f, "confirmation"),
            Self::AccountUpdate => write!(f, "account_update"),
            Self::Governance => write!(f, "governance"),
            Self::Verification => write!(f, "verification"),
        }
    }
}

/// Optional filter for subscriptions.
#[derive(Clone, Debug, Deserialize)]
pub struct SubscriptionFilter {
    /// Only receive events for these accounts.
    pub accounts: Option<Vec<String>>,
}

/// An event sent to subscribed clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscriptionEvent {
    pub topic: String,
    pub data: serde_json::Value,
    pub timestamp: u64,
}

/// Server-to-client acknowledgement/error message.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Acknowledgement of a subscribe/unsubscribe action.
    Ack {
        action: String,
        topic: SubscriptionTopic,
    },
    /// An event matching a subscription.
    Event(SubscriptionEvent),
    /// Pong response to a ping.
    Pong,
    /// Error message.
    Error { message: String },
}

/// Manages subscriptions for a single WebSocket connection.
pub struct ClientSubscriptions {
    /// Active subscriptions: topic -> optional filter.
    subscriptions: HashMap<SubscriptionTopic, Option<SubscriptionFilter>>,
}

impl ClientSubscriptions {
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
        }
    }

    /// Add or replace a subscription for the given topic.
    pub fn subscribe(&mut self, topic: SubscriptionTopic, filter: Option<SubscriptionFilter>) {
        self.subscriptions.insert(topic, filter);
    }

    /// Remove a subscription for the given topic. Returns true if it was present.
    pub fn unsubscribe(&mut self, topic: &SubscriptionTopic) -> bool {
        self.subscriptions.remove(topic).is_some()
    }

    /// Check whether the client is subscribed to the given topic.
    pub fn is_subscribed(&self, topic: &SubscriptionTopic) -> bool {
        self.subscriptions.contains_key(topic)
    }

    /// Return the set of topics the client is subscribed to.
    pub fn topics(&self) -> Vec<&SubscriptionTopic> {
        self.subscriptions.keys().collect()
    }

    /// Check if an event matches the client's subscription filter for the given topic.
    ///
    /// Returns `false` if the client is not subscribed to the topic.
    /// Returns `true` if subscribed with no filter (match all).
    /// When an account filter is set, the event's `data.account` field must match
    /// one of the listed accounts.
    pub fn matches_filter(&self, topic: &SubscriptionTopic, event: &SubscriptionEvent) -> bool {
        match self.subscriptions.get(topic) {
            None => false,
            Some(None) => true,
            Some(Some(filter)) => {
                if let Some(accounts) = &filter.accounts {
                    if let Some(account) = event.data.get("account").and_then(|v| v.as_str()) {
                        return accounts.iter().any(|a| a == account);
                    }
                    return false;
                }
                true
            }
        }
    }
}

impl Default for ClientSubscriptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(topic: &str, account: &str) -> SubscriptionEvent {
        SubscriptionEvent {
            topic: topic.to_string(),
            data: serde_json::json!({ "account": account, "amount": "100" }),
            timestamp: 1000,
        }
    }

    fn make_event_no_account(topic: &str) -> SubscriptionEvent {
        SubscriptionEvent {
            topic: topic.to_string(),
            data: serde_json::json!({ "proposal_id": "prop_1" }),
            timestamp: 1000,
        }
    }

    #[test]
    fn test_subscribe_and_is_subscribed() {
        let mut subs = ClientSubscriptions::new();
        assert!(!subs.is_subscribed(&SubscriptionTopic::Confirmation));

        subs.subscribe(SubscriptionTopic::Confirmation, None);
        assert!(subs.is_subscribed(&SubscriptionTopic::Confirmation));
        assert!(!subs.is_subscribed(&SubscriptionTopic::Governance));
    }

    #[test]
    fn test_unsubscribe() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(SubscriptionTopic::Confirmation, None);
        assert!(subs.is_subscribed(&SubscriptionTopic::Confirmation));

        let removed = subs.unsubscribe(&SubscriptionTopic::Confirmation);
        assert!(removed);
        assert!(!subs.is_subscribed(&SubscriptionTopic::Confirmation));

        // Unsubscribing again returns false.
        let removed = subs.unsubscribe(&SubscriptionTopic::Confirmation);
        assert!(!removed);
    }

    #[test]
    fn test_matches_filter_no_subscription() {
        let subs = ClientSubscriptions::new();
        let event = make_event("confirmation", "brst_alice");
        assert!(!subs.matches_filter(&SubscriptionTopic::Confirmation, &event));
    }

    #[test]
    fn test_matches_filter_no_filter_matches_all() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(SubscriptionTopic::Confirmation, None);

        let event = make_event("confirmation", "brst_alice");
        assert!(subs.matches_filter(&SubscriptionTopic::Confirmation, &event));

        let event2 = make_event("confirmation", "brst_bob");
        assert!(subs.matches_filter(&SubscriptionTopic::Confirmation, &event2));
    }

    #[test]
    fn test_matches_filter_with_account_filter() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(
            SubscriptionTopic::Confirmation,
            Some(SubscriptionFilter {
                accounts: Some(vec!["brst_alice".to_string(), "brst_carol".to_string()]),
            }),
        );

        let event_alice = make_event("confirmation", "brst_alice");
        assert!(subs.matches_filter(&SubscriptionTopic::Confirmation, &event_alice));

        let event_bob = make_event("confirmation", "brst_bob");
        assert!(!subs.matches_filter(&SubscriptionTopic::Confirmation, &event_bob));

        let event_carol = make_event("confirmation", "brst_carol");
        assert!(subs.matches_filter(&SubscriptionTopic::Confirmation, &event_carol));
    }

    #[test]
    fn test_matches_filter_account_filter_event_missing_account_field() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(
            SubscriptionTopic::Governance,
            Some(SubscriptionFilter {
                accounts: Some(vec!["brst_alice".to_string()]),
            }),
        );

        // Event has no "account" field in data — does not match.
        let event = make_event_no_account("governance");
        assert!(!subs.matches_filter(&SubscriptionTopic::Governance, &event));
    }

    #[test]
    fn test_matches_filter_empty_account_filter_matches_all() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(
            SubscriptionTopic::AccountUpdate,
            Some(SubscriptionFilter { accounts: None }),
        );

        let event = make_event("account_update", "brst_anyone");
        assert!(subs.matches_filter(&SubscriptionTopic::AccountUpdate, &event));
    }

    #[test]
    fn test_replace_subscription_filter() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(
            SubscriptionTopic::Confirmation,
            Some(SubscriptionFilter {
                accounts: Some(vec!["brst_alice".to_string()]),
            }),
        );

        let event_bob = make_event("confirmation", "brst_bob");
        assert!(!subs.matches_filter(&SubscriptionTopic::Confirmation, &event_bob));

        // Replace filter to include bob.
        subs.subscribe(
            SubscriptionTopic::Confirmation,
            Some(SubscriptionFilter {
                accounts: Some(vec!["brst_bob".to_string()]),
            }),
        );
        assert!(subs.matches_filter(&SubscriptionTopic::Confirmation, &event_bob));
    }

    #[test]
    fn test_topics_returns_subscribed_topics() {
        let mut subs = ClientSubscriptions::new();
        subs.subscribe(SubscriptionTopic::Confirmation, None);
        subs.subscribe(SubscriptionTopic::Governance, None);

        let mut topics: Vec<String> = subs.topics().iter().map(|t| t.to_string()).collect();
        topics.sort();
        assert_eq!(topics, vec!["confirmation", "governance"]);
    }

    #[test]
    fn test_client_message_deserialize_subscribe() {
        let json = r#"{"action": "subscribe", "topic": "confirmation", "filter": {"accounts": ["brst_alice"]}}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Subscribe { topic, filter } => {
                assert_eq!(topic, SubscriptionTopic::Confirmation);
                let accounts = filter.unwrap().accounts.unwrap();
                assert_eq!(accounts, vec!["brst_alice".to_string()]);
            }
            _ => panic!("expected Subscribe"),
        }
    }

    #[test]
    fn test_client_message_deserialize_unsubscribe() {
        let json = r#"{"action": "unsubscribe", "topic": "governance"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Unsubscribe { topic } => {
                assert_eq!(topic, SubscriptionTopic::Governance);
            }
            _ => panic!("expected Unsubscribe"),
        }
    }

    #[test]
    fn test_client_message_deserialize_ping() {
        let json = r#"{"action": "ping"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Ping));
    }

    #[test]
    fn test_subscription_topic_display() {
        assert_eq!(SubscriptionTopic::Confirmation.to_string(), "confirmation");
        assert_eq!(
            SubscriptionTopic::AccountUpdate.to_string(),
            "account_update"
        );
        assert_eq!(SubscriptionTopic::Governance.to_string(), "governance");
        assert_eq!(SubscriptionTopic::Verification.to_string(), "verification");
    }
}
