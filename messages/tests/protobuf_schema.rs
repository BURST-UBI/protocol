//! Cross-language schema conformance; ordinary builds do not require protoc.
use burst_messages::WireMessage;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires the independent protoc schema compiler"]
fn protoc_schema_and_rust_codec_agree_for_every_message() {
    let version = burst_types::PROTOCOL_VERSION;
    let hash = "h".repeat(32);
    let signature = "s".repeat(64);
    let vote = format!("voter: 'brst_v' block_hashes: '{hash}' is_final: true timestamp: 8 sequence: 9 signature: '{signature}'");
    let bootstrap = [
        format!("blocks_request {{ account:'brst_a' start:'{hash}' count:128 }}"),
        "account_request { account:'brst_a' }".into(),
        "frontiers_request { start_account:'brst_a' count:512 }".into(),
        "blocks_response { blocks:'one' blocks:'two' }".into(),
        format!("account_response {{ account:'brst_a' head:'{hash}' block_count:7 }}"),
        format!("frontiers_response {{ entries {{ account:'brst_a' head:'{hash}' }} }}"),
    ];
    let mut bodies = vec![
        format!("vote {{ {vote} }}"),
        format!("confirm_req {{ block_hashes:'{hash}' remaining_hops:3 }}"),
        format!("confirm_ack {{ {vote} }}"),
        "keepalive { peers:'127.0.0.1:7076' peers:'[::1]:7076' }".into(),
        format!("handshake {{ node_id:'brst_n' cookie:'{hash}' cookie_signature:'{signature}' params_hash:'{hash}' network:3 peering_port:7076 block_version:6 wire_version:{version} }}"),
        "verification_request { target:'brst_t' endorser:'brst_e' }".into(),
        format!("verification_vote {{ target:'brst_t' voter:'brst_v' vote:2 signature:'{signature}' }}"),
        format!("governance_proposal {{ proposal_hash:'{hash}' proposer:'brst_p' content:'abc' }}"),
        format!("governance_vote {{ proposal_hash:'{hash}' voter:'brst_v' vote:1 }}"),
        "telemetry_req {}".into(),
        format!("telemetry_ack {{ block_count:1 cemented_count:2 unchecked_count:3 account_count:4 bandwidth_cap:5 peer_count:6 protocol_version:30 uptime:8 genesis_hash:'{hash}' major_version:1 minor_version:2 patch_version:3 timestamp:12 params_hash:'{hash}' }}"),
        "checkpoint_sync { request { height:1 offset:0 } }".into(),
        "checkpoint_sync { tip_request { nonce:18446744073709551615 } }".into(),
        format!("checkpoint_sync {{ tip_observation {{ nonce:7 height:0 epoch:'{hash}' }} }}"),
        format!("checkpoint_sync {{ witness_request {{ start:'{hash}' offset:0 }} }}"),
        format!("checkpoint_sync {{ witness_request {{ start:'{hash}' offset:16384 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ witness_heads_request {{ starts:'{hash}' offset:0 }} }}"),
        format!("checkpoint_sync {{ witness_heads_request {{ starts:'{hash}' starts:'{}' offset:16384 digest:'{hash}' }} }}", "z".repeat(32)),
        format!("checkpoint_sync {{ witness_chunk {{ start:'{hash}' offset:16384 total:16387 digest:'{hash}' data:'abc' }} }}"),
        "checkpoint_sync { snapshot_query { minimum_height:10 } }".into(),
        format!("checkpoint_sync {{ snapshot_offer {{ epoch:'{hash}' height:10 total:16387 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ snapshot_request {{ epoch:'{hash}' offset:0 }} }}"),
        format!("checkpoint_sync {{ snapshot_request {{ epoch:'{hash}' offset:16384 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ snapshot_chunk {{ epoch:'{hash}' offset:16384 total:16387 digest:'{hash}' data:'abc' }} }}"),
        format!("checkpoint_sync {{ snapshot_request {{ epoch:'{hash}' offset:4294967296 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ snapshot_offer {{ epoch:'{hash}' height:1 total:18446744073709551615 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ snapshot_chunk {{ epoch:'{hash}' offset:4294967296 total:4294967299 digest:'{hash}' data:'abc' }} }}"),
        format!("checkpoint_sync {{ request {{ height:1 offset:16384 digest:'{hash}' }} }}"),
        format!("checkpoint_sync {{ chunk {{ height:1 offset:16384 total:16387 digest:'{hash}' data:'abc' }} }}"),
        format!("governance_content {{ request {{ proposal:'{hash}' offset:8 }} }}"),
        format!("governance_content {{ chunk {{ proposal:'{hash}' hash:'{hash}' length:11 offset:8 data:'abc' }} }}"),
    ];
    for body in bootstrap {
        bodies.push(format!("bootstrap {{ id:18446744073709551615 {body} }}"));
    }
    for body in [
        format!("offer {{ digest:'{hash}' total:3 }}"),
        format!("request {{ digest:'{hash}' offset:8 }}"),
        format!("chunk {{ digest:'{hash}' total:11 offset:8 data:'abc' }}"),
    ] {
        bodies.push(format!("checkpoint_agreement {{ context {{ network:3 genesis:'{hash}' parent:'{hash}' height:1 }} {body} }}"));
    }
    let block =
        burst_ledger::genesis::legacy_genesis_fixture(&burst_ledger::genesis::GenesisConfig {
            network: burst_types::NetworkId::Dev,
            creator: burst_types::WalletAddress::new("brst_schema_fixture"),
        });
    let bytes: String = bincode::serialize(&block)
        .unwrap()
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect();
    bodies.push(format!("block {{ data:'{bytes}' }}"));
    for body in bodies {
        let input = format!("wire_version:{version} {body}");
        let mut compiler = Command::new("protoc")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--encode=burst.wire.v33.Envelope", "wire.proto"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("install protoc to run schema conformance");
        compiler
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = compiler.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        use burst_types::TrafficClass::{Bulk, Control};
        let expected = if body.starts_with("checkpoint_sync {") {
            if body.contains("witness_chunk {") || body.contains("snapshot_chunk {")
                || body.starts_with("checkpoint_sync { chunk {") { Bulk } else { Control }
        } else if ["vote", "confirm_req", "confirm_ack", "keepalive", "handshake",
            "verification_vote", "governance_vote", "telemetry_req", "telemetry_ack",
            "checkpoint_agreement"].contains(&body.split_whitespace().next().unwrap()) {
            Control
        } else { Bulk };
        assert_eq!(WireMessage::outbound_class(&output.stdout), expected, "{body}");
        let decoded = WireMessage::decode(&output.stdout).unwrap();
        assert_eq!(
            decoded.encode().unwrap(),
            output.stdout,
            "schema drift: {body}"
        );
    }
}

#[test]
fn outbound_classification_rejects_truncated_and_noncanonical_headers() {
    use burst_types::TrafficClass::Bulk;
    let encoded = WireMessage::TelemetryReq.encode().unwrap();
    for end in 0..encoded.len() {
        assert_eq!(WireMessage::outbound_class(&encoded[..end]), Bulk);
    }
    let mut trailing = encoded.clone();
    trailing.extend_from_slice(&encoded);
    assert_eq!(WireMessage::outbound_class(&trailing), Bulk);
    let mut wrong_version = encoded;
    wrong_version[1] = 0;
    assert_eq!(WireMessage::outbound_class(&wrong_version), Bulk);
}

#[test]
fn previous_governance_protocol_is_not_silently_accepted() {
    let mut previous = WireMessage::TelemetryReq.encode().unwrap();
    assert_eq!(previous[1], burst_types::PROTOCOL_VERSION as u8);
    previous[1] = 36;
    assert!(WireMessage::decode(&previous).is_err());
    assert_eq!(WireMessage::outbound_class(&previous), burst_types::TrafficClass::Bulk);
}
