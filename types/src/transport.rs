//! Low-level transport values shared without ledger or server dependencies.
use serde::{Deserialize, Serialize};

/// A peer's network address.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerAddress {
    pub ip: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrafficClass {
    Control,
    Bulk,
}
