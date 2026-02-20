//! TCP connection management.
//!
//! `PeerConnection` wraps a `TcpStream` with length-prefixed (4-byte big-endian)
//! send/recv. `ConnectionPool` enforces a maximum number of simultaneous
//! connections (default 64).

use std::collections::HashMap;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::NetworkError;

/// Default maximum number of simultaneous peer connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;

/// Maximum message body size accepted on a connection (same as protocol codec limit).
const MAX_BODY_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

/// A live TCP connection to a peer.
pub struct PeerConnection {
    pub peer_id: String,
    pub address: String,
    pub stream: TcpStream,
    pub connected_at_secs: u64,
}

impl PeerConnection {
    /// Open a new TCP connection to the given address (e.g. "127.0.0.1:7076").
    pub async fn connect(address: &str) -> Result<Self, NetworkError> {
        let stream = TcpStream::connect(address)
            .await
            .map_err(|e| NetworkError::ConnectionFailed(format!("{address}: {e}")))?;

        let connected_at_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Self {
            peer_id: String::new(), // assigned after handshake
            address: address.to_string(),
            stream,
            connected_at_secs,
        })
    }

    /// Wrap an already-accepted `TcpStream` (for inbound connections).
    pub fn from_stream(stream: TcpStream, address: String) -> Self {
        let connected_at_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            peer_id: String::new(),
            address,
            stream,
            connected_at_secs,
        }
    }

    /// Send a length-prefixed message over the connection.
    ///
    /// Wire format: `[4-byte big-endian body length][body bytes]`
    pub async fn send(&mut self, data: &[u8]) -> Result<(), NetworkError> {
        if data.len() > MAX_BODY_SIZE {
            return Err(NetworkError::ConnectionFailed(format!(
                "message too large: {} > {}",
                data.len(),
                MAX_BODY_SIZE
            )));
        }

        let len_bytes = (data.len() as u32).to_be_bytes();
        self.stream
            .write_all(&len_bytes)
            .await
            .map_err(|e| NetworkError::Io(e.to_string()))?;
        self.stream
            .write_all(data)
            .await
            .map_err(|e| NetworkError::Io(e.to_string()))?;
        self.stream
            .flush()
            .await
            .map_err(|e| NetworkError::Io(e.to_string()))?;
        Ok(())
    }

    /// Receive a length-prefixed message from the connection.
    ///
    /// Reads a 4-byte big-endian length, then reads exactly that many bytes.
    pub async fn recv(&mut self) -> Result<Vec<u8>, NetworkError> {
        let mut len_buf = [0u8; 4];
        self.stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| NetworkError::Io(e.to_string()))?;

        let body_len = u32::from_be_bytes(len_buf) as usize;
        if body_len > MAX_BODY_SIZE {
            return Err(NetworkError::ConnectionFailed(format!(
                "peer sent oversized message: {} > {}",
                body_len, MAX_BODY_SIZE
            )));
        }

        let mut body = vec![0u8; body_len];
        self.stream
            .read_exact(&mut body)
            .await
            .map_err(|e| NetworkError::Io(e.to_string()))?;

        Ok(body)
    }
}

/// Manages a pool of active peer connections, enforcing a maximum count.
pub struct ConnectionPool {
    connections: HashMap<String, PeerConnection>,
    max_connections: usize,
}

impl ConnectionPool {
    /// Create a new pool with the given maximum connection count.
    pub fn new(max_connections: usize) -> Self {
        Self {
            connections: HashMap::new(),
            max_connections,
        }
    }

    /// Create a pool with the default maximum of [`DEFAULT_MAX_CONNECTIONS`] (64).
    pub fn with_default_max() -> Self {
        Self::new(DEFAULT_MAX_CONNECTIONS)
    }

    /// Add a connection to the pool. Returns an error if the pool is full.
    pub fn add(&mut self, conn: PeerConnection) -> Result<(), NetworkError> {
        if self.connections.len() >= self.max_connections {
            return Err(NetworkError::ConnectionFailed(format!(
                "connection pool full ({}/{})",
                self.connections.len(),
                self.max_connections
            )));
        }
        let key = if conn.peer_id.is_empty() {
            conn.address.clone()
        } else {
            conn.peer_id.clone()
        };
        self.connections.insert(key, conn);
        Ok(())
    }

    /// Check whether the pool can accept a new connection.
    pub fn is_full(&self) -> bool {
        self.connections.len() >= self.max_connections
    }

    /// Remove a peer connection by its identifier (peer_id or address).
    pub fn remove(&mut self, peer_id: &str) -> Option<PeerConnection> {
        self.connections.remove(peer_id)
    }

    /// Get a mutable reference to a connection by its identifier.
    pub fn get_mut(&mut self, peer_id: &str) -> Option<&mut PeerConnection> {
        self.connections.get_mut(peer_id)
    }

    /// Get an immutable reference to a connection by its identifier.
    pub fn get(&self, peer_id: &str) -> Option<&PeerConnection> {
        self.connections.get(peer_id)
    }

    /// Number of active connections.
    pub fn count(&self) -> usize {
        self.connections.len()
    }

    /// Maximum number of connections this pool will hold.
    pub fn max(&self) -> usize {
        self.max_connections
    }

    /// Iterate over all peer IDs in the pool.
    pub fn peer_ids(&self) -> impl Iterator<Item = &String> {
        self.connections.keys()
    }

    /// Broadcast raw data to all connected peers. Collects errors per-peer.
    pub async fn broadcast(&mut self, data: &[u8]) -> Vec<(String, NetworkError)> {
        let mut errors = Vec::new();
        for (id, conn) in self.connections.iter_mut() {
            if let Err(e) = conn.send(data).await {
                errors.push((id.clone(), e));
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn test_pool_max_connections() {
        let pool = ConnectionPool::new(2);
        assert!(!pool.is_full());
        assert_eq!(pool.count(), 0);
        assert_eq!(pool.max(), 2);
    }

    #[test]
    fn test_pool_default_max() {
        let pool = ConnectionPool::with_default_max();
        assert_eq!(pool.max(), DEFAULT_MAX_CONNECTIONS);
        assert_eq!(pool.max(), 64);
    }

    #[tokio::test]
    async fn test_peer_connection_send_recv() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, peer_addr) = listener.accept().await.unwrap();
            let mut conn = PeerConnection::from_stream(stream, peer_addr.to_string());
            let data = conn.recv().await.unwrap();
            conn.send(&data).await.unwrap();
            data
        });

        let mut client = PeerConnection::connect(&addr.to_string()).await.unwrap();
        let payload = b"hello burst network";
        client.send(payload).await.unwrap();
        let echo = client.recv().await.unwrap();
        assert_eq!(echo, payload);

        let server_saw = server.await.unwrap();
        assert_eq!(server_saw, payload);
    }

    #[tokio::test]
    async fn test_pool_add_remove() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept in background so connect succeeds.
        let accept_handle = tokio::spawn(async move {
            let _ = listener.accept().await.unwrap();
        });

        let conn = PeerConnection::connect(&addr.to_string()).await.unwrap();
        let mut pool = ConnectionPool::new(2);

        let key = conn.address.clone();
        pool.add(conn).unwrap();
        assert_eq!(pool.count(), 1);

        assert!(pool.get(&key).is_some());
        pool.remove(&key);
        assert_eq!(pool.count(), 0);

        accept_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_pool_rejects_when_full() {
        let listener1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr().unwrap();
        let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();

        let h1 = tokio::spawn(async move { listener1.accept().await.unwrap() });
        let h2 = tokio::spawn(async move { listener2.accept().await.unwrap() });

        let conn1 = PeerConnection::connect(&addr1.to_string()).await.unwrap();
        let conn2 = PeerConnection::connect(&addr2.to_string()).await.unwrap();

        let mut pool = ConnectionPool::new(1); // only 1 slot
        pool.add(conn1).unwrap();
        assert!(pool.is_full());

        let result = pool.add(conn2);
        assert!(result.is_err());
        assert_eq!(pool.count(), 1);

        h1.await.unwrap();
        h2.await.unwrap();
    }
}
