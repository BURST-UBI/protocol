//! P2P networking layer for the BURST protocol.
//!
//! Handles peer discovery, TCP connection management, message routing,
//! block/transaction propagation, sync, and clock synchronization.

pub mod clock_sync;
pub mod connection;
pub mod error;
pub mod peer_manager;
pub mod sync;

pub use clock_sync::ClockSync;
pub use error::NetworkError;
pub use peer_manager::PeerManager;
pub use sync::SyncProtocol;
