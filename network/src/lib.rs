//! Peer, connection, traffic-budget, clock and port-mapping components.
//!
//! The node integrates transport, authenticated handshakes, routing and recovery.
//! A helper exported here does not by itself enforce the running node's policy.

pub mod auth;
pub mod admission;
pub mod bandwidth;
pub mod egress;
pub mod fair_queue;
pub mod broadcast;
pub mod clock_sync;
pub mod connection;
pub mod dedup;
pub mod error;
pub mod peer_manager;
pub mod syn_cookies;
pub mod trusted_clock;
pub mod upnp;

pub use auth::PeerAuth;
pub use broadcast::{BroadcastResult, Broadcaster};
pub use clock_sync::ClockSync;
pub use connection::{ConnectionPool, PeerConnection, DEFAULT_MAX_CONNECTIONS};
pub use dedup::{MessageDedup, DEFAULT_DEDUP_CAPACITY};
pub use error::NetworkError;
pub use peer_manager::{PeerManager, PeerState, PeerTelemetry, PenaltyReason};
pub use syn_cookies::SynCookies;
pub use upnp::{PortMapper, PortMapperConfig, UpnpState};

pub use bandwidth::DEFAULT_MAX_BYTES_PER_SEC;
