//! Pre-built [`tracing::Span`] constructors for common BURST node operations.
//!
//! Using consistent span names and field sets across the codebase makes it
//! easy to filter, search, and correlate traces in Jaeger / Grafana Tempo /
//! any OpenTelemetry-compatible backend.

use tracing::{info_span, Span};

/// Span covering the full block-processing pipeline for a single block.
pub fn block_process_span(block_hash: &str) -> Span {
    info_span!("block_process", hash = %block_hash)
}

/// Span covering the validation phase of a single block.
pub fn block_validate_span(block_hash: &str) -> Span {
    info_span!("block_validate", hash = %block_hash)
}

/// Span covering a consensus vote on an election.
pub fn vote_span(election_id: &str) -> Span {
    info_span!("vote", election = %election_id)
}

/// Span covering the handling of a single inbound network message.
pub fn network_recv_span(peer: &str, msg_type: &str) -> Span {
    info_span!("network_recv", peer = %peer, msg_type = %msg_type)
}

/// Span covering the broadcast of a message to connected peers.
pub fn broadcast_span(msg_type: &str, peer_count: usize) -> Span {
    info_span!("broadcast", msg_type = %msg_type, peer_count = %peer_count)
}

/// Span covering a single JSON-RPC action handled by the RPC server.
pub fn rpc_span(action: &str) -> Span {
    info_span!("rpc", action = %action)
}
