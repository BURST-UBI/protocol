use super::*;
use burst_store::meta::MetaStore;
use burst_types::{ChallengeReason, Timestamp};

#[test]
fn only_latest_certified_challenge_is_exposed_and_storage_errors_stay_errors() {
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
