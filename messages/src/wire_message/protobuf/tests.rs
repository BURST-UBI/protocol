use super::*;
use proptest::prelude::*;

fn envelope(body: proto::envelope::Body) -> proto::Envelope {
    proto::Envelope {
        wire_version: burst_types::PROTOCOL_VERSION.into(),
        body: Some(body),
    }
}
fn roundtrip(message: WireMessage) {
    let before = format!("{message:?}");
    let bytes = message.encode().unwrap();
    use burst_types::TrafficClass::{Bulk, Control};
    let expected = match &message {
        WireMessage::Vote(_)
        | WireMessage::ConfirmReq(_)
        | WireMessage::ConfirmAck(_)
        | WireMessage::Keepalive(_)
        | WireMessage::Handshake(_)
        | WireMessage::VerificationVote(_)
        | WireMessage::GovernanceVote(_)
        | WireMessage::TelemetryReq
        | WireMessage::TelemetryAck(_)
        | WireMessage::CheckpointAgreement(_) => Control,
        WireMessage::CheckpointSync(sync) => match sync {
            Sync::Chunk { .. } | Sync::SnapshotChunk { .. } | Sync::WitnessChunk { .. } => Bulk,
            _ => Control,
        },
        _ => Bulk,
    };
    assert_eq!(WireMessage::outbound_class(&bytes), expected, "{before}");
    let decoded = WireMessage::decode(&bytes).unwrap();
    assert_eq!(before, format!("{decoded:?}"));
    assert_eq!(bytes, decoded.encode().unwrap());
    let frame = message.encode_framed().unwrap();
    assert_eq!(
        u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
        bytes.len()
    );
    let (_, used) = WireMessage::decode_framed(&frame).unwrap();
    assert_eq!(used, frame.len());
}

#[test]
fn schema_golden_and_unknown_field_evolution() {
    // Independently readable using protoc --decode=burst.wire.v33.Envelope.
    assert_eq!(
        WireMessage::TelemetryReq.encode().unwrap(),
        [8, 39, 170, 1, 0]
    );
    let message = WireMessage::Keepalive(KeepaliveMsg {
        peers: vec!["x".into()],
    });
    let mut encoded = message.encode().unwrap();
    assert_eq!(encoded, [8, 39, 114, 3, 10, 1, 120]);
    encoded.extend([160, 6, 1]); // unknown varint field 100
    assert!(matches!(
        WireMessage::decode(&encoded),
        Ok(WireMessage::Keepalive(_))
    ));
    roundtrip(message);
}

#[test]
fn malformed_peer_addresses_are_errors_not_constructor_panics() {
    // Coverage-guided regression: an empty ConfirmAck voter follows a bootstrap field.
    let crash = [8, 31, 122, 4, 8, 255, 255, 1, 106, 0];
    assert!(WireMessage::decode(&crash).is_err());
    let mut frame = (crash.len() as u32).to_be_bytes().to_vec();
    frame.extend(crash);
    assert!(WireMessage::decode_framed(&frame).is_err());
    for raw in ["", "wrong-prefix", "BRST_account", "\0brst_a"] {
        assert!(address(raw.into()).is_err());
        for (target, endorser) in [(raw, "brst_test"), ("brst_test", raw)] {
            let bytes = envelope(proto::envelope::Body::VerificationRequest(
                proto::VerificationRequest {
                    target: target.into(),
                    endorser: endorser.into(),
                },
            ))
            .encode_to_vec();
            assert!(WireMessage::decode(&bytes).is_err());
        }
    }
    assert!(address(format!("brst_{}", "x".repeat(124))).is_err());
    assert!(address("brst_test".into()).is_ok());
}

#[test]
fn every_bootstrap_payload_retains_request_identity() {
    let a = WalletAddress::new("brst_test");
    let h = BlockHash::new([7; 32]);
    for payload in [
        Req::Blocks {
            account: a.clone(),
            start: h,
            count: 128,
        },
        Req::AccountInfo { account: a.clone() },
        Req::Frontiers {
            start_account: a.clone(),
            count: 512,
        },
    ] {
        roundtrip(WireMessage::Bootstrap(BootstrapMessage::AscPullReq {
            id: u64::MAX,
            payload,
        }));
    }
    for payload in [
        Ack::Blocks(vec![vec![1, 2], vec![3, 4]]),
        Ack::AccountInfo {
            account: a.clone(),
            head: h,
            block_count: u64::MAX,
        },
        Ack::Frontiers(vec![(a, h)]),
    ] {
        roundtrip(WireMessage::Bootstrap(BootstrapMessage::AscPullAck {
            id: u64::MAX,
            payload,
        }));
    }
}

#[test]
fn typed_chunk_messages_preserve_context_and_exact_bytes() {
    let h = BlockHash::new([5; 32]);
    roundtrip(WireMessage::CheckpointSync(Sync::SnapshotQuery {
        minimum_height: 3,
    }));
    roundtrip(WireMessage::CheckpointSync(Sync::SnapshotOffer {
        epoch: h,
        height: 3,
        total: 25,
        digest: h,
    }));
    for digest in [None, Some(h)] {
        roundtrip(WireMessage::CheckpointSync(Sync::WitnessHeadsRequest {
            starts: vec![h, BlockHash::new([6; 32])],
            offset: 0,
            digest,
        }));
        roundtrip(WireMessage::CheckpointSync(Sync::WitnessRequest {
            start: h,
            offset: 0,
            digest,
        }));
        roundtrip(WireMessage::CheckpointSync(Sync::SnapshotRequest {
            epoch: h,
            offset: 0,
            digest,
        }));
        for record_only in [false, true] {
            roundtrip(WireMessage::CheckpointSync(Sync::Request {
                record_only,
                height: 1,
                offset: 0,
                digest,
            }));
        }
    }
    roundtrip(WireMessage::CheckpointSync(Sync::SnapshotChunk {
        epoch: h,
        offset: 0,
        total: 25,
        digest: h,
        data: vec![1; 25],
    }));
    roundtrip(WireMessage::CheckpointSync(Sync::WitnessChunk {
        start: h,
        offset: 0,
        total: 5,
        digest: h,
        data: vec![1; 5],
    }));
    roundtrip(WireMessage::CheckpointSync(Sync::Chunk {
        height: 1,
        offset: 0,
        total: 3,
        digest: h,
        data: vec![1, 2, 3],
    }));
    for body in [
        AgreementBody::Offer {
            digest: h,
            total: 3,
        },
        AgreementBody::Request {
            digest: h,
            offset: 0,
        },
        AgreementBody::Chunk {
            digest: h,
            offset: 0,
            total: 3,
            data: vec![1, 2, 3],
        },
    ] {
        roundtrip(WireMessage::CheckpointAgreement(AgreementPacket {
            context: Context {
                network: NetworkId::Dev,
                genesis: h,
                parent: h,
                height: 1,
            },
            body,
        }));
    }
    roundtrip(WireMessage::GovernanceContent(Content::Request {
        proposal: h,
        offset: 0,
    }));
    roundtrip(WireMessage::GovernanceContent(Content::Chunk {
        proposal: h,
        chunk: burst_governance::content::ContentChunk {
            manifest: burst_governance::content::ContentManifest { hash: h, length: 3 },
            offset: 0,
            bytes: vec![1, 2, 3],
        },
    }));
}

#[test]
fn snapshot_lengths_preserve_all_64_bits_under_the_new_wire_version() {
    let hash = BlockHash::new([7; 32]);
    for offset in [1u64 << 32, u64::MAX - 16383] {
        roundtrip(WireMessage::CheckpointSync(Sync::SnapshotRequest {
            epoch: hash,
            offset,
            digest: Some(hash),
        }));
        roundtrip(WireMessage::CheckpointSync(Sync::SnapshotOffer {
            epoch: hash,
            height: 1,
            total: offset + 3,
            digest: hash,
        }));
        roundtrip(WireMessage::CheckpointSync(Sync::SnapshotChunk {
            epoch: hash,
            offset,
            total: offset + 3,
            digest: hash,
            data: vec![1; 3],
        }));
    }
}

#[test]
fn record_only_request_preserves_default_encoding_and_legacy_field_decoding() {
    #[derive(Clone, PartialEq, prost::Message)]
    struct LegacyRequest {
        #[prost(uint64, tag = "1")]
        height: u64,
        #[prost(uint32, tag = "2")]
        offset: u32,
        #[prost(bytes = "vec", optional, tag = "3")]
        digest: Option<Vec<u8>>,
    }
    let legacy = LegacyRequest {
        height: 129,
        offset: 16384,
        digest: Some(vec![7; 32]),
    };
    let bytes = legacy.encode_to_vec();
    let mut current = proto::SyncRequest::decode(bytes.as_slice()).unwrap();
    assert!(!current.record_only);
    assert_eq!(current.encode_to_vec(), bytes);
    current.record_only = true;
    let bytes = current.encode_to_vec();
    assert_eq!(LegacyRequest::decode(bytes.as_slice()).unwrap(), legacy);
    assert!(
        proto::SyncRequest::decode(bytes.as_slice())
            .unwrap()
            .record_only
    );
}

#[test]
fn rejects_legacy_missing_oversized_and_truncated_envelopes() {
    assert!(WireMessage::decode(&11u32.to_le_bytes()).is_err());
    assert!(WireMessage::decode(
        &proto::Envelope {
            wire_version: burst_types::PROTOCOL_VERSION.into(),
            body: None
        }
        .encode_to_vec()
    )
    .is_err());
    let mut message = envelope(proto::envelope::Body::TelemetryReq(proto::Empty {}));
    for version in [
        0,
        burst_types::PROTOCOL_VERSION as u32 - 1,
        burst_types::PROTOCOL_VERSION as u32 + 1,
        u32::MAX,
    ] {
        message.wire_version = version;
        assert!(WireMessage::decode(&message.encode_to_vec()).is_err());
    }
    assert!(WireMessage::decode(&vec![0; burst_types::MAX_P2P_MESSAGE_BYTES + 1]).is_err());
    assert!(WireMessage::Keepalive(KeepaliveMsg {
        peers: vec!["x".repeat(burst_types::MAX_P2P_MESSAGE_BYTES)]
    })
    .encode()
    .is_err());
    assert!(WireMessage::decode_framed(&u32::MAX.to_be_bytes()).is_err());
    let frame = WireMessage::TelemetryReq.encode_framed().unwrap();
    for length in 0..frame.len() {
        assert!(WireMessage::decode_framed(&frame[..length]).is_err());
    }
    let mut two = frame.clone();
    two.extend(&frame);
    assert_eq!(WireMessage::decode_framed(&two).unwrap().1, frame.len());

    let mut forged = vec![8, 31, 114]; // keepalive with a dishonest length
    forged.extend([255, 255, 255, 255, 15]);
    assert!(WireMessage::decode(&forged).is_err());
    let mut nested = WireMessage::TelemetryReq.encode().unwrap();
    nested.extend(std::iter::repeat_n(27, 200)); // unknown field 3, start group
    nested.extend(std::iter::repeat_n(28, 200)); // end group
    assert!(WireMessage::decode(&nested).is_err());
}

#[test]
fn rejects_narrowing_missing_network_and_wrong_fixed_widths() {
    use proto::envelope::Body as B;
    let baseline = proto::Handshake {
        node_id: "brst_test".into(),
        cookie: Some(vec![1; 32]),
        cookie_signature: Some(vec![2; 64]),
        params_hash: vec![3; 32],
        network: 3,
        peering_port: 7076,
        block_version: 6,
        wire_version: burst_types::PROTOCOL_VERSION.into(),
    };
    assert!(WireMessage::decode(&envelope(B::Handshake(baseline.clone())).encode_to_vec()).is_ok());
    for changed in 0..8 {
        let mut h = baseline.clone();
        match changed {
            0 => h.network = 0,
            1 => h.network = 4,
            2 => h.peering_port = 65536,
            3 => h.block_version = 256,
            4 => h.wire_version = 65536,
            5 => h.cookie = Some(vec![0; 31]),
            6 => h.cookie_signature = Some(vec![0; 63]),
            _ => h.params_hash = vec![0; 33],
        }
        assert!(WireMessage::decode(&envelope(B::Handshake(h)).encode_to_vec()).is_err());
    }
    let mut v = vote(&super::super::tests::sample_vote());
    v.block_hashes[0].pop();
    assert!(WireMessage::decode(&envelope(B::Vote(v)).encode_to_vec()).is_err());
    let q = proto::ConfirmRequest {
        block_hashes: vec![],
        remaining_hops: 256,
    };
    assert!(WireMessage::decode(&envelope(B::ConfirmReq(q)).encode_to_vec()).is_err());
}

#[test]
fn block_hash_signature_and_bytes_are_unchanged() {
    let block = super::super::tests::sample_block();
    let original = bincode::serialize(&block).unwrap();
    roundtrip(WireMessage::Block(Box::new(block)));
    let mut trailing = original.clone();
    trailing.push(0);
    let body = proto::envelope::Body::Block(proto::BytesPayload { data: trailing });
    assert!(WireMessage::decode(&envelope(body).encode_to_vec()).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn arbitrary_peer_bytes_do_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _=WireMessage::decode(&bytes);
        let _=WireMessage::decode_framed(&bytes);
    }
}
