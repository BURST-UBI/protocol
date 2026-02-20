//! P2P networking layer for the BURST protocol.
//!
//! Handles peer discovery, TCP connection management, message routing,
//! block/transaction propagation, sync, and clock synchronization.

pub mod auth;
pub mod broadcast;
pub mod clock_sync;
pub mod connection;
pub mod dedup;
pub mod error;
pub mod peer_manager;
pub mod syn_cookies;
pub mod sync;
pub mod throttle;

pub use auth::PeerAuth;
pub use broadcast::{BroadcastResult, Broadcaster};
pub use clock_sync::ClockSync;
pub use connection::{ConnectionPool, PeerConnection, DEFAULT_MAX_CONNECTIONS};
pub use dedup::{MessageDedup, DEFAULT_DEDUP_CAPACITY};
pub use error::NetworkError;
pub use peer_manager::{PeerManager, PeerState, PeerTelemetry, PenaltyReason};
pub use syn_cookies::SynCookies;
pub use sync::{
    BootstrapResult, SyncAccountResult, SyncHandle, SyncProtocol, SyncRequest, SyncResponse,
};
pub use throttle::{BandwidthThrottle, DEFAULT_MAX_BYTES_PER_SEC};
