//! Protocol version management.

/// Current protocol version.
pub const PROTOCOL_VERSION: u16 = burst_types::PROTOCOL_VERSION;

/// Minimum supported protocol version.
pub const MIN_PROTOCOL_VERSION: u16 = PROTOCOL_VERSION;

/// Check if a peer's protocol version is compatible.
pub fn is_compatible(peer_version: u16) -> bool {
    peer_version == PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_current_development_protocol_is_compatible() {
        for older in 0..PROTOCOL_VERSION {
            assert!(!is_compatible(older));
        }
        assert!(is_compatible(PROTOCOL_VERSION));
        assert!(!is_compatible(PROTOCOL_VERSION + 1));
    }
}
