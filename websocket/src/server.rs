//! WebSocket server implementation.

pub struct WebSocketServer {
    pub port: u16,
}

impl WebSocketServer {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        todo!("start axum server with WebSocket upgrade endpoint")
    }
}
