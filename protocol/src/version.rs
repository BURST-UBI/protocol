//! Protocol version management.

/// Current protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Minimum supported protocol version.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// Check if a peer's protocol version is compatible.
pub fn is_compatible(peer_version: u16) -> bool {
    peer_version >= MIN_PROTOCOL_VERSION && peer_version <= PROTOCOL_VERSION
}
