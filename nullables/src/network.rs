//! Nullable network — record messages without sending them.

use std::cell::RefCell;

/// A test network that records messages instead of sending them.
pub struct NullNetwork {
    /// All messages "sent" by the node.
    sent_messages: RefCell<Vec<Vec<u8>>>,
    /// Messages to deliver on the next "receive" call.
    inbox: RefCell<Vec<Vec<u8>>>,
}

impl NullNetwork {
    pub fn new() -> Self {
        Self {
            sent_messages: RefCell::new(Vec::new()),
            inbox: RefCell::new(Vec::new()),
        }
    }

    /// Record a message as "sent".
    pub fn send(&self, message: Vec<u8>) {
        self.sent_messages.borrow_mut().push(message);
    }

    /// Enqueue a message for the node to "receive".
    pub fn enqueue(&self, message: Vec<u8>) {
        self.inbox.borrow_mut().push(message);
    }

    /// Receive the next message from the inbox.
    pub fn receive(&self) -> Option<Vec<u8>> {
        let mut inbox = self.inbox.borrow_mut();
        if inbox.is_empty() {
            None
        } else {
            Some(inbox.remove(0))
        }
    }

    /// Get all sent messages (for assertions).
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.sent_messages.borrow().clone()
    }

    /// Clear all state.
    pub fn reset(&self) {
        self.sent_messages.borrow_mut().clear();
        self.inbox.borrow_mut().clear();
    }
}

impl Default for NullNetwork {
    fn default() -> Self {
        Self::new()
    }
}
