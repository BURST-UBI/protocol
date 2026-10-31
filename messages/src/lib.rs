//! Live peer envelopes, payloads, Protobuf framing and stateless bounds.

use burst_types::{BlockHash, Signature, WalletAddress};
use serde::{Deserialize, Serialize};

pub mod bootstrap;

pub use burst_types::PeerAddress;

/// A vote broadcast on the network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireVote {
    pub voter: WalletAddress,
    pub block_hashes: Vec<BlockHash>,
    pub is_final: bool,
    pub timestamp: u64,
    pub sequence: u64,
    pub signature: Signature,
}

/// Request votes for specific block hashes (confirm_req).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmReqMsg {
    pub block_hashes: Vec<BlockHash>,
    pub remaining_hops: u8,
}

/// Vote response to a confirm_req (confirm_ack).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmAckMsg {
    pub vote: WireVote,
}

/// Keepalive message carrying a list of known peer addresses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeepaliveMsg {
    pub peers: Vec<String>,
}

/// Legacy verification request. The node logs it without admitting an endorsement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationRequestMessage {
    pub target: WalletAddress,
    pub endorser: WalletAddress,
}

/// Legacy verification vote. The node logs it without recording a verifier vote.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationVoteMessage {
    pub target: WalletAddress,
    pub voter: WalletAddress,
    /// 0 = Legitimate, 1 = Illegitimate, 2 = Neither
    pub vote: u8,
    pub signature: Vec<u8>,
}

/// Legacy proposal announcement. The node logs it without admitting a proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceProposalMessage {
    pub proposal_hash: BlockHash,
    pub proposer: WalletAddress,
    pub content: Vec<u8>,
}

/// Legacy governance vote. The node logs it without recording a ballot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceVoteMessage {
    pub proposal_hash: BlockHash,
    pub voter: WalletAddress,
    /// 0 = Yea, 1 = Nay, 2 = Abstain
    pub vote: u8,
}

/// Peer-reported node statistics. These values are not consensus evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryAckMessage {
    pub block_count: u64,
    pub cemented_count: u64,
    pub unchecked_count: u64,
    pub account_count: u64,
    pub bandwidth_cap: u64,
    pub peer_count: u32,
    pub protocol_version: u8,
    pub uptime: u64,
    pub genesis_hash: BlockHash,
    pub major_version: u8,
    pub minor_version: u8,
    pub patch_version: u8,
    pub timestamp: u64,
    /// Deterministic hash of the node's current ProtocolParams.
    #[serde(default)]
    pub params_hash: BlockHash,
}

pub mod checkpoint_sync;
pub mod checkpoint_transport;
pub mod governance_content;
pub mod wire_message;
pub use wire_message::{HandshakeMsg, WireMessage};
