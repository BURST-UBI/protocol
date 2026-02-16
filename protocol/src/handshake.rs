//! Node handshake protocol.

use burst_types::PublicKey;

/// Perform a handshake with a peer to verify node identity and protocol version.
pub async fn perform_handshake(
    _our_node_id: &PublicKey,
    _peer_version: u16,
) -> Result<PeerInfo, super::ProtocolError> {
    todo!("exchange NodeIdHandshake messages, verify signatures, check version")
}

/// Information about a peer after successful handshake.
pub struct PeerInfo {
    pub node_id: PublicKey,
    pub protocol_version: u16,
    pub network_id: burst_types::NetworkId,
}
