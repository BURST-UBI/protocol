//! TCP connection management.

use crate::NetworkError;

/// A connection to a peer.
pub struct Connection {
    pub peer_id: String,
    pub address: String,
    pub connected_at_secs: u64,
}

/// Connection pool — manages multiple peer connections.
pub struct ConnectionPool {
    connections: Vec<Connection>,
    max_connections: usize,
}

impl ConnectionPool {
    pub fn new(max_connections: usize) -> Self {
        Self {
            connections: Vec::new(),
            max_connections,
        }
    }

    /// Add a new connection.
    pub fn add(&mut self, conn: Connection) -> Result<(), NetworkError> {
        if self.connections.len() >= self.max_connections {
            return Err(NetworkError::ConnectionFailed("pool full".into()));
        }
        self.connections.push(conn);
        Ok(())
    }

    /// Remove a disconnected peer.
    pub fn remove(&mut self, peer_id: &str) {
        self.connections.retain(|c| c.peer_id != peer_id);
    }

    /// Number of active connections.
    pub fn count(&self) -> usize {
        self.connections.len()
    }
}
