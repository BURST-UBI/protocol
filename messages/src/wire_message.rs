//! Top-level P2P wire message envelope.
//!
//! Every message sent between BURST nodes is wrapped in [`WireMessage`].
//! The peer read loop decodes bounded frames as `WireMessage`.
//! Bare blocks are not an alternative wire format.

use burst_ledger::StateBlock;
use burst_types::{BlockHash, NetworkId, Signature, WalletAddress};
use serde::{Deserialize, Serialize};

use crate::bootstrap::BootstrapMessage;

mod protobuf;

pub use crate::{
    ConfirmAckMsg, ConfirmReqMsg, GovernanceProposalMessage, GovernanceVoteMessage, KeepaliveMsg,
    TelemetryAckMessage, VerificationRequestMessage, VerificationVoteMessage, WireVote,
};

/// Top-level P2P wire message.
/// Every message sent between nodes is wrapped in this enum.
#[derive(Clone, Debug)]
pub enum WireMessage {
    /// A block (publish/flood)
    Block(Box<StateBlock>),
    /// A vote for a block
    Vote(WireVote),
    /// Request votes for a block (confirm_req)
    ConfirmReq(ConfirmReqMsg),
    /// Response to confirm_req (confirm_ack)
    ConfirmAck(ConfirmAckMsg),
    /// Keepalive with peer list
    Keepalive(KeepaliveMsg),
    /// Bootstrap protocol message
    Bootstrap(BootstrapMessage),
    /// Handshake / SYN cookie exchange
    Handshake(HandshakeMsg),
    /// Legacy verification request; the node only logs receipt.
    VerificationRequest(VerificationRequestMessage),
    /// Legacy verification vote; the node only logs receipt.
    VerificationVote(VerificationVoteMessage),
    /// Legacy proposal announcement; the node only logs receipt.
    GovernanceProposal(GovernanceProposalMessage),
    /// Legacy governance vote; the node only logs receipt.
    GovernanceVote(GovernanceVoteMessage),
    /// Telemetry request (solicits a TelemetryAck)
    TelemetryReq,
    /// Telemetry acknowledgment with node stats
    TelemetryAck(TelemetryAckMessage),
    /// Checkpoint and recovery transfer. Receivers authenticate and validate contents.
    CheckpointSync(crate::checkpoint_sync::CheckpointSyncMessage),
    /// Bounded agreement payload transfer; signed contents are checked by the driver.
    CheckpointAgreement(crate::checkpoint_transport::Packet),
    GovernanceContent(crate::governance_content::Packet),
}

/// Handshake / SYN-cookie exchange for peer authentication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeMsg {
    pub node_id: WalletAddress,
    pub cookie: Option<[u8; 32]>,
    pub cookie_signature: Option<Signature>,
    /// Deterministic hash of the node's current ProtocolParams.
    /// Authenticated by the handshake proof, but equality is not required.
    #[serde(default)]
    pub params_hash: BlockHash,
    /// Network identity. The handshake rejects a different network even when
    /// the parameter hashes match.
    #[serde(default)]
    pub network_id: NetworkId,
    /// The sender's listening port, authenticated as transport metadata.
    /// It does not establish a public NAT mapping. Peers are keyed by their
    /// proven node identity, not by a public-IP/local-port combination.
    #[serde(default)]
    pub peering_port: u16,
    /// Reject incompatible consensus rules before registering a peer.
    pub block_version: u8,
    pub wire_version: u16,
}

impl HandshakeMsg {
    pub fn supports_current_protocol(&self) -> bool {
        self.block_version == burst_ledger::CURRENT_BLOCK_VERSION
            && self.wire_version == burst_types::PROTOCOL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_types::{Timestamp, TxHash};

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{s}"))
    }

    pub(super) fn sample_block() -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: addr("alice"),
            previous: BlockHash::new([1u8; 32]),
            representative: addr("rep"),
            brn_spent: 1000,
            trst_balance: 500,
            link: BlockHash::new([2u8; 32]),
            origin: TxHash::new([3u8; 32]),
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(42),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0xDEAD,
            signature: Signature([0xFF; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    pub(super) fn sample_vote() -> WireVote {
        WireVote {
            voter: addr("voter1"),
            block_hashes: vec![BlockHash::new([0xAA; 32])],
            is_final: false,
            timestamp: 12345,
            sequence: 1,
            signature: Signature([0xBB; 64]),
        }
    }

    #[test]
    fn block_message_roundtrip() {
        let msg = WireMessage::Block(Box::new(sample_block()));
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Block(b) => assert_eq!(b.hash, sample_block().hash),
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn vote_message_roundtrip() {
        let msg = WireMessage::Vote(sample_vote());
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Vote(v) => {
                assert_eq!(v.voter, addr("voter1"));
                assert_eq!(v.block_hashes.len(), 1);
                assert!(!v.is_final);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn confirm_req_roundtrip() {
        let msg = WireMessage::ConfirmReq(ConfirmReqMsg {
            block_hashes: vec![BlockHash::new([1u8; 32]), BlockHash::new([2u8; 32])],
            remaining_hops: 3,
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::ConfirmReq(r) => assert_eq!(r.block_hashes.len(), 2),
            other => panic!("expected ConfirmReq, got {:?}", other),
        }
    }

    #[test]
    fn confirm_ack_roundtrip() {
        let msg = WireMessage::ConfirmAck(ConfirmAckMsg {
            vote: sample_vote(),
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::ConfirmAck(a) => assert_eq!(a.vote.voter, addr("voter1")),
            other => panic!("expected ConfirmAck, got {:?}", other),
        }
    }

    #[test]
    fn keepalive_roundtrip() {
        let msg = WireMessage::Keepalive(KeepaliveMsg {
            peers: vec!["192.168.1.1:7075".into(), "10.0.0.1:7075".into()],
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Keepalive(k) => assert_eq!(k.peers.len(), 2),
            other => panic!("expected Keepalive, got {:?}", other),
        }
    }

    #[test]
    fn handshake_roundtrip() {
        let msg = WireMessage::Handshake(HandshakeMsg {
            node_id: addr("node1"),
            cookie: Some([0xCC; 32]),
            cookie_signature: Some(Signature([0xDD; 64])),
            params_hash: burst_types::BlockHash::default(),
            network_id: NetworkId::Live,
            peering_port: 7076,
            block_version: CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Handshake(h) => {
                assert_eq!(h.node_id, addr("node1"));
                assert!(h.cookie.is_some());
                assert!(h.cookie_signature.is_some());
                assert_eq!(h.network_id, NetworkId::Live);
                assert!(h.supports_current_protocol());
            }
            other => panic!("expected Handshake, got {:?}", other),
        }
    }

    #[test]
    fn handshake_requires_current_block_and_wire_versions() {
        let mut handshake = HandshakeMsg {
            node_id: addr("old_node"),
            cookie: None,
            cookie_signature: None,
            params_hash: BlockHash::ZERO,
            network_id: NetworkId::Dev,
            peering_port: 27076,
            block_version: CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
        };
        assert!(handshake.supports_current_protocol());
        handshake.block_version = 1;
        assert!(!handshake.supports_current_protocol());
        handshake.block_version = CURRENT_BLOCK_VERSION + 1;
        assert!(!handshake.supports_current_protocol());
        handshake.block_version = CURRENT_BLOCK_VERSION;
        handshake.wire_version = burst_types::PROTOCOL_VERSION - 1;
        assert!(!handshake.supports_current_protocol());
        handshake.wire_version = burst_types::PROTOCOL_VERSION + 1;
        assert!(!handshake.supports_current_protocol());
        handshake.wire_version = burst_types::PROTOCOL_VERSION;

        let mut old_bytes = 6u32.to_le_bytes().to_vec();
        old_bytes.extend(bincode::serialize(&handshake).unwrap());
        old_bytes.truncate(old_bytes.len() - 2); // Earlier handshakes omitted wire_version.
        assert!(WireMessage::decode(&old_bytes).is_err());
    }

    #[test]
    fn handshake_no_cookie_roundtrip() {
        let msg = WireMessage::Handshake(HandshakeMsg {
            node_id: addr("node2"),
            cookie: None,
            cookie_signature: None,
            params_hash: burst_types::BlockHash::default(),
            network_id: NetworkId::Dev,
            peering_port: 27076,
            block_version: CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Handshake(h) => {
                assert!(h.cookie.is_none());
                assert!(h.cookie_signature.is_none());
            }
            other => panic!("expected Handshake, got {:?}", other),
        }
    }

    #[test]
    fn verification_request_roundtrip() {
        let msg = WireMessage::VerificationRequest(VerificationRequestMessage {
            target: addr("target"),
            endorser: addr("endorser"),
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::VerificationRequest(v) => {
                assert_eq!(v.target, addr("target"));
                assert_eq!(v.endorser, addr("endorser"));
            }
            other => panic!("expected VerificationRequest, got {:?}", other),
        }
    }

    #[test]
    fn verification_vote_roundtrip() {
        let msg = WireMessage::VerificationVote(VerificationVoteMessage {
            target: addr("target"),
            voter: addr("voter"),
            vote: 0,
            signature: vec![1, 2, 3, 4],
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::VerificationVote(v) => {
                assert_eq!(v.vote, 0);
                assert_eq!(v.signature, vec![1, 2, 3, 4]);
            }
            other => panic!("expected VerificationVote, got {:?}", other),
        }
    }

    #[test]
    fn governance_proposal_roundtrip() {
        let msg = WireMessage::GovernanceProposal(GovernanceProposalMessage {
            proposal_hash: BlockHash::new([0xAA; 32]),
            proposer: addr("proposer"),
            content: b"change brn rate to 200".to_vec(),
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::GovernanceProposal(g) => {
                assert_eq!(g.proposer, addr("proposer"));
                assert!(!g.content.is_empty());
            }
            other => panic!("expected GovernanceProposal, got {:?}", other),
        }
    }

    #[test]
    fn governance_vote_roundtrip() {
        let msg = WireMessage::GovernanceVote(GovernanceVoteMessage {
            proposal_hash: BlockHash::new([0xBB; 32]),
            voter: addr("voter"),
            vote: 1,
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::GovernanceVote(g) => assert_eq!(g.vote, 1),
            other => panic!("expected GovernanceVote, got {:?}", other),
        }
    }

    #[test]
    fn telemetry_req_roundtrip() {
        let msg = WireMessage::TelemetryReq;
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        assert!(matches!(decoded, WireMessage::TelemetryReq));
    }

    #[test]
    fn telemetry_ack_roundtrip() {
        let msg = WireMessage::TelemetryAck(TelemetryAckMessage {
            block_count: 1_000_000,
            cemented_count: 999_000,
            unchecked_count: 500,
            account_count: 50_000,
            bandwidth_cap: 10_000_000,
            peer_count: 200,
            protocol_version: 1,
            uptime: 86400,
            genesis_hash: BlockHash::new([0xFF; 32]),
            major_version: 0,
            minor_version: 1,
            patch_version: 0,
            timestamp: 1700000000,
            params_hash: BlockHash::ZERO,
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::TelemetryAck(t) => {
                assert_eq!(t.block_count, 1_000_000);
                assert_eq!(t.peer_count, 200);
                assert_eq!(t.protocol_version, 1);
            }
            other => panic!("expected TelemetryAck, got {:?}", other),
        }
    }

    #[test]
    fn corrupt_bytes_rejected_gracefully() {
        let garbage = vec![0xFF, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];
        let result = WireMessage::decode(&garbage);
        assert!(result.is_err());
    }

    #[test]
    fn truncated_message_rejected() {
        let msg = WireMessage::Vote(sample_vote());
        let bytes = msg.encode().unwrap();
        let truncated = &bytes[..bytes.len() / 2];
        let result = WireMessage::decode(truncated);
        assert!(result.is_err());
    }

    #[test]
    fn empty_bytes_rejected() {
        let result = WireMessage::decode(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn vote_with_many_hashes_roundtrip() {
        let hashes: Vec<BlockHash> = (0..100u8).map(|i| BlockHash::new([i; 32])).collect();
        let msg = WireMessage::Vote(WireVote {
            voter: addr("bulk_voter"),
            block_hashes: hashes.clone(),
            is_final: true,
            timestamp: 99999,
            sequence: 42,
            signature: Signature([0x11; 64]),
        });
        let bytes = msg.encode().unwrap();
        let decoded: WireMessage = WireMessage::decode(&bytes).unwrap();
        match decoded {
            WireMessage::Vote(v) => {
                assert_eq!(v.block_hashes.len(), 100);
                assert!(v.is_final);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }
}
