//! Network message types for BURST node-to-node communication.

use burst_transactions::Transaction;
use burst_types::{BlockHash, Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// Header present on every network message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageHeader {
    pub message_type: MessageType,
    pub network_id: burst_types::NetworkId,
    pub protocol_version: u16,
    pub timestamp: Timestamp,
}

/// All message types in the protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    // Block/transaction propagation
    Keepalive,
    Publish,
    ConfirmReq,
    ConfirmAck,

    // Sync
    FrontierReq,
    FrontierResp,
    BulkPull,
    BulkPush,

    // Verification
    VerificationRequest,
    VerificationVote,

    // Governance
    GovernanceProposal,
    GovernanceVote,

    // Handshake
    NodeIdHandshake,

    // Telemetry
    TelemetryReq,
    TelemetryAck,
}

/// A block/transaction publish message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishMessage {
    pub header: MessageHeader,
    pub transaction: Transaction,
}

/// Request confirmation of a block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmReqMessage {
    pub header: MessageHeader,
    pub block_hash: BlockHash,
}

/// Confirmation acknowledgment (representative vote).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmAckMessage {
    pub header: MessageHeader,
    pub representative: WalletAddress,
    pub block_hash: BlockHash,
    pub signature: burst_types::Signature,
}

/// Keepalive message with peer addresses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeepaliveMessage {
    pub header: MessageHeader,
    pub peers: Vec<PeerAddress>,
}

/// A peer's network address.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerAddress {
    pub ip: String,
    pub port: u16,
}

/// Frontier request — ask for account chain heads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrontierReqMessage {
    pub header: MessageHeader,
    pub start_account: WalletAddress,
    pub count: u32,
}

/// Frontier response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrontierRespMessage {
    pub header: MessageHeader,
    pub frontiers: Vec<(WalletAddress, BlockHash)>,
}

/// Bulk pull request — ask for blocks from an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BulkPullMessage {
    pub header: MessageHeader,
    pub account: WalletAddress,
    pub end_hash: BlockHash,
}

/// Node ID handshake for peer authentication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeIdHandshakeMessage {
    pub header: MessageHeader,
    pub node_id: burst_types::PublicKey,
    pub signature: burst_types::Signature,
}

/// Telemetry data shared between nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryData {
    pub block_count: u64,
    pub account_count: u64,
    pub peer_count: u32,
    pub protocol_version: u16,
    pub uptime_secs: u64,
    pub timestamp: Timestamp,
}
