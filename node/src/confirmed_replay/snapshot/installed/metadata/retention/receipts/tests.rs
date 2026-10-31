use super::*;
use burst_store::meta::MetaStore;
use burst_types::{BlockHash, Timestamp, WalletAddress};

fn hash(n: u8) -> BlockHash {
    BlockHash::new([n; 32])
}
fn fixture() -> (tempfile::TempDir, LmdbStore, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    // These raw rows test reference extraction, not recovery authentication.
    #[derive(serde::Serialize)]
    struct StakeRow {
        block: BlockHash,
        amount: u128,
        settled_by: Option<BlockHash>,
    }
    let stake = format!(
        "verification_stake/v2/vote/brst_target/brst_staker/{}",
        hash(1)
    );
    store
        .meta_store()
        .put_meta(
            &stake,
            &bincode::serialize(&StakeRow {
                block: hash(2),
                amount: 3,
                settled_by: Some(hash(4)),
            })
            .unwrap(),
        )
        .unwrap();
    let outcome = crate::verification_commit::receipts::Receipt {
        source: hash(4),
        target: WalletAddress::new("brst_target"),
        completed_at: Timestamp::new(5),
        previous: None,
        verdict: crate::verification_commit::receipts::Verdict::Verified,
        rewards: vec![],
    };
    store
        .meta_store()
        .put_meta(
            &crate::verification_commit::receipts::key(hash(4)),
            &bincode::serialize(&outcome).unwrap(),
        )
        .unwrap();
    for (key, bytes) in [
        (
            format!("verification_outcome_sources/v1/{}", hash(4)),
            bincode::serialize(&vec![hash(1), hash(4)]).unwrap(),
        ),
        (
            format!("verification_transition_sources/v1/{}", hash(2)),
            bincode::serialize(&vec![hash(1), hash(2)]).unwrap(),
        ),
        (
            "verification_identity_source/v1/brst_target".into(),
            hash(8).as_bytes().to_vec(),
        ),
        (
            format!("verification_identity_binding/v1/{}", hash(6)),
            hash(8).as_bytes().to_vec(),
        ),
        (
            "verification_identity_bound/v1/brst_target".into(),
            hash(8).as_bytes().to_vec(),
        ),
        (
            "verification_latest_outcome/v1/brst_target".into(),
            hash(4).as_bytes().to_vec(),
        ),
        (
            format!("verification_certified_outcome/v1/{}", hash(4)),
            hash(9).as_bytes().to_vec(),
        ),
    ] {
        store.meta_store().put_meta(&key, &bytes).unwrap();
    }
    let rules = burst_types::ProtocolParams::default();
    let activation = crate::verification_activation::Activation {
        target: WalletAddress::new("brst_target"),
        context: hash(11),
        selection: hash(12),
        params_hash: rules.params_hash(),
        rules_at_activation: rules,
        started_at: 13,
        deadline: 14,
    };
    let completion = crate::verification_deadline::Completion {
        activation: activation.clone(),
        lifecycle: hash(15),
        completed_at: 16,
        absent: vec![WalletAddress::new("brst_absent")],
    };
    for (key, bytes) in [
        (
            crate::verification_activation::key(hash(11)),
            bincode::serialize(&activation).unwrap(),
        ),
        (
            crate::verification_deadline::key(hash(11)),
            bincode::serialize(&completion).unwrap(),
        ),
        (
            crate::verification_deadline::voter_key(&activation.target, &completion.absent[0]),
            hash(11).as_bytes().to_vec(),
        ),
    ] {
        store.meta_store().put_meta(&key, &bytes).unwrap();
    }
    (dir, store, stake)
}

#[test]
fn receipt_pages_pin_both_families_and_never_treat_empty_phase_as_completion() {
    let (_dir, store, stake) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader = ReceiptReferenceReader::new(&store, &view);
    store.meta_store().delete_meta(&stake).unwrap();
    store
        .meta_store()
        .put_meta(
            &format!("verification_certified_outcome/v1/{}", hash(4)),
            hash(10).as_bytes(),
        )
        .unwrap();
    store
        .meta_store()
        .put_meta(
            "verification_default/v1/brst_target/brst_absent",
            hash(17).as_bytes(),
        )
        .unwrap();
    let mut checkpoints = Vec::new();
    let mut parameters = Vec::new();
    let mut boundaries = Vec::new();
    let mut contexts = Vec::new();
    let mut outcomes = Vec::new();
    let mut blocks = Vec::new();
    let mut rows = 0;
    let mut done = false;
    for _ in 0..(2 * retention::PREFIXES.len() + 1) {
        let page = reader
            .visit_next_page(1, 4096, |r| {
                match r {
                    Reference::Block(hash) => blocks.push(hash),
                    Reference::Checkpoint(hash) => checkpoints.push(hash),
                    Reference::CommitteeContext(hash) => contexts.push(hash),
                    Reference::Outcome(hash) => outcomes.push(hash),
                    Reference::Parameters(hash) => parameters.push(hash),
                    Reference::Boundary(time) => boundaries.push(time.as_secs()),
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        rows += page.rows;
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(rows, 12);
    assert_eq!(blocks, [2, 1, 4, 2, 1, 2, 8, 6, 8, 8, 15].map(hash));
    assert_eq!(contexts, [1, 11, 11, 11].map(hash));
    assert_eq!(outcomes, [hash(4); 5]);
    assert_eq!(checkpoints, [9, 12, 12].map(hash));
    assert_eq!(
        parameters,
        [burst_types::ProtocolParams::default().params_hash(); 2]
    );
    assert_eq!(boundaries, [5, 13, 14, 13, 14, 16]);
    drop(reader);
    drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader = ReceiptReferenceReader::new(&store, &view);
    let empty = reader
        .visit_next_page(100, 512, |_| panic!("no stakes"))
        .unwrap();
    assert_eq!(empty.rows, 0);
    assert!(!empty.done);
    let outcome = reader.visit_next_page(100, 512, |_| Ok(())).unwrap();
    assert_eq!(outcome.rows, 1);
    assert!(!outcome.done);
    let mut later_rows = 0;
    let mut latest_checkpoints = Vec::new();
    let mut latest_contexts = Vec::new();
    let mut finished = false;
    for _ in 0..retention::PREFIXES.len() {
        let page = reader
            .visit_next_page(100, 4096, |r| {
                match r {
                    Reference::Checkpoint(hash) => latest_checkpoints.push(hash),
                    Reference::CommitteeContext(hash) => latest_contexts.push(hash),
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
        later_rows += page.rows;
        if page.done {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert_eq!(later_rows, 10);
    assert_eq!(latest_checkpoints, [10, 12, 12].map(hash));
    assert_eq!(latest_contexts, [11, 11, 17].map(hash));
}

#[test]
fn receipt_pages_invalidate_on_callback_budget_and_unsupported_schema_errors() {
    let (_dir, store, _) = fixture();
    let view = store.tx_begin_read().unwrap();
    for (rows, bytes) in [(0, 512), (101, 512), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut reader = ReceiptReferenceReader::new(&store, &view);
        assert!(reader
            .visit_next_page(rows, bytes, |_| panic!("invalid budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 512, |_| Ok(())).is_err());
    }
    let mut reader = ReceiptReferenceReader::new(&store, &view);
    let mut calls = 0;
    assert!(matches!(reader.visit_next_page(100, 512, |_| {
        calls += 1; Err(StoreError::Backend("stop".into()))
    }), Err(StoreError::Backend(reason)) if reason == "stop"));
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(100, 512, |_| Ok(())).is_err());
    drop(reader);
    drop(view);
    store
        .meta_store()
        .put_meta("verification_stake/v1/unsupported", b"old")
        .unwrap();
    let view = store.tx_begin_read().unwrap();
    let mut reader = ReceiptReferenceReader::new(&store, &view);
    assert!(reader
        .visit_next_page(100, 512, |_| panic!("unknown schema"))
        .is_err());
    assert!(reader.visit_next_page(100, 512, |_| Ok(())).is_err());
}
