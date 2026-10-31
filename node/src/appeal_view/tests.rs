use super::*;
use burst_store::meta::MetaStore;
use burst_types::{ChallengeReason, Timestamp};

#[test]
fn only_latest_certified_challenge_is_exposed_and_storage_errors_stay_errors() {
    for format in [0, 1, 2] {
        check_latest_certified_challenge(format);
    }
}

fn check_latest_certified_challenge(format: u8) {
    let directory = tempfile::tempdir().unwrap();
    let store =
        Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let reader = Reader {
        store: store.clone(),
        initial_params: ProtocolParams::burst_defaults(),
    };
    let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
    assert_eq!(reader.read(&target, false).unwrap().outcome, None);
    let mut receipt = receipts::Receipt {
        source: BlockHash::new([3; 32]),
        target: target.clone(),
        completed_at: Timestamp::new(10),
        previous: None,
        rewards: vec![],
        verdict: receipts::Verdict::Challenge {
            admission: BlockHash::new([4; 32]),
            reason: ChallengeReason::Fraud,
            result: receipts::ChallengeVerdict::Upheld,
        },
    };
    let meta = store.meta_store();
    meta.put_meta(
        crate::endorsement_commit::SNAPSHOT_KEY,
        &burst_verification::VerificationOrchestrator::new()
            .snapshot()
            .encode()
            .unwrap(),
    )
    .unwrap();
    if format != 0 {
        let mut batch = store.tx_begin_write().unwrap();
        batch.migrate_verification_records().unwrap();
        if format == 2 { batch.normalize_verification_records().unwrap(); }
        batch.commit().unwrap();
    }
    meta.put_meta(
        &receipts::key(receipt.source),
        &bincode::serialize(&receipt).unwrap(),
    )
    .unwrap();
    meta.put_meta(&receipts::latest_key(&target), receipt.source.as_bytes())
        .unwrap();
    assert_eq!(reader.read(&target, true).unwrap().outcome, None);
    meta.put_meta(&receipts::certified_key(receipt.source), &[5; 32])
        .unwrap();
    let view = reader.read(&target, false).unwrap();
    assert!(!view.enabled);
    assert_eq!(view.outcome.unwrap().hash, receipt.source.to_string());
    meta.put_meta(&receipts::certified_key(receipt.source), &[0; 32])
        .unwrap();
    assert!(reader.read(&target, true).is_err());
    meta.put_meta(&receipts::certified_key(receipt.source), &[5; 32])
        .unwrap();
    receipt.verdict = receipts::Verdict::Verified;
    meta.put_meta(
        &receipts::key(receipt.source),
        &bincode::serialize(&receipt).unwrap(),
    )
    .unwrap();
    assert_eq!(reader.read(&target, true).unwrap().outcome, None);
    meta.put_meta(&receipts::latest_key(&target), b"broken")
        .unwrap();
    assert!(reader.read(&target, true).is_err());
}

#[test]
fn keyed_appeal_reads_validate_the_requested_row_and_format() {
    use burst_verification::orchestrator::records::VerificationRecord;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let reader = Reader { store: store.clone(), initial_params: ProtocolParams::burst_defaults() };
    let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
    let mut state = burst_verification::VerificationOrchestrator::new();
    state.initialize_genesis(&target);
    let bytes = state.snapshot().encode().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &bytes).unwrap();
    batch.migrate_verification_records().unwrap();
    batch.commit().unwrap();
    assert!(!reader.read(&target, true).unwrap().challenge_active);
    let meta = store.meta_store();
    let key = VerificationRecord::target_storage_key(&target).unwrap();
    let valid = meta.get_meta(&key).unwrap();
    meta.put_meta(&key, b"invalid").unwrap();
    assert!(reader.read(&target, true).is_err());
    meta.put_meta(&key, &valid).unwrap();
    meta.put_meta("verification_records_format", &[2]).unwrap();
    assert!(reader.read(&target, true).is_err());
    meta.put_meta("verification_records_format", &[1]).unwrap();
    assert!(reader.read(&target, true).is_ok());
    meta.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &bytes).unwrap();
    assert!(reader.read(&target, true).is_err());
}


#[test]
fn normalized_appeal_header_rejects_malformed_and_wrong_identity() {
    use burst_verification::orchestrator::records::normalized;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let reader = Reader { store: store.clone(), initial_params: ProtocolParams::burst_defaults() };
    let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
    let other = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
    let mut engine = burst_verification::VerificationOrchestrator::new();
    engine.initialize_genesis(&target);
    engine.initialize_genesis(&other);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &engine.snapshot().encode().unwrap()).unwrap();
    batch.normalize_verification_records().unwrap();
    batch.commit().unwrap();
    assert!(!reader.read(&target, true).unwrap().challenge_active);
    let meta = store.meta_store();
    let key = normalized::target_header_key(&target).unwrap();
    let valid = meta.get_meta(&key).unwrap();
    for invalid in [b"invalid".to_vec(), meta.get_meta(&normalized::target_header_key(&other).unwrap()).unwrap(),
        [valid.as_slice(), &[0]].concat()] {
        meta.put_meta(&key, &invalid).unwrap();
        assert!(reader.read(&target, true).is_err());
    }
    meta.put_meta(&key, &valid).unwrap();
    assert!(!reader.read(&target, true).unwrap().challenge_active);
}
