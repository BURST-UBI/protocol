//! Events emitted during block processing for subscribers.

use burst_types::{BlockHash, WalletAddress};

/// Ledger-level events that observers can subscribe to via the [`EventBus`].
#[derive(Clone, Debug)]
pub enum LedgerEvent {
    /// A block was accepted and added to the ledger.
    BlockConfirmed {
        hash: BlockHash,
        account: WalletAddress,
    },
    /// A block was rejected.
    BlockRejected {
        hash: BlockHash,
        reason: String,
    },
    /// A fork was detected.
    ForkDetected {
        account: WalletAddress,
        existing: BlockHash,
        incoming: BlockHash,
    },
    /// A gap block was queued for later processing.
    BlockQueued {
        hash: BlockHash,
        dependency: BlockHash,
    },
    /// An account was created (first block).
    AccountCreated {
        address: WalletAddress,
    },
    /// A TRST transfer was processed.
    TrstTransfer {
        from: WalletAddress,
        to: WalletAddress,
        amount: u128,
    },
    /// BRN was burned to create TRST.
    BrnBurned {
        burner: WalletAddress,
        receiver: WalletAddress,
        amount: u128,
    },
}

/// Synchronous fan-out event bus for ledger events.
///
/// Listeners are invoked inline on the emitting thread; keep handlers fast to
/// avoid stalling block processing.
pub struct EventBus {
    listeners: Vec<Box<dyn Fn(&LedgerEvent) + Send + Sync>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    pub fn subscribe(&mut self, listener: Box<dyn Fn(&LedgerEvent) + Send + Sync>) {
        self.listeners.push(listener);
    }

    pub fn emit(&self, event: &LedgerEvent) {
        for listener in &self.listeners {
            listener(event);
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    fn test_account() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    #[test]
    fn emit_calls_all_listeners() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();

        let c1 = Arc::clone(&counter);
        bus.subscribe(Box::new(move |_| {
            c1.fetch_add(1, Ordering::SeqCst);
        }));

        let c2 = Arc::clone(&counter);
        bus.subscribe(Box::new(move |_| {
            c2.fetch_add(10, Ordering::SeqCst);
        }));

        let event = LedgerEvent::BlockConfirmed {
            hash: BlockHash::ZERO,
            account: test_account(),
        };
        bus.emit(&event);

        assert_eq!(counter.load(Ordering::SeqCst), 11);
    }

    #[test]
    fn emit_with_no_listeners_is_noop() {
        let bus = EventBus::new();
        let event = LedgerEvent::BlockRejected {
            hash: BlockHash::ZERO,
            reason: "test".into(),
        };
        bus.emit(&event); // should not panic
    }

    #[test]
    fn listener_receives_correct_event_variant() {
        let saw_confirmed = Arc::new(AtomicUsize::new(0));
        let saw_rejected = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();

        let sc = Arc::clone(&saw_confirmed);
        let sr = Arc::clone(&saw_rejected);
        bus.subscribe(Box::new(move |event| {
            match event {
                LedgerEvent::BlockConfirmed { .. } => {
                    sc.fetch_add(1, Ordering::SeqCst);
                }
                LedgerEvent::BlockRejected { .. } => {
                    sr.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }));

        bus.emit(&LedgerEvent::BlockConfirmed {
            hash: BlockHash::ZERO,
            account: test_account(),
        });
        bus.emit(&LedgerEvent::BlockRejected {
            hash: BlockHash::ZERO,
            reason: "bad".into(),
        });

        assert_eq!(saw_confirmed.load(Ordering::SeqCst), 1);
        assert_eq!(saw_rejected.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn default_creates_empty_bus() {
        let bus = EventBus::default();
        assert!(bus.listeners.is_empty());
    }
}
