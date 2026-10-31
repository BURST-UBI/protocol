use super::*;
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_store::meta::{encode_payment_vote_limit, MetaStore};

fn fixture() -> (
    tempfile::TempDir,
    LmdbStore,
    CheckpointAnchor,
    String,
    Vec<(String, Vec<u8>)>,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &burst_types::ProtocolParams::default(),
    )
    .unwrap();
    let signer =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public).to_string();
    let epoch = anchor.hash();
    let snapshot =
        SnapshotManifest::from_commitment(&anchor, 234, BlockHash::new([55; 32])).unwrap();
    let mut closed = BlockHash::new([56; 32]).as_bytes().to_vec();
    closed.extend_from_slice(epoch.as_bytes());
    let rows = vec![
        (
            format!(
                "consensus_final/v2/{epoch}/{signer}/{signer}/{}",
                BlockHash::ZERO
            ),
            vec![77; 32],
        ),
        (
            format!("consensus_fence/v2/{epoch}/{signer}"),
            epoch.as_bytes().to_vec(),
        ),
        (
            format!("consensus_time/v1/{epoch}/{signer}"),
            10u64.to_be_bytes().to_vec(),
        ),
        (
            format!("consensus_governance/v1/{epoch}/{signer}"),
            20u64.to_be_bytes().to_vec(),
        ),
        (
            format!("consensus_sequence/v1/{signer}"),
            12u64.to_be_bytes().to_vec(),
        ),
        ("payment_epoch/v1".to_owned(), epoch.as_bytes().to_vec()),
        (
            "payment_epoch/v1/cutoff".to_owned(),
            30u64.to_be_bytes().to_vec(),
        ),
        (
            "payment_epoch/v1/vote_limit".to_owned(),
            encode_payment_vote_limit(epoch, None),
        ),
        (
            "payment_epoch/v1/counting_limit".to_owned(),
            encode_payment_vote_limit(epoch, Some(40)),
        ),
        (format!("payment_epoch/v1/closed/{epoch}"), closed),
        (
            format!(
                "snapshot_attestation/v1/dev/{}/{epoch}/{signer}",
                anchor.genesis_hash()
            ),
            bincode::serialize(&snapshot).unwrap(),
        ),
    ];
    for (key, bytes) in &rows {
        store.meta_store().put_meta(key, bytes).unwrap();
    }
    (directory, store, anchor, signer, rows)
}

#[test]
fn signer_pages_preserve_all_record_kinds_and_pinned_counter_values() {
    let (_dir, store, anchor, signer, rows) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        SignerRecordReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    let sequence_key = format!("consensus_sequence/v1/{signer}");
    store
        .meta_store()
        .put_meta(&sequence_key, &u64::MAX.to_be_bytes())
        .unwrap();
    let epoch = anchor.hash();
    let mut seen = [false; 11];
    let mut charged = 0;
    let mut done = false;
    for _ in 0..40 {
        let page = reader
            .visit_next_page(1, 4096, |key, record| {
                let n = rows.iter().position(|(name, _)| name == key).unwrap();
                assert!(!seen[n]);
                seen[n] = true;
                let expected = match n {
                    0 => SignerRecord::Payment(PaymentPromise::Final {
                        epoch,
                        signer: &signer,
                        account: &signer,
                        root: BlockHash::ZERO,
                        block: BlockHash::new([77; 32]),
                    }),
                    1 => SignerRecord::Payment(PaymentPromise::Fence {
                        epoch,
                        signer: &signer,
                    }),
                    2 => SignerRecord::Time {
                        epoch,
                        signer: &signer,
                        value: 10,
                    },
                    3 => SignerRecord::Governance {
                        epoch,
                        signer: &signer,
                        value: 20,
                    },
                    4 => SignerRecord::Sequence {
                        signer: &signer,
                        value: 12,
                    },
                    5 => SignerRecord::Epoch(epoch),
                    6 => SignerRecord::Cutoff(30),
                    7 => SignerRecord::VoteLimit { epoch, limit: None },
                    8 => SignerRecord::CountingLimit {
                        epoch,
                        limit: Some(40),
                    },
                    9 => SignerRecord::ClosedEpoch {
                        from: epoch,
                        to: BlockHash::new([56; 32]),
                    },
                    10 => SignerRecord::Snapshot {
                        signer: &signer,
                        manifest: SnapshotManifest::from_commitment(
                            &anchor,
                            234,
                            BlockHash::new([55; 32]),
                        )
                        .unwrap(),
                    },
                    _ => unreachable!(),
                };
                assert_eq!(record, expected);
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        charged += page.bytes;
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done && seen.into_iter().all(|v| v));
    assert_eq!(
        charged,
        rows.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
    );
    assert!(
        reader
            .visit_next_page(1, 4096, |_, _| panic!("finished"))
            .unwrap()
            .done
    );
    drop(reader);
    drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        SignerRecordReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    let mut current = None;
    loop {
        let page = reader
            .visit_next_page(100, 4096, |_, record| {
                if let SignerRecord::Sequence { value, .. } = record {
                    current = Some(value);
                }
                Ok(())
            })
            .unwrap();
        if page.done {
            break;
        }
    }
    assert_eq!(current, Some(u64::MAX));
}

#[test]
fn signer_pages_reject_unknown_namespaces_malformed_records_and_callback_failure() {
    let (_dir, store, anchor, signer, rows) = fixture();
    let epoch = anchor.hash();
    let mut bad: Vec<_> = PREFIXES
        .iter()
        .map(|p| (format!("{p}v99/unknown"), vec![1]))
        .collect();
    bad.extend([
        (rows[0].0.clone(), vec![0; 32]),
        (rows[1].0.clone(), vec![9; 32]),
        (rows[2].0.clone(), vec![1]),
        (rows[4].0.clone(), 0u64.to_be_bytes().to_vec()),
        (rows[7].0.clone(), vec![1; 33]),
        (rows[9].0.clone(), vec![9; 64]),
        (rows[10].0.clone(), vec![1]),
        (
            format!(
                "consensus_final/v2/{epoch}/{signer}/bad/{}",
                BlockHash::ZERO
            ),
            vec![7; 32],
        ),
        (
            format!(
                "consensus_final/v2/{epoch}/{signer}/{signer}/{}",
                "A".repeat(64)
            ),
            vec![7; 32],
        ),
    ]);
    for (key, bytes) in bad {
        store.meta_store().put_meta(&key, &bytes).unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader =
            SignerRecordReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
        let mut failed = false;
        for _ in 0..40 {
            match reader.visit_next_page(100, 4096, |_, _| Ok(())) {
                Err(_) => {
                    failed = true;
                    break;
                }
                Ok(page) => {
                    if page.done {
                        break;
                    }
                }
            }
        }
        assert!(failed, "accepted {key}");
        assert!(reader.visit_next_page(100, 4096, |_, _| Ok(())).is_err());
        drop(reader);
        drop(view);
        if let Some((_, old)) = rows.iter().find(|(name, _)| name == &key) {
            store.meta_store().put_meta(&key, old).unwrap();
        } else {
            store.meta_store().delete_meta(&key).unwrap();
        }
    }
    let view = store.tx_begin_read().unwrap();
    for (count, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut reader =
            SignerRecordReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
        assert!(reader
            .visit_next_page(count, bytes, |_, _| panic!("budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 4096, |_, _| Ok(())).is_err());
    }
    let mut reader =
        SignerRecordReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    let mut called = 0;
    assert!(reader
        .visit_next_page(100, 4096, |_, _| {
            called += 1;
            Err(invalid("stop"))
        })
        .is_err());
    assert_eq!(called, 1);
    assert!(reader.visit_next_page(100, 4096, |_, _| Ok(())).is_err());
}
