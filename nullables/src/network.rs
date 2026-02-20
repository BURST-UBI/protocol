//! Nullable network — record messages without sending them.
//!
//! Thread-safe: uses Mutex so it can be shared across async tasks.

use std::sync::Mutex;

/// A test network that records messages instead of sending them.
/// Thread-safe for use with tokio's multi-threaded runtime.
pub struct NullNetwork {
    /// All messages "sent" by the node.
    sent_messages: Mutex<Vec<Vec<u8>>>,
    /// Messages to deliver on the next "receive" call.
    inbox: Mutex<Vec<Vec<u8>>>,
}

impl NullNetwork {
    pub fn new() -> Self {
        Self {
            sent_messages: Mutex::new(Vec::new()),
            inbox: Mutex::new(Vec::new()),
        }
    }

    /// Record a message as "sent".
    pub fn send(&self, message: Vec<u8>) {
        self.sent_messages.lock().unwrap().push(message);
    }

    /// Enqueue a message for the node to "receive".
    pub fn enqueue(&self, message: Vec<u8>) {
        self.inbox.lock().unwrap().push(message);
    }

    /// Receive the next message from the inbox.
    pub fn receive(&self) -> Option<Vec<u8>> {
        let mut inbox = self.inbox.lock().unwrap();
        if inbox.is_empty() {
            None
        } else {
            Some(inbox.remove(0))
        }
    }

    /// Get all sent messages (for assertions).
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.sent_messages.lock().unwrap().clone()
    }

    /// Get count of sent messages.
    pub fn sent_count(&self) -> usize {
        self.sent_messages.lock().unwrap().len()
    }

    /// Clear all state.
    pub fn reset(&self) {
        self.sent_messages.lock().unwrap().clear();
        self.inbox.lock().unwrap().clear();
    }
}

impl Default for NullNetwork {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_and_receive() {
        let net = NullNetwork::new();
        net.enqueue(vec![1, 2, 3]);
        net.enqueue(vec![4, 5, 6]);
        assert_eq!(net.receive(), Some(vec![1, 2, 3]));
        assert_eq!(net.receive(), Some(vec![4, 5, 6]));
        assert_eq!(net.receive(), None);
    }

    #[test]
    fn test_sent_messages_tracked() {
        let net = NullNetwork::new();
        net.send(vec![10, 20]);
        net.send(vec![30, 40]);
        assert_eq!(net.sent_count(), 2);
        assert_eq!(net.sent(), vec![vec![10, 20], vec![30, 40]]);
    }

    #[test]
    fn test_reset() {
        let net = NullNetwork::new();
        net.send(vec![1]);
        net.enqueue(vec![2]);
        net.reset();
        assert_eq!(net.sent_count(), 0);
        assert_eq!(net.receive(), None);
    }
}
