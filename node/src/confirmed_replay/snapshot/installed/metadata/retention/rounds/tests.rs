use super::*;
use burst_consensus::checkpoint::{CheckpointAnchor, HumanDelegation, HumanRoster};
use burst_consensus::checkpoint_round::RoundState;
use burst_store::meta::MetaStore;
use burst_types::WalletAddress;

fn fixture() -> (tempfile::TempDir, LmdbStore, CheckpointAnchor, Vec<String>) {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let keys: Vec<_> = [1, 2]
        .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
        .into();
    let roster = HumanRoster::new(
        keys.iter()
            .map(|key| {
                let account = burst_crypto::derive_address(&key.public);
                HumanDelegation {
                    human: account.clone(),
                    representative: account,
                }
            })
            .collect(),
    )
    .unwrap();
    let anchor =
        CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([9; 32]), 100, roster, 6700)
            .unwrap();
    for key in &keys {
        let (name, bytes) =
            retention::empty_round_fixture(&anchor, &burst_crypto::derive_address(&key.public));
        store.meta_store().put_meta(&name, &bytes).unwrap();
    }
    let names = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_round/")
        .unwrap()
        .into_iter()
        .map(|(key, _)| String::from_utf8(key).unwrap())
        .collect();
    (directory, store, anchor, names)
}

#[test]
fn round_pages_preserve_pinned_states_and_charge_complete_rows() {
    let (_dir, store, anchor, names) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        RoundJournalReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    store.meta_store().delete_meta(&names[0]).unwrap();
    store
        .meta_store()
        .put_meta(&names[1], b"damaged after pin")
        .unwrap();
    let mut observed = Vec::new();
    let mut done = false;
    for _ in 0..3 {
        let page = reader
            .visit_next_page(1, 4096, |row| {
                assert_eq!(row.height, 1);
                assert_eq!(
                    row.state,
                    &RoundState::new(&anchor, WalletAddress::new(row.representative)).unwrap()
                );
                assert_eq!(row.manifests, Some(&[][..]));
                observed.push(row.key.to_owned());
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        if page.rows != 0 {
            assert!(page.bytes > observed.last().unwrap().len());
        }
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(observed, names);
    assert!(
        reader
            .visit_next_page(1, 4096, |_| panic!("finished"))
            .unwrap()
            .done
    );
    drop(reader);
    drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        RoundJournalReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    assert!(reader
        .visit_next_page(100, 4096, |_| panic!("damaged row"))
        .is_err());
    assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
}

#[test]
fn round_pages_reject_budgets_unknown_versions_and_poison_callback_errors() {
    let (_dir, store, anchor, _) = fixture();
    let view = store.tx_begin_read().unwrap();
    for (rows, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut reader =
            RoundJournalReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
        assert!(reader
            .visit_next_page(rows, bytes, |_| panic!("invalid budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    }
    let mut reader =
        RoundJournalReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    let mut calls = 0;
    assert!(reader
        .visit_next_page(100, 4096, |_| {
            calls += 1;
            Err(invalid("stop"))
        })
        .is_err());
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
    drop(reader);
    drop(view);
    store
        .meta_store()
        .put_meta("checkpoint_round/v2/unknown", b"unknown")
        .unwrap();
    let view = store.tx_begin_read().unwrap();
    let mut reader =
        RoundJournalReader::new(&store, &view, anchor.network(), anchor.genesis_hash());
    assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
    assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
}
