//! Typed Protobuf transport. Signed block/record bytes keep their own encoding.
use super::*;
use crate::bootstrap::{AscPullAckPayload as Ack, AscPullReqPayload as Req};
use crate::checkpoint_sync::CheckpointSyncMessage as Sync;
use crate::checkpoint_transport::{Body as AgreementBody, Context, Packet as AgreementPacket};
use crate::governance_content::Packet as Content;
use bincode::Options;
use burst_types::ProtocolError;
use prost::Message;

mod model;
use model as proto;

fn invalid(message: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::Malformed(message.to_string())
}
fn check_message_size(size: usize) -> Result<(), ProtocolError> {
    if size > burst_types::MAX_P2P_MESSAGE_BYTES {
        return Err(ProtocolError::MessageTooLarge {
            size,
            max: burst_types::MAX_P2P_MESSAGE_BYTES,
        });
    }
    Ok(())
}
fn fixed<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], ProtocolError> {
    bytes
        .try_into()
        .map_err(|_| invalid(format!("expected {N} bytes")))
}
fn hash(bytes: Vec<u8>) -> Result<BlockHash, ProtocolError> {
    Ok(BlockHash::new(fixed(bytes)?))
}
fn narrow<T: TryFrom<u32>>(value: u32) -> Result<T, ProtocolError> {
    T::try_from(value).map_err(|_| invalid("integer out of range"))
}
fn address(value: String) -> Result<WalletAddress, ProtocolError> {
    if value.len() > 128 {
        return Err(invalid("address too long"));
    }
    if !value.starts_with(WalletAddress::PREFIX) {
        return Err(invalid("address must start with brst_"));
    }
    Ok(WalletAddress::new(value))
}
fn network(value: u32) -> Result<NetworkId, ProtocolError> {
    match value {
        1 => Ok(NetworkId::Live),
        2 => Ok(NetworkId::Test),
        3 => Ok(NetworkId::Dev),
        _ => Err(invalid("unknown network")),
    }
}
fn network_number(value: NetworkId) -> u32 {
    match value {
        NetworkId::Live => 1,
        NetworkId::Test => 2,
        NetworkId::Dev => 3,
    }
}
fn vote(value: &WireVote) -> proto::Vote {
    proto::Vote {
        voter: value.voter.as_str().into(),
        block_hashes: value
            .block_hashes
            .iter()
            .map(|h| h.as_bytes().to_vec())
            .collect(),
        is_final: value.is_final,
        timestamp: value.timestamp,
        sequence: value.sequence,
        signature: value.signature.0.to_vec(),
    }
}
fn read_vote(value: proto::Vote) -> Result<WireVote, ProtocolError> {
    Ok(WireVote {
        voter: address(value.voter)?,
        block_hashes: value
            .block_hashes
            .into_iter()
            .map(hash)
            .collect::<Result<_, _>>()?,
        is_final: value.is_final,
        timestamp: value.timestamp,
        sequence: value.sequence,
        signature: Signature(fixed(value.signature)?),
    })
}

impl WireMessage {
    /// Encode a Protobuf envelope without the four-byte TCP length prefix.
    /// Enforce the envelope size limit; this does not validate payload semantics.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        use proto::envelope::Body as Body;
        let body = match self {
            Self::Block(block) => Body::Block(proto::BytesPayload {
                data: bincode::serialize(block).map_err(invalid)?,
            }),
            Self::Vote(value) => Body::Vote(vote(value)),
            Self::ConfirmAck(value) => Body::ConfirmAck(vote(&value.vote)),
            Self::ConfirmReq(value) => Body::ConfirmReq(proto::ConfirmRequest {
                block_hashes: value
                    .block_hashes
                    .iter()
                    .map(|h| h.as_bytes().to_vec())
                    .collect(),
                remaining_hops: value.remaining_hops.into(),
            }),
            Self::Keepalive(value) => Body::Keepalive(proto::Keepalive {
                peers: value.peers.clone(),
            }),
            Self::Handshake(value) => Body::Handshake(proto::Handshake {
                node_id: value.node_id.as_str().into(),
                cookie: value.cookie.map(|v| v.to_vec()),
                cookie_signature: value.cookie_signature.as_ref().map(|v| v.0.to_vec()),
                params_hash: value.params_hash.as_bytes().to_vec(),
                network: network_number(value.network_id),
                peering_port: value.peering_port.into(),
                block_version: value.block_version.into(),
                wire_version: value.wire_version.into(),
            }),
            Self::VerificationRequest(value) => Body::VerificationRequest(proto::VerificationRequest {
                target: value.target.as_str().into(),
                endorser: value.endorser.as_str().into(),
            }),
            Self::VerificationVote(value) => Body::VerificationVote(proto::VerificationVote {
                target: value.target.as_str().into(),
                voter: value.voter.as_str().into(),
                vote: value.vote.into(),
                signature: value.signature.clone(),
            }),
            Self::GovernanceProposal(value) => Body::GovernanceProposal(proto::GovernanceProposal {
                proposal_hash: value.proposal_hash.as_bytes().to_vec(),
                proposer: value.proposer.as_str().into(),
                content: value.content.clone(),
            }),
            Self::GovernanceVote(value) => Body::GovernanceVote(proto::GovernanceVote {
                proposal_hash: value.proposal_hash.as_bytes().to_vec(),
                voter: value.voter.as_str().into(),
                vote: value.vote.into(),
            }),
            Self::TelemetryReq => Body::TelemetryReq(proto::Empty {}),
            Self::TelemetryAck(v) => Body::TelemetryAck(proto::Telemetry {
                block_count: v.block_count,
                cemented_count: v.cemented_count,
                unchecked_count: v.unchecked_count,
                account_count: v.account_count,
                bandwidth_cap: v.bandwidth_cap,
                peer_count: v.peer_count,
                protocol_version: v.protocol_version.into(),
                uptime: v.uptime,
                genesis_hash: v.genesis_hash.as_bytes().to_vec(),
                major_version: v.major_version.into(),
                minor_version: v.minor_version.into(),
                patch_version: v.patch_version.into(),
                timestamp: v.timestamp,
                params_hash: v.params_hash.as_bytes().to_vec(),
            }),
            Self::Bootstrap(value) => Body::Bootstrap(bootstrap(value)),
            Self::CheckpointSync(value) => Body::CheckpointSync(sync(value)),
            Self::CheckpointAgreement(value) => Body::CheckpointAgreement(agreement(value)),
            Self::GovernanceContent(value) => Body::GovernanceContent(content(value)),
        };
        let envelope = proto::Envelope {
            wire_version: burst_types::PROTOCOL_VERSION.into(),
            body: Some(body),
        };
        check_message_size(envelope.encoded_len())?;
        Ok(envelope.encode_to_vec())
    }

    /// Classify canonical locally encoded bytes without decoding their payload.
    /// This is a scheduling hint, never authentication or recovery permission.
    /// Unknown/noncanonical envelopes conservatively use the bulk queue.
    pub fn outbound_class(mut bytes: &[u8]) -> burst_types::TrafficClass {
        use burst_types::TrafficClass::{Bulk, Control};
        use prost::encoding::{decode_key, decode_varint, WireType};
        if decode_key(&mut bytes).ok() != Some((1, WireType::Varint))
            || decode_varint(&mut bytes).ok() != Some(u64::from(burst_types::PROTOCOL_VERSION))
        {
            return Bulk;
        }
        let Ok((tag, WireType::LengthDelimited)) = decode_key(&mut bytes) else {
            return Bulk;
        };
        if decode_varint(&mut bytes).ok() != Some(bytes.len() as u64) {
            return Bulk;
        }
        match tag {
            11..=14 | 16 | 18 | 20..=22 | 24 => Control,
            // Sync requests/discovery are small control messages. Certificate,
            // snapshot and witness bodies retain the bulk bandwidth budget.
            23 => {
                let Ok((kind, WireType::LengthDelimited)) = decode_key(&mut bytes) else {
                    return Bulk;
                };
                if decode_varint(&mut bytes).ok() == Some(bytes.len() as u64)
                    && matches!(kind, 1 | 3 | 5..=7 | 9..=11)
                {
                    Control
                } else {
                    Bulk
                }
            }
            _ => Bulk,
        }
    }

    /// Decode an envelope with the current wire version, size limit, fixed-width
    /// fields and checked integer conversions. Transfer bounds are checked by
    /// receiving services, as are signatures, addresses and operation eligibility.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        check_message_size(bytes.len())?;
        let envelope = proto::Envelope::decode(bytes).map_err(invalid)?;
        if envelope.wire_version != u32::from(burst_types::PROTOCOL_VERSION) {
            return Err(invalid("incompatible wire version"));
        }
        use proto::envelope::Body as Body;
        Ok(
            match envelope
                .body
                .ok_or_else(|| invalid("missing message body"))?
            {
                Body::Block(value) => Self::Block(Box::new(
                    bincode::DefaultOptions::new()
                        .with_fixint_encoding()
                        .reject_trailing_bytes()
                        .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
                        .deserialize(&value.data)
                        .map_err(invalid)?,
                )),
                Body::Vote(value) => Self::Vote(read_vote(value)?),
                Body::ConfirmAck(value) => Self::ConfirmAck(ConfirmAckMsg {
                    vote: read_vote(value)?,
                }),
                Body::ConfirmReq(value) => Self::ConfirmReq(ConfirmReqMsg {
                    block_hashes: value
                        .block_hashes
                        .into_iter()
                        .map(hash)
                        .collect::<Result<_, _>>()?,
                    remaining_hops: narrow(value.remaining_hops)?,
                }),
                Body::Keepalive(value) => Self::Keepalive(KeepaliveMsg { peers: value.peers }),
                Body::Handshake(value) => Self::Handshake(HandshakeMsg {
                    node_id: address(value.node_id)?,
                    cookie: value.cookie.map(fixed).transpose()?,
                    cookie_signature: value
                        .cookie_signature
                        .map(|v| fixed(v).map(Signature))
                        .transpose()?,
                    params_hash: hash(value.params_hash)?,
                    network_id: network(value.network)?,
                    peering_port: narrow(value.peering_port)?,
                    block_version: narrow(value.block_version)?,
                    wire_version: narrow(value.wire_version)?,
                }),
                Body::VerificationRequest(value) => {
                    Self::VerificationRequest(VerificationRequestMessage {
                        target: address(value.target)?,
                        endorser: address(value.endorser)?,
                    })
                }
                Body::VerificationVote(value) => Self::VerificationVote(VerificationVoteMessage {
                    target: address(value.target)?,
                    voter: address(value.voter)?,
                    vote: narrow(value.vote)?,
                    signature: value.signature,
                }),
                Body::GovernanceProposal(value) => {
                    Self::GovernanceProposal(GovernanceProposalMessage {
                        proposal_hash: hash(value.proposal_hash)?,
                        proposer: address(value.proposer)?,
                        content: value.content,
                    })
                }
                Body::GovernanceVote(value) => Self::GovernanceVote(GovernanceVoteMessage {
                    proposal_hash: hash(value.proposal_hash)?,
                    voter: address(value.voter)?,
                    vote: narrow(value.vote)?,
                }),
                Body::TelemetryReq(_) => Self::TelemetryReq,
                Body::TelemetryAck(v) => Self::TelemetryAck(TelemetryAckMessage {
                    block_count: v.block_count,
                    cemented_count: v.cemented_count,
                    unchecked_count: v.unchecked_count,
                    account_count: v.account_count,
                    bandwidth_cap: v.bandwidth_cap,
                    peer_count: v.peer_count,
                    protocol_version: narrow(v.protocol_version)?,
                    uptime: v.uptime,
                    genesis_hash: hash(v.genesis_hash)?,
                    major_version: narrow(v.major_version)?,
                    minor_version: narrow(v.minor_version)?,
                    patch_version: narrow(v.patch_version)?,
                    timestamp: v.timestamp,
                    params_hash: hash(v.params_hash)?,
                }),
                Body::Bootstrap(value) => Self::Bootstrap(read_bootstrap(value)?),
                Body::CheckpointSync(value) => Self::CheckpointSync(read_sync(value)?),
                Body::CheckpointAgreement(value) => Self::CheckpointAgreement(read_agreement(value)?),
                Body::GovernanceContent(value) => Self::GovernanceContent(read_content(value)?),
            },
        )
    }

    pub fn encode_framed(&self) -> Result<Vec<u8>, ProtocolError> {
        let body = self.encode()?;
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        Ok(frame)
    }

    /// Decode the first complete length-prefixed frame and return bytes consumed.
    /// Any following frames remain in the caller's input buffer.
    pub fn decode_framed(bytes: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let prefix = bytes
            .get(..4)
            .ok_or_else(|| invalid("missing frame length"))?;
        let length = u32::from_be_bytes(prefix.try_into().unwrap()) as usize;
        check_message_size(length)?;
        let body = bytes
            .get(4..4 + length)
            .ok_or_else(|| invalid("incomplete frame"))?;
        Ok((Self::decode(body)?, 4 + length))
    }
}

fn bootstrap(value: &BootstrapMessage) -> proto::Bootstrap {
    use proto::bootstrap::Body as Body;
    let (id, body) = match value {
        BootstrapMessage::AscPullReq { id, payload } => (
            *id,
            match payload {
                Req::Blocks {
                    account,
                    start,
                    count,
                } => Body::BlocksRequest(proto::BlocksRequest {
                    account: account.as_str().into(),
                    start: start.as_bytes().to_vec(),
                    count: (*count).into(),
                }),
                Req::AccountInfo { account } => Body::AccountRequest(proto::AccountRequest {
                    account: account.as_str().into(),
                }),
                Req::Frontiers {
                    start_account,
                    count,
                } => Body::FrontiersRequest(proto::FrontiersRequest {
                    start_account: start_account.as_str().into(),
                    count: (*count).into(),
                }),
            },
        ),
        BootstrapMessage::AscPullAck { id, payload } => (
            *id,
            match payload {
                Ack::Blocks(blocks) => Body::BlocksResponse(proto::BlocksResponse {
                    blocks: blocks.clone(),
                }),
                Ack::AccountInfo {
                    account,
                    head,
                    block_count,
                } => Body::AccountResponse(proto::AccountResponse {
                    account: account.as_str().into(),
                    head: head.as_bytes().to_vec(),
                    block_count: *block_count,
                }),
                Ack::Frontiers(entries) => Body::FrontiersResponse(proto::FrontiersResponse {
                    entries: entries
                        .iter()
                        .map(|(account, head)| proto::Frontier {
                            account: account.as_str().into(),
                            head: head.as_bytes().to_vec(),
                        })
                        .collect(),
                }),
            },
        ),
    };
    proto::Bootstrap {
        id,
        body: Some(body),
    }
}
fn read_bootstrap(value: proto::Bootstrap) -> Result<BootstrapMessage, ProtocolError> {
    use proto::bootstrap::Body as Body;
    let id = value.id;
    Ok(
        match value
            .body
            .ok_or_else(|| invalid("missing bootstrap body"))?
        {
            Body::BlocksRequest(v) => BootstrapMessage::AscPullReq {
                id,
                payload: Req::Blocks {
                    account: address(v.account)?,
                    start: hash(v.start)?,
                    count: narrow(v.count)?,
                },
            },
            Body::AccountRequest(v) => BootstrapMessage::AscPullReq {
                id,
                payload: Req::AccountInfo {
                    account: address(v.account)?,
                },
            },
            Body::FrontiersRequest(v) => BootstrapMessage::AscPullReq {
                id,
                payload: Req::Frontiers {
                    start_account: address(v.start_account)?,
                    count: narrow(v.count)?,
                },
            },
            Body::BlocksResponse(v) => BootstrapMessage::AscPullAck {
                id,
                payload: Ack::Blocks(v.blocks),
            },
            Body::AccountResponse(v) => BootstrapMessage::AscPullAck {
                id,
                payload: Ack::AccountInfo {
                    account: address(v.account)?,
                    head: hash(v.head)?,
                    block_count: v.block_count,
                },
            },
            Body::FrontiersResponse(v) => BootstrapMessage::AscPullAck {
                id,
                payload: Ack::Frontiers(
                    v.entries
                        .into_iter()
                        .map(|v| Ok((address(v.account)?, hash(v.head)?)))
                        .collect::<Result<_, ProtocolError>>()?,
                ),
            },
        },
    )
}
fn sync(value: &Sync) -> proto::CheckpointSync {
    use proto::checkpoint_sync::Body as Body;
    proto::CheckpointSync {
        body: Some(match value {
            Sync::TipRequest { nonce } => Body::TipRequest(proto::TipRequest { nonce: *nonce }),
            Sync::TipObservation {
                nonce,
                height,
                epoch,
            } => Body::TipObservation(proto::TipObservation {
                nonce: *nonce,
                height: *height,
                epoch: epoch.as_bytes().to_vec(),
            }),
            Sync::WitnessHeadsRequest {
                starts,
                offset,
                digest,
            } => Body::WitnessHeadsRequest(proto::WitnessHeadsRequest {
                starts: starts.iter().map(|hash| hash.as_bytes().to_vec()).collect(),
                offset: *offset,
                digest: digest.map(|hash| hash.as_bytes().to_vec()),
            }),
            Sync::WitnessRequest {
                start,
                offset,
                digest,
            } => Body::WitnessRequest(proto::WitnessRequest {
                start: start.as_bytes().to_vec(),
                offset: *offset,
                digest: digest.map(|hash| hash.as_bytes().to_vec()),
            }),
            Sync::WitnessChunk {
                start,
                offset,
                total,
                digest,
                data,
            } => Body::WitnessChunk(proto::WitnessChunk {
                start: start.as_bytes().to_vec(),
                offset: *offset,
                total: *total,
                digest: digest.as_bytes().to_vec(),
                data: data.clone(),
            }),
            Sync::SnapshotQuery { minimum_height } => Body::SnapshotQuery(proto::SnapshotQuery {
                minimum_height: *minimum_height,
            }),
            Sync::SnapshotOffer {
                epoch,
                height,
                total,
                digest,
            } => Body::SnapshotOffer(proto::SnapshotOffer {
                epoch: epoch.as_bytes().to_vec(),
                height: *height,
                total: *total,
                digest: digest.as_bytes().to_vec(),
            }),
            Sync::SnapshotRequest {
                epoch,
                offset,
                digest,
            } => Body::SnapshotRequest(proto::SnapshotRequest {
                epoch: epoch.as_bytes().to_vec(),
                offset: *offset,
                digest: digest.map(|hash| hash.as_bytes().to_vec()),
            }),
            Sync::SnapshotChunk {
                epoch,
                offset,
                total,
                digest,
                data,
            } => Body::SnapshotChunk(proto::SnapshotChunk {
                epoch: epoch.as_bytes().to_vec(),
                offset: *offset,
                total: *total,
                digest: digest.as_bytes().to_vec(),
                data: data.clone(),
            }),
            Sync::Request {
                height,
                offset,
                digest,
                record_only,
            } => Body::Request(proto::SyncRequest {
                height: *height,
                offset: *offset,
                digest: digest.map(|h| h.as_bytes().to_vec()),
                record_only: *record_only,
            }),
            Sync::Chunk {
                height,
                offset,
                total,
                digest,
                data,
            } => Body::Chunk(proto::SyncChunk {
                height: *height,
                offset: *offset,
                total: *total,
                digest: digest.as_bytes().to_vec(),
                data: data.clone(),
            }),
        }),
    }
}
fn read_sync(value: proto::CheckpointSync) -> Result<Sync, ProtocolError> {
    use proto::checkpoint_sync::Body as Body;
    Ok(
        match value
            .body
            .ok_or_else(|| invalid("missing checkpoint sync body"))?
        {
            Body::TipRequest(v) => Sync::TipRequest { nonce: v.nonce },
            Body::TipObservation(v) => Sync::TipObservation {
                nonce: v.nonce,
                height: v.height,
                epoch: hash(v.epoch)?,
            },
            Body::WitnessHeadsRequest(v) => Sync::WitnessHeadsRequest {
                starts: v.starts.into_iter().map(hash).collect::<Result<_, _>>()?,
                offset: v.offset,
                digest: v.digest.map(hash).transpose()?,
            },
            Body::WitnessRequest(v) => Sync::WitnessRequest {
                start: hash(v.start)?,
                offset: v.offset,
                digest: v.digest.map(hash).transpose()?,
            },
            Body::WitnessChunk(v) => Sync::WitnessChunk {
                start: hash(v.start)?,
                offset: v.offset,
                total: v.total,
                digest: hash(v.digest)?,
                data: v.data,
            },
            Body::SnapshotQuery(v) => Sync::SnapshotQuery {
                minimum_height: v.minimum_height,
            },
            Body::SnapshotOffer(v) => Sync::SnapshotOffer {
                epoch: hash(v.epoch)?,
                height: v.height,
                total: v.total,
                digest: hash(v.digest)?,
            },
            Body::SnapshotRequest(v) => Sync::SnapshotRequest {
                epoch: hash(v.epoch)?,
                offset: v.offset,
                digest: v.digest.map(hash).transpose()?,
            },
            Body::SnapshotChunk(v) => Sync::SnapshotChunk {
                epoch: hash(v.epoch)?,
                offset: v.offset,
                total: v.total,
                digest: hash(v.digest)?,
                data: v.data,
            },
            Body::Request(v) => Sync::Request {
                height: v.height,
                offset: v.offset,
                digest: v.digest.map(hash).transpose()?,
                record_only: v.record_only,
            },
            Body::Chunk(v) => Sync::Chunk {
                height: v.height,
                offset: v.offset,
                total: v.total,
                digest: hash(v.digest)?,
                data: v.data,
            },
        },
    )
}
fn agreement(value: &AgreementPacket) -> proto::Agreement {
    use proto::agreement::Body as Body;
    proto::Agreement {
        context: Some(proto::AgreementContext {
            network: network_number(value.context.network),
            genesis: value.context.genesis.as_bytes().to_vec(),
            parent: value.context.parent.as_bytes().to_vec(),
            height: value.context.height,
        }),
        body: Some(match &value.body {
            AgreementBody::Offer { digest, total } => Body::Offer(proto::Offer {
                digest: digest.as_bytes().to_vec(),
                total: *total,
            }),
            AgreementBody::Request { digest, offset } => Body::Request(proto::ChunkRequest {
                digest: digest.as_bytes().to_vec(),
                offset: *offset,
            }),
            AgreementBody::Chunk {
                digest,
                total,
                offset,
                data,
            } => Body::Chunk(proto::Chunk {
                digest: digest.as_bytes().to_vec(),
                total: *total,
                offset: *offset,
                data: data.clone(),
            }),
        }),
    }
}
fn read_agreement(value: proto::Agreement) -> Result<AgreementPacket, ProtocolError> {
    use proto::agreement::Body as Body;
    let context = value
        .context
        .ok_or_else(|| invalid("missing agreement context"))?;
    Ok(AgreementPacket {
        context: Context {
            network: network(context.network)?,
            genesis: hash(context.genesis)?,
            parent: hash(context.parent)?,
            height: context.height,
        },
        body: match value
            .body
            .ok_or_else(|| invalid("missing agreement body"))?
        {
            Body::Offer(v) => AgreementBody::Offer {
                digest: hash(v.digest)?,
                total: v.total,
            },
            Body::Request(v) => AgreementBody::Request {
                digest: hash(v.digest)?,
                offset: v.offset,
            },
            Body::Chunk(v) => AgreementBody::Chunk {
                digest: hash(v.digest)?,
                total: v.total,
                offset: v.offset,
                data: v.data,
            },
        },
    })
}
fn content(value: &Content) -> proto::Content {
    use proto::content::Body as Body;
    proto::Content {
        body: Some(match value {
            Content::Request { proposal, offset } => Body::Request(proto::ContentRequest {
                proposal: proposal.as_bytes().to_vec(),
                offset: *offset,
            }),
            Content::Chunk { proposal, chunk } => Body::Chunk(proto::ContentChunk {
                proposal: proposal.as_bytes().to_vec(),
                hash: chunk.manifest.hash.as_bytes().to_vec(),
                length: chunk.manifest.length,
                offset: chunk.offset,
                data: chunk.bytes.clone(),
            }),
        }),
    }
}
fn read_content(value: proto::Content) -> Result<Content, ProtocolError> {
    use proto::content::Body as Body;
    Ok(
        match value.body.ok_or_else(|| invalid("missing content body"))? {
            Body::Request(v) => Content::Request {
                proposal: hash(v.proposal)?,
                offset: v.offset,
            },
            Body::Chunk(v) => Content::Chunk {
                proposal: hash(v.proposal)?,
                chunk: burst_governance::content::ContentChunk {
                    manifest: burst_governance::content::ContentManifest {
                        hash: hash(v.hash)?,
                        length: v.length,
                    },
                    offset: v.offset,
                    bytes: v.data,
                },
            },
        },
    )
}

#[cfg(test)]
mod tests;
