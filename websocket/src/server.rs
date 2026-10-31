//! WebSocket server implementation.
//!
//! Accepts subscriptions at `/ws` and forwards matching broadcast events.
//! Delivery is transient: lag drops events without a client gap notification.
//! See the crate documentation for which topics the node actually publishes.

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use crate::subscriptions::{
    ClientMessage, ClientSubscriptions, ServerMessage, SubscriptionEvent, SubscriptionTopic,
};

pub use crate::state::WsState;

/// The WebSocket server, configured with a port and shared state.
pub struct WebSocketServer {
    pub port: u16,
    pub bind_address: String,
    pub state: Arc<WsState>,
}

struct Sessions {
    events: Arc<WsState>,
    stopping: watch::Receiver<bool>,
    active: watch::Sender<()>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl WebSocketServer {
    /// Create a new server with a default channel capacity of 256.
    pub fn new(port: u16) -> Self {
        Self::with_state(port, Arc::new(WsState::new(256)))
    }

    /// Create a new server with the provided shared state.
    pub fn with_state(port: u16, state: Arc<WsState>) -> Self {
        Self::with_bind(port, "127.0.0.1".into(), state)
    }

    /// Non-local event access requires an explicit bind address.
    pub fn with_bind(port: u16, bind_address: String, state: Arc<WsState>) -> Self {
        Self {
            port,
            bind_address,
            state,
        }
    }

    /// Shutdown closes upgraded sockets and joins their subscription forwarders.
    pub async fn start(
        &self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (stop, stopping) = watch::channel(false);
        let (active, initial) = watch::channel(());
        drop(initial);
        let sessions = Arc::new(Sessions {
            events: self.state.clone(),
            stopping,
            active,
            slots: Arc::new(tokio::sync::Semaphore::new(64)),
        });
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(sessions.clone());

        let listener =
            tokio::net::TcpListener::bind((self.bind_address.as_str(), self.port)).await?;
        info!(address = %listener.local_addr()?, "WebSocket server listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.await;
                stop.send_replace(true);
            })
            .await?;
        sessions.active.closed().await;
        Ok(())
    }
}

/// Axum handler that upgrades an HTTP request to a WebSocket connection.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(sessions): State<Arc<Sessions>>,
) -> impl IntoResponse {
    let Ok(permit) = sessions.slots.clone().try_acquire_owned() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "WebSocket capacity reached",
        )
            .into_response();
    };
    let active = sessions.active.subscribe();
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            let _active = active;
            handle_socket(socket, sessions.events.clone(), sessions.stopping.clone()).await;
        })
        .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: Arc<WsState>,
    mut stopping: watch::Receiver<bool>,
) {
    let (ws_sender, mut ws_receiver) = socket.split();

    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let mut client_subs = ClientSubscriptions::new();

    // Track spawned forwarder tasks so we can abort them on disconnect/unsubscribe.
    let mut forwarder_handles: std::collections::HashMap<
        SubscriptionTopic,
        tokio::task::JoinHandle<()>,
    > = std::collections::HashMap::new();

    debug!("New WebSocket client connected");

    let receive = async {
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
    };
    tokio::select! {
        biased;
        _ = stopping.wait_for(|stopped| *stopped) => {},
        () = receive => {},
    }

    // Disconnect and server shutdown both cancel and join topic forwarders.
    for (topic, handle) in forwarder_handles.drain() {
        debug!("Aborting forwarder for topic: {}", topic);
        handle.abort();
        let _ = handle.await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        ws_sender.lock().await.send(Message::Close(None)).await
    })
    .await;
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
                let _ = handle.await;
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

            // The forwarder is already running, so an event can precede this ack.
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
                let _ = handle.await;
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

/// Forward matching events as bare SubscriptionEvent JSON, without a `type` tag.
/// Lag is logged locally; the client receives no gap notification.
async fn forward_events(
    mut rx: broadcast::Receiver<Arc<SubscriptionEvent>>,
    ws_sender: Arc<tokio::sync::Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    topic: SubscriptionTopic,
    filter: Option<crate::subscriptions::SubscriptionFilter>,
) {
    // Build a single-topic ClientSubscriptions to reuse `matches_filter`.
    let mut matcher = ClientSubscriptions::new();
    matcher.subscribe(topic.clone(), filter);

    loop {
        match rx.recv().await {
            Ok(event) => {
                if matcher.matches_filter(&topic, &event) {
                    let event_str = match serde_json::to_string(event.as_ref()) {
                        Ok(encoded) => encoded,
                        Err(error) => {
                            warn!(%error, "cannot serialize subscription event");
                            continue;
                        }
                    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientFrame};

    #[tokio::test]
    async fn connection_capacity_recovers_and_oversized_messages_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (stop, stopping) = watch::channel(false);
        let (active, initial) = watch::channel(());
        drop(initial);
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let sessions = Arc::new(Sessions {
            events: Arc::new(WsState::new(8)),
            stopping: stopping.clone(),
            active,
            slots: slots.clone(),
        });
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(sessions);
        let worker = tokio::spawn(async move {
            let mut stopping = stopping;
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopping.wait_for(|value| *value).await;
                })
                .await
                .unwrap();
        });
        let (mut first, _) = connect_async(&url).await.unwrap();
        let rejected = connect_async(&url).await.unwrap_err();
        match rejected {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), 503)
            }
            error => panic!("unexpected admission failure: {error}"),
        }
        first.close(None).await.unwrap();
        drop(first);
        tokio::time::timeout(Duration::from_secs(3), async {
            while slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (mut second, _) = connect_async(&url).await.unwrap();
        second
            .send(ClientFrame::Text("x".repeat(64 * 1024 + 1).into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(3), second.next())
            .await
            .unwrap();
        assert!(matches!(
            reply,
            None | Some(Err(_)) | Some(Ok(ClientFrame::Close(_)))
        ));
        drop(second);
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn external_event_access_requires_explicit_opt_in() {
        assert_eq!(WebSocketServer::new(7078).bind_address, "127.0.0.1");
        let state = Arc::new(WsState::new(8));
        assert_eq!(
            WebSocketServer::with_state(7078, state.clone()).bind_address,
            "127.0.0.1"
        );
        assert_eq!(
            WebSocketServer::with_bind(7078, "::1".into(), state).bind_address,
            "::1"
        );
    }

    #[tokio::test]
    async fn shutdown_closes_sessions_and_reaps_subscription_forwarders() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        for generation in 0..2 {
            let state = Arc::new(WsState::new(8));
            let server = WebSocketServer::with_state(port, state.clone());
            let (shutdown, stop) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn(async move {
                server
                    .start(async move {
                        let _ = stop.await;
                    })
                    .await
                    .unwrap();
            });
            let mut client = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok((client, _)) =
                        connect_async(format!("ws://127.0.0.1:{port}/ws")).await
                    {
                        break client;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            client
                .send(ClientFrame::Text(
                    r#"{"action":"subscribe","topic":"account_update"}"#.into(),
                ))
                .await
                .unwrap();
            let ack = tokio::time::timeout(Duration::from_secs(1), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let ack: serde_json::Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
            assert_eq!(ack["type"], "ack");
            assert_eq!(state.account_update_tx.receiver_count(), 1);
            state.publish_account_update("fixture", &generation.to_string(), "balance");
            let event = tokio::time::timeout(Duration::from_secs(1), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let event: serde_json::Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
            assert_eq!(event["data"]["balance"], generation.to_string());
            shutdown.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(state.account_update_tx.receiver_count(), 0);
            let close = tokio::time::timeout(Duration::from_secs(1), client.next())
                .await
                .unwrap();
            assert!(matches!(close, Some(Ok(ClientFrame::Close(_))) | None));
            assert!(connect_async(format!("ws://127.0.0.1:{port}/ws"))
                .await
                .is_err());
        }
    }
}
