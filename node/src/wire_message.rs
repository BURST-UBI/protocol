//! Top-level P2P wire message envelope.
//!
//! Every message sent between BURST nodes is wrapped in [`WireMessage`].
//! The peer read loop deserializes incoming frames as `WireMessage` first;
//! if that fails it falls back to bare `StateBlock` for backward compat.

use burst_ledger::StateBlock;
use burst_types::{BlockHash, Signature, WalletAddress};
use serde::{Deserialize, Serialize};

use crate::bootstrap::BootstrapMessage;

/// Top-level P2P wire message.
/// Every message sent between nodes is wrapped in this enum.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WireMessage {
    /// A block (publish/flood)
    Block(StateBlock),
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
    /// UHV verification request (endorser vouching for a target)
    VerificationRequest(VerificationRequestMessage),
    /// UHV verification vote from a selected verifier
    VerificationVote(VerificationVoteMessage),
    /// Governance proposal broadcast
    GovernanceProposal(GovernanceProposalMessage),
    /// Governance vote broadcast
    GovernanceVote(GovernanceVoteMessage),
    /// Telemetry request (solicits a TelemetryAck)
    TelemetryReq,
    /// Telemetry acknowledgment with node stats
    TelemetryAck(TelemetryAckMessage),
}

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
}

/// Response to a confirm_req — carries a vote (confirm_ack).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmAckMsg {
    pub vote: WireVote,
}

/// Keepalive message carrying a list of known peer addresses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeepaliveMsg {
    pub peers: Vec<String>,
}

/// Handshake / SYN-cookie exchange for peer authentication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeMsg {
    pub node_id: WalletAddress,
    pub cookie: Option<[u8; 32]>,
    pub cookie_signature: Option<Signature>,
    /// Deterministic hash of the node's current ProtocolParams.
    /// Peers compare this to detect protocol version divergence.
    #[serde(default)]
    pub params_hash: BlockHash,
}

/// UHV verification request: an endorser vouches for a target wallet's humanity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationRequestMessage {
    pub target: WalletAddress,
    pub endorser: WalletAddress,
}

/// UHV verification vote from a selected verifier.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationVoteMessage {
    pub target: WalletAddress,
    pub voter: WalletAddress,
    /// 0 = Legitimate, 1 = Illegitimate, 2 = Neither
    pub vote: u8,
    pub signature: Vec<u8>,
}

/// Governance proposal broadcast on the network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceProposalMessage {
    pub proposal_hash: BlockHash,
    pub proposer: WalletAddress,
    pub content: Vec<u8>,
}

/// Governance vote broadcast on the network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceVoteMessage {
    pub proposal_hash: BlockHash,
    pub voter: WalletAddress,
    /// 0 = Yea, 1 = Nay, 2 = Abstain
    pub vote: u8,
}

/// Telemetry acknowledgment carrying node statistics.
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

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_types::{Timestamp, TxHash};

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(&format!("brst_{s}"))
    }

    fn sample_block() -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Send,
            account: addr("alice"),
            previous: BlockHash::new([1u8; 32]),
            representative: addr("rep"),
            brn_balance: 1000,
            trst_balance: 500,
            link: BlockHash::new([2u8; 32]),
            origin: TxHash::new([3u8; 32]),
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(42),
            params_hash: BlockHash::ZERO,
            work: 0xDEAD,
            signature: Signature([0xFF; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn sample_vote() -> WireVote {
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
        let msg = WireMessage::Block(sample_block());
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
        match decoded {
            WireMessage::Block(b) => assert_eq!(b.hash, sample_block().hash),
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn vote_message_roundtrip() {
        let msg = WireMessage::Vote(sample_vote());
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
        match decoded {
            WireMessage::Vote(v) => {
                assert_eq!(v.voter, addr("voter1"));
                assert_eq!(v.block_hashes.len(), 1);
                assert_eq!(v.is_final, false);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn confirm_req_roundtrip() {
        let msg = WireMessage::ConfirmReq(ConfirmReqMsg {
            block_hashes: vec![BlockHash::new([1u8; 32]), BlockHash::new([2u8; 32])],
        });
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        });
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
        match decoded {
            WireMessage::Handshake(h) => {
                assert_eq!(h.node_id, addr("node1"));
                assert!(h.cookie.is_some());
                assert!(h.cookie_signature.is_some());
            }
            other => panic!("expected Handshake, got {:?}", other),
        }
    }

    #[test]
    fn handshake_no_cookie_roundtrip() {
        let msg = WireMessage::Handshake(HandshakeMsg {
            node_id: addr("node2"),
            cookie: None,
            cookie_signature: None,
            params_hash: burst_types::BlockHash::default(),
        });
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
        match decoded {
            WireMessage::GovernanceVote(g) => assert_eq!(g.vote, 1),
            other => panic!("expected GovernanceVote, got {:?}", other),
        }
    }

    #[test]
    fn telemetry_req_roundtrip() {
        let msg = WireMessage::TelemetryReq;
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
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
        let result = bincode::deserialize::<WireMessage>(&garbage);
        assert!(result.is_err());
    }

    #[test]
    fn truncated_message_rejected() {
        let msg = WireMessage::Vote(sample_vote());
        let bytes = bincode::serialize(&msg).unwrap();
        let truncated = &bytes[..bytes.len() / 2];
        let result = bincode::deserialize::<WireMessage>(truncated);
        assert!(result.is_err());
    }

    #[test]
    fn empty_bytes_rejected() {
        let result = bincode::deserialize::<WireMessage>(&[]);
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
        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: WireMessage = bincode::deserialize(&bytes).unwrap();
        match decoded {
            WireMessage::Vote(v) => {
                assert_eq!(v.block_hashes.len(), 100);
                assert!(v.is_final);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }
}
