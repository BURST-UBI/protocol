// Protobuf model for protocol/wire.proto. No ledger or signature encoding lives here.

#[derive(Clone, PartialEq, prost::Message)]
pub struct BytesPayload {
    #[prost(bytes = "vec", tag = "1")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Vote {
    #[prost(string, tag = "1")]
    pub voter: String,
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub block_hashes: Vec<Vec<u8>>,
    #[prost(bool, tag = "3")]
    pub is_final: bool,
    #[prost(uint64, tag = "4")]
    pub timestamp: u64,
    #[prost(uint64, tag = "5")]
    pub sequence: u64,
    #[prost(bytes = "vec", tag = "6")]
    pub signature: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfirmRequest {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub block_hashes: Vec<Vec<u8>>,
    #[prost(uint32, tag = "2")]
    pub remaining_hops: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Keepalive {
    #[prost(string, repeated, tag = "1")]
    pub peers: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Handshake {
    #[prost(string, tag = "1")]
    pub node_id: String,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub cookie: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub cookie_signature: Option<Vec<u8>>,
    #[prost(bytes = "vec", tag = "4")]
    pub params_hash: Vec<u8>,
    #[prost(uint32, tag = "5")]
    pub network: u32,
    #[prost(uint32, tag = "6")]
    pub peering_port: u32,
    #[prost(uint32, tag = "7")]
    pub block_version: u32,
    #[prost(uint32, tag = "8")]
    pub wire_version: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VerificationRequest {
    #[prost(string, tag = "1")]
    pub target: String,
    #[prost(string, tag = "2")]
    pub endorser: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VerificationVote {
    #[prost(string, tag = "1")]
    pub target: String,
    #[prost(string, tag = "2")]
    pub voter: String,
    #[prost(uint32, tag = "3")]
    pub vote: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub signature: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GovernanceProposal {
    #[prost(bytes = "vec", tag = "1")]
    pub proposal_hash: Vec<u8>,
    #[prost(string, tag = "2")]
    pub proposer: String,
    #[prost(bytes = "vec", tag = "3")]
    pub content: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GovernanceVote {
    #[prost(bytes = "vec", tag = "1")]
    pub proposal_hash: Vec<u8>,
    #[prost(string, tag = "2")]
    pub voter: String,
    #[prost(uint32, tag = "3")]
    pub vote: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Telemetry {
    #[prost(uint64, tag = "1")]
    pub block_count: u64,
    #[prost(uint64, tag = "2")]
    pub cemented_count: u64,
    #[prost(uint64, tag = "3")]
    pub unchecked_count: u64,
    #[prost(uint64, tag = "4")]
    pub account_count: u64,
    #[prost(uint64, tag = "5")]
    pub bandwidth_cap: u64,
    #[prost(uint32, tag = "6")]
    pub peer_count: u32,
    #[prost(uint32, tag = "7")]
    pub protocol_version: u32,
    #[prost(uint64, tag = "8")]
    pub uptime: u64,
    #[prost(bytes = "vec", tag = "9")]
    pub genesis_hash: Vec<u8>,
    #[prost(uint32, tag = "10")]
    pub major_version: u32,
    #[prost(uint32, tag = "11")]
    pub minor_version: u32,
    #[prost(uint32, tag = "12")]
    pub patch_version: u32,
    #[prost(uint64, tag = "13")]
    pub timestamp: u64,
    #[prost(bytes = "vec", tag = "14")]
    pub params_hash: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct BlocksRequest {
    #[prost(string, tag = "1")]
    pub account: String,
    #[prost(bytes = "vec", tag = "2")]
    pub start: Vec<u8>,
    #[prost(uint32, tag = "3")]
    pub count: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AccountRequest {
    #[prost(string, tag = "1")]
    pub account: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FrontiersRequest {
    #[prost(string, tag = "1")]
    pub start_account: String,
    #[prost(uint32, tag = "2")]
    pub count: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct BlocksResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub blocks: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AccountResponse {
    #[prost(string, tag = "1")]
    pub account: String,
    #[prost(bytes = "vec", tag = "2")]
    pub head: Vec<u8>,
    #[prost(uint64, tag = "3")]
    pub block_count: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Frontier {
    #[prost(string, tag = "1")]
    pub account: String,
    #[prost(bytes = "vec", tag = "2")]
    pub head: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FrontiersResponse {
    #[prost(message, repeated, tag = "1")]
    pub entries: Vec<Frontier>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncRequest {
    #[prost(uint64, tag = "1")]
    pub height: u64,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub digest: Option<Vec<u8>>,
    #[prost(bool, tag = "4")]
    pub record_only: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncChunk {
    #[prost(uint64, tag = "1")]
    pub height: u64,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
    #[prost(uint32, tag = "3")]
    pub total: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub digest: Vec<u8>,
    #[prost(bytes = "vec", tag = "5")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AgreementContext {
    #[prost(uint32, tag = "1")]
    pub network: u32,
    #[prost(bytes = "vec", tag = "2")]
    pub genesis: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    pub parent: Vec<u8>,
    #[prost(uint64, tag = "4")]
    pub height: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Offer {
    #[prost(bytes = "vec", tag = "1")]
    pub digest: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub total: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ChunkRequest {
    #[prost(bytes = "vec", tag = "1")]
    pub digest: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Chunk {
    #[prost(bytes = "vec", tag = "1")]
    pub digest: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub total: u32,
    #[prost(uint32, tag = "3")]
    pub offset: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ContentRequest {
    #[prost(bytes = "vec", tag = "1")]
    pub proposal: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ContentChunk {
    #[prost(bytes = "vec", tag = "1")]
    pub proposal: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub hash: Vec<u8>,
    #[prost(uint32, tag = "3")]
    pub length: u32,
    #[prost(uint32, tag = "4")]
    pub offset: u32,
    #[prost(bytes = "vec", tag = "5")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    #[prost(uint32, tag = "1")]
    pub wire_version: u32,
    #[prost(
        oneof = "envelope::Body",
        tags = "10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25"
    )]
    pub body: Option<envelope::Body>,
}
pub mod envelope {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "10")]
        Block(super::BytesPayload),
        #[prost(message, tag = "11")]
        Vote(super::Vote),
        #[prost(message, tag = "12")]
        ConfirmReq(super::ConfirmRequest),
        #[prost(message, tag = "13")]
        ConfirmAck(super::Vote),
        #[prost(message, tag = "14")]
        Keepalive(super::Keepalive),
        #[prost(message, tag = "15")]
        Bootstrap(super::Bootstrap),
        #[prost(message, tag = "16")]
        Handshake(super::Handshake),
        #[prost(message, tag = "17")]
        VerificationRequest(super::VerificationRequest),
        #[prost(message, tag = "18")]
        VerificationVote(super::VerificationVote),
        #[prost(message, tag = "19")]
        GovernanceProposal(super::GovernanceProposal),
        #[prost(message, tag = "20")]
        GovernanceVote(super::GovernanceVote),
        #[prost(message, tag = "21")]
        TelemetryReq(super::Empty),
        #[prost(message, tag = "22")]
        TelemetryAck(super::Telemetry),
        #[prost(message, tag = "23")]
        CheckpointSync(super::CheckpointSync),
        #[prost(message, tag = "24")]
        CheckpointAgreement(super::Agreement),
        #[prost(message, tag = "25")]
        GovernanceContent(super::Content),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Bootstrap {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(oneof = "bootstrap::Body", tags = "10, 11, 12, 13, 14, 15")]
    pub body: Option<bootstrap::Body>,
}
pub mod bootstrap {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "10")]
        BlocksRequest(super::BlocksRequest),
        #[prost(message, tag = "11")]
        AccountRequest(super::AccountRequest),
        #[prost(message, tag = "12")]
        FrontiersRequest(super::FrontiersRequest),
        #[prost(message, tag = "13")]
        BlocksResponse(super::BlocksResponse),
        #[prost(message, tag = "14")]
        AccountResponse(super::AccountResponse),
        #[prost(message, tag = "15")]
        FrontiersResponse(super::FrontiersResponse),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct CheckpointSync {
    #[prost(oneof = "checkpoint_sync::Body", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11")]
    pub body: Option<checkpoint_sync::Body>,
}
pub mod checkpoint_sync {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Request(super::SyncRequest),
        #[prost(message, tag = "2")]
        Chunk(super::SyncChunk),
        #[prost(message, tag = "3")]
        SnapshotRequest(super::SnapshotRequest),
        #[prost(message, tag = "4")]
        SnapshotChunk(super::SnapshotChunk),
        #[prost(message, tag = "5")]
        SnapshotQuery(super::SnapshotQuery),
        #[prost(message, tag = "6")]
        SnapshotOffer(super::SnapshotOffer),
        #[prost(message, tag = "7")]
        WitnessRequest(super::WitnessRequest),
        #[prost(message, tag = "8")]
        WitnessChunk(super::WitnessChunk),
        #[prost(message, tag = "9")]
        WitnessHeadsRequest(super::WitnessHeadsRequest),
        #[prost(message, tag = "10")]
        TipRequest(super::TipRequest),
        #[prost(message, tag = "11")]
        TipObservation(super::TipObservation),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TipRequest {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TipObservation {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
    #[prost(uint64, tag = "2")]
    pub height: u64,
    #[prost(bytes = "vec", tag = "3")]
    pub epoch: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct WitnessRequest {
    #[prost(bytes = "vec", tag = "1")]
    pub start: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub digest: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct WitnessHeadsRequest {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub starts: Vec<Vec<u8>>,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub digest: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct WitnessChunk {
    #[prost(bytes = "vec", tag = "1")]
    pub start: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub offset: u32,
    #[prost(uint32, tag = "3")]
    pub total: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub digest: Vec<u8>,
    #[prost(bytes = "vec", tag = "5")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SnapshotRequest {
    #[prost(bytes = "vec", tag = "1")]
    pub epoch: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub offset: u64,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub digest: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SnapshotQuery {
    #[prost(uint64, tag = "1")]
    pub minimum_height: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SnapshotOffer {
    #[prost(bytes = "vec", tag = "1")]
    pub epoch: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub height: u64,
    #[prost(uint64, tag = "3")]
    pub total: u64,
    #[prost(bytes = "vec", tag = "4")]
    pub digest: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SnapshotChunk {
    #[prost(bytes = "vec", tag = "1")]
    pub epoch: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub offset: u64,
    #[prost(uint64, tag = "3")]
    pub total: u64,
    #[prost(bytes = "vec", tag = "4")]
    pub digest: Vec<u8>,
    #[prost(bytes = "vec", tag = "5")]
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Agreement {
    #[prost(message, optional, tag = "1")]
    pub context: Option<AgreementContext>,
    #[prost(oneof = "agreement::Body", tags = "10, 11, 12")]
    pub body: Option<agreement::Body>,
}
pub mod agreement {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "10")]
        Offer(super::Offer),
        #[prost(message, tag = "11")]
        Request(super::ChunkRequest),
        #[prost(message, tag = "12")]
        Chunk(super::Chunk),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Content {
    #[prost(oneof = "content::Body", tags = "1, 2")]
    pub body: Option<content::Body>,
}
pub mod content {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Request(super::ContentRequest),
        #[prost(message, tag = "2")]
        Chunk(super::ContentChunk),
    }
}
