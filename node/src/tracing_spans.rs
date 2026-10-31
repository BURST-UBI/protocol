//! Optional span constructors. The running node does not currently call these
//! helpers; they neither instrument operations nor install an exporter themselves.

use tracing::{info_span, Span};

/// Construct a block-processing span tagged with its hash.
pub fn block_process_span(block_hash: &str) -> Span {
    info_span!("block_process", hash = %block_hash)
}

/// Construct a block-validation span tagged with its hash.
pub fn block_validate_span(block_hash: &str) -> Span {
    info_span!("block_validate", hash = %block_hash)
}

/// Construct a vote span tagged with an election identifier.
pub fn vote_span(election_id: &str) -> Span {
    info_span!("vote", election = %election_id)
}

/// Construct an inbound-message span tagged with peer and message type.
pub fn network_recv_span(peer: &str, msg_type: &str) -> Span {
    info_span!("network_recv", peer = %peer, msg_type = %msg_type)
}

/// Construct a broadcast span tagged with message type and peer count.
pub fn broadcast_span(msg_type: &str, peer_count: usize) -> Span {
    info_span!("broadcast", msg_type = %msg_type, peer_count = %peer_count)
}

/// Construct an RPC span tagged with its action name.
pub fn rpc_span(action: &str) -> Span {
    info_span!("rpc", action = %action)
}
