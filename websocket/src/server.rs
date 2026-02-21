//! WebSocket server implementation.
//!
//! Accepts WebSocket connections at `/ws` and allows clients to subscribe
//! to real-time event topics (confirmations, account updates, governance,
//! verification). Events are delivered via broadcast channels and filtered
//! per-client based on subscription filters.

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::subscriptions::{
    ClientMessage, ClientSubscriptions, ServerMessage, SubscriptionEvent, SubscriptionTopic,
};

/// Shared state for the WebSocket server, holding broadcast channels
/// for each event topic.
pub struct WsState {
    /// Broadcast channel for confirmation events.
    pub confirmation_tx: broadcast::Sender<String>,
    /// Broadcast channel for account update events.
    pub account_update_tx: broadcast::Sender<String>,
    /// Broadcast channel for governance events.
    pub governance_tx: broadcast::Sender<String>,
    /// Broadcast channel for verification events.
    pub verification_tx: broadcast::Sender<String>,
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
    pub fn sender_for(&self, topic: &SubscriptionTopic) -> &broadcast::Sender<String> {
        match topic {
            SubscriptionTopic::Confirmation => &self.confirmation_tx,
            SubscriptionTopic::AccountUpdate => &self.account_update_tx,
            SubscriptionTopic::Governance => &self.governance_tx,
            SubscriptionTopic::Verification => &self.verification_tx,
        }
    }

    /// Publish a block confirmation event.
    pub fn publish_confirmation(&self, account: &str, block_hash: &str, amount: &str) {
        let event = serde_json::json!({
            "topic": "confirmation",
            "data": {
                "account": account,
                "block_hash": block_hash,
                "amount": amount,
            },
            "timestamp": unix_timestamp_secs(),
        });
        let _ = self.confirmation_tx.send(event.to_string());
    }

    /// Publish an account update event.
    pub fn publish_account_update(&self, account: &str, balance: &str, change_type: &str) {
        let event = serde_json::json!({
            "topic": "account_update",
            "data": {
                "account": account,
                "balance": balance,
                "change_type": change_type,
            },
            "timestamp": unix_timestamp_secs(),
        });
        let _ = self.account_update_tx.send(event.to_string());
    }

    /// Publish a governance event.
    pub fn publish_governance(&self, event_type: &str, proposal_id: &str, account: &str) {
        let event = serde_json::json!({
            "topic": "governance",
            "data": {
                "event_type": event_type,
                "proposal_id": proposal_id,
                "account": account,
            },
            "timestamp": unix_timestamp_secs(),
        });
        let _ = self.governance_tx.send(event.to_string());
    }

    /// Publish a verification event.
    pub fn publish_verification(&self, event_type: &str, subject: &str, account: &str) {
        let event = serde_json::json!({
            "topic": "verification",
            "data": {
                "event_type": event_type,
                "subject": subject,
                "account": account,
            },
            "timestamp": unix_timestamp_secs(),
        });
        let _ = self.verification_tx.send(event.to_string());
    }
}

/// The WebSocket server, configured with a port and shared state.
pub struct WebSocketServer {
    pub port: u16,
    pub state: Arc<WsState>,
}

impl WebSocketServer {
    /// Create a new server with a default channel capacity of 256.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            state: Arc::new(WsState::new(256)),
        }
    }

    /// Create a new server with the provided shared state.
    pub fn with_state(port: u16, state: Arc<WsState>) -> Self {
        Self { port, state }
    }

    /// Start listening for WebSocket connections. This runs until the server
    /// is shut down.
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let state = self.state.clone();
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state);

        let addr = format!("0.0.0.0:{}", self.port);
        info!("WebSocket server listening on {}", addr);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

/// Axum handler that upgrades an HTTP request to a WebSocket connection.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WsState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle a single WebSocket connection.
///
/// The flow:
/// 1. Split the socket into sender and receiver halves.
/// 2. Listen for client messages (subscribe, unsubscribe, ping).
/// 3. For each active subscription, spawn a forwarder task that reads from
///    the topic broadcast channel and sends matching events to the client.
/// 4. Clean up all forwarder tasks when the client disconnects.
async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    let (ws_sender, mut ws_receiver) = socket.split();

    // Wrap the sender in an Arc<Mutex> so forwarder tasks can share it.
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let mut client_subs = ClientSubscriptions::new();

    // Track spawned forwarder tasks so we can abort them on disconnect/unsubscribe.
    let mut forwarder_handles: std::collections::HashMap<
        SubscriptionTopic,
        tokio::task::JoinHandle<()>,
    > = std::collections::HashMap::new();

    debug!("New WebSocket client connected");

    while let Some(msg_result) = ws_receiver.next().await {
        let msg = match msg_result {
            Ok(msg) => msg,
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                handle_text_message(
                    &text,
                    &state,
                    &mut client_subs,
                    &mut forwarder_handles,
                    &ws_sender,
                )
                .await;
            }
            Message::Close(_) => {
                debug!("Client sent close frame");
                break;
            }
            Message::Ping(data) => {
                let mut sender = ws_sender.lock().await;
                let _ = sender.send(Message::Pong(data)).await;
            }
            _ => {}
        }
    }

    // Client disconnected — abort all forwarder tasks.
    for (topic, handle) in forwarder_handles.drain() {
        debug!("Aborting forwarder for topic: {}", topic);
        handle.abort();
    }
    debug!("WebSocket client disconnected");
}

/// Process a text message from the client.
async fn handle_text_message(
    text: &str,
    state: &Arc<WsState>,
    client_subs: &mut ClientSubscriptions,
    forwarder_handles: &mut std::collections::HashMap<
        SubscriptionTopic,
        tokio::task::JoinHandle<()>,
    >,
    ws_sender: &Arc<tokio::sync::Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
) {
    let client_msg: ClientMessage = match serde_json::from_str(text) {
        Ok(msg) => msg,
        Err(e) => {
            let error_msg = ServerMessage::Error {
                message: format!("Invalid message: {}", e),
            };
            let mut sender = ws_sender.lock().await;
            let _ = sender
                .send(Message::Text(serde_json::to_string(&error_msg).unwrap()))
                .await;
            return;
        }
    };

    match client_msg {
        ClientMessage::Subscribe { topic, filter } => {
            // If already subscribed, abort the old forwarder first.
            if let Some(handle) = forwarder_handles.remove(&topic) {
                handle.abort();
            }

            client_subs.subscribe(topic.clone(), filter.clone());

            // Spawn a forwarder task for this topic.
            let rx = state.sender_for(&topic).subscribe();
            let sender = ws_sender.clone();
            let sub_topic = topic.clone();
            let sub_filter = filter;

            let handle = tokio::spawn(async move {
                forward_events(rx, sender, sub_topic, sub_filter).await;
            });
            forwarder_handles.insert(topic.clone(), handle);

            // Send ack.
            let ack = ServerMessage::Ack {
                action: "subscribe".to_string(),
                topic: topic.clone(),
            };
            let mut sender = ws_sender.lock().await;
            let _ = sender
                .send(Message::Text(serde_json::to_string(&ack).unwrap()))
                .await;

            debug!("Client subscribed to {}", topic);
        }
        ClientMessage::Unsubscribe { topic } => {
            let was_subscribed = client_subs.unsubscribe(&topic);
            if let Some(handle) = forwarder_handles.remove(&topic) {
                handle.abort();
            }

            let ack = if was_subscribed {
                ServerMessage::Ack {
                    action: "unsubscribe".to_string(),
                    topic: topic.clone(),
                }
            } else {
                ServerMessage::Error {
                    message: format!("Not subscribed to {}", topic),
                }
            };
            let mut sender = ws_sender.lock().await;
            let _ = sender
                .send(Message::Text(serde_json::to_string(&ack).unwrap()))
                .await;

            debug!("Client unsubscribed from {}", topic);
        }
        ClientMessage::Ping => {
            let pong = ServerMessage::Pong;
            let mut sender = ws_sender.lock().await;
            let _ = sender
                .send(Message::Text(serde_json::to_string(&pong).unwrap()))
                .await;
        }
    }
}

/// Forwarder task: reads events from a broadcast receiver and sends matching
/// ones to the WebSocket client.
async fn forward_events(
    mut rx: broadcast::Receiver<String>,
    ws_sender: Arc<tokio::sync::Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    topic: SubscriptionTopic,
    filter: Option<crate::subscriptions::SubscriptionFilter>,
) {
    // Build a single-topic ClientSubscriptions to reuse `matches_filter`.
    let mut matcher = ClientSubscriptions::new();
    matcher.subscribe(topic.clone(), filter);

    loop {
        match rx.recv().await {
            Ok(event_str) => {
                // Parse the event to check the filter.
                let should_send = match serde_json::from_str::<SubscriptionEvent>(&event_str) {
                    Ok(event) => matcher.matches_filter(&topic, &event),
                    Err(_) => true, // If we can't parse, send it anyway.
                };

                if should_send {
                    let mut sender = ws_sender.lock().await;
                    if sender.send(Message::Text(event_str)).await.is_err() {
                        break;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Client lagged behind by {} events on topic {}", n, topic);
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed for topic {}", topic);
                break;
            }
        }
    }
}

/// Helper to get the current UNIX timestamp in seconds.
fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
