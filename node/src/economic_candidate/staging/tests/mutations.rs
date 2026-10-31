use super::*;
use burst_store::pending::PendingProvenance;

#[test]
fn missing_challenge_target_rejects_without_mutation_or_panic() {
    let (brn, trst, mut block, account) = fixture(1);
    let params = ProtocolParams::default();
    block.block_type = BlockType::Challenge;
    block.brn_spent = 10;
    block.link = BlockHash::ZERO;
    let mut staged = StagedEngines::new(&brn, &trst);
    assert_eq!(
        staged
            .apply(&block, context(&account, &params))
            .err()
            .as_deref(),
        Some("challenge block requires a target wallet")
    );
    let (brn, trst) = staged.into_staged();
    assert!(brn.is_none() && trst.is_none());
}

#[test]
fn transfers_copy_only_trst_and_match_full_evaluation() {
    let (brn, trst, template, recipient) = fixture(16);
    let params = ProtocolParams::default();
    let origin = TxHash::new(burst_crypto::blake2b_256(&0u64.to_le_bytes()));
    let sender = burst_crypto::derive_address(&PublicKey(*origin.as_bytes()));
    let mut account = recipient.clone();
    account.address = sender.clone();
    account.trst_balance = 100;
    let pending = PendingInfo {
        source: sender.clone(),
        amount: 10,
        timestamp: template.timestamp,
        provenance: vec![PendingProvenance {
            basis: Default::default(),
            amount: 10,
            origin,
            origin_wallet: sender.clone(),
            origin_timestamp: Timestamp::new(100),
            effective_origin_timestamp: Timestamp::new(100),
            revoked_amounts: Default::default(),
        }],
        return_credit: false,
    };
    let before = snapshot(&brn, &trst);
    for kind in [BlockType::Send, BlockType::Receive, BlockType::Open] {
        let mut block = template.clone();
        let sending = kind == BlockType::Send;
        block.block_type = kind;
        block.account = if sending {
            sender.clone()
        } else {
            template.account.clone()
        };
        block.trst_balance = if sending { 90 } else { 10 };
        block.link =
            BlockHash::new(burst_crypto::decode_address(template.account.as_str()).unwrap());
        block.origin = origin;
        block.hash = block.compute_hash();
        let make_context = || EconomicContext {
            account: Some(if sending { &account } else { &recipient }),
            previous_brn_spent: 0,
            pending: if sending { None } else { Some(&pending) },
            params: &params,
        };
        let mut full_brn = brn.clone();
        let mut full_trst = trst.clone();
        let expected =
            economic_candidate::apply(&block, make_context(), &mut full_brn, &mut full_trst)
                .unwrap();
        let mut staged = StagedEngines::new(&brn, &trst);
        let actual = staged.apply(&block, make_context()).unwrap();
        assert!(staged
            .trst()
            .merger_graph
            .shares_storage_with(&trst.merger_graph));
        assert_eq!(
            format!("{:?}", actual.effect),
            format!("{:?}", expected.effect)
        );
        assert_eq!(
            format!("{:?}", actual.pending),
            format!("{:?}", expected.pending)
        );
        let (next_brn, next_trst) = staged.into_staged();
        assert!(next_brn.is_none(), "TRST transfer copied BRN state");
        assert_eq!(
            snapshot(&brn, &next_trst.unwrap()),
            snapshot(&full_brn, &full_trst)
        );
        assert_eq!(snapshot(&brn, &trst), before);
    }

    // The expiry boundary already returned this value. Receiving its credit
    // consumes ledger pending state, but must not deliver TRST a second time.
    let mut credit = pending;
    credit.return_credit = true;
    credit.source = template.account.clone();
    let mut block = template;
    block.block_type = BlockType::Receive;
    block.trst_balance = 10;
    let mut context = context(&recipient, &params);
    context.pending = Some(&credit);
    let mut staged = StagedEngines::new(&brn, &trst);
    staged.apply(&block, context).unwrap();
    let (next_brn, next_trst) = staged.into_staged();
    assert!(next_brn.is_none() && next_trst.is_none());
    assert_eq!(snapshot(&brn, &trst), before);
}

#[test]
fn only_changed_merger_indexes_are_serialized_and_persisted() {
    let (brn, trst, _, _) = fixture(16);
    let before = snapshot(&brn, &trst);
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
    // A sentinel exposes any attempt to rewrite the unchanged graph. This
    // store is used only to observe persistence, never to start a node.
    batch.put_meta("merger_graph", b"unchanged-graph").unwrap();
    batch
        .put_meta(TrstEngine::meta_key(), b"unchanged-wallets")
        .unwrap();
    batch.commit().unwrap();
    let mut staged = StagedEngines::new(&brn, &trst);
    let _ = staged.trst_mut();
    let mut batch = store.tx_begin_write().unwrap();
    staged.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store.meta_store().get_meta("merger_graph").unwrap(),
        b"unchanged-graph"
    );
    staged
        .trst_mut()
        .merger_graph
        .record_merge(burst_trst::MergeNode {
            merge_tx: TxHash::new([99; 32]),
            source_origins: vec![burst_trst::MergeSource {
                origin: TxHash::new([98; 32]),
                amount: 10,
            }],
            total_amount: 10,
            revoked_contribs: Default::default(),
        });
    assert!(!staged
        .trst()
        .merger_graph
        .shares_storage_with(&trst.merger_graph));
    let mut batch = store.tx_begin_write().unwrap();
    staged.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let bytes = store.meta_store().get_meta("merger_graph").unwrap();
    assert_eq!(bytes, staged.trst().merger_graph.to_bytes());
    assert!(burst_trst::MergerGraph::from_bytes(&bytes)
        .unwrap()
        .contains_merge(&TxHash::new([99; 32])));
    assert_eq!(
        store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
        b"unchanged-wallets"
    );
    assert_eq!(snapshot(&brn, &trst), before);
}

#[test]
fn brn_only_effects_never_copy_or_persist_trst() {
    let (brn, trst, template, account) = fixture(16);
    let params = ProtocolParams::default();
    let before = snapshot(&brn, &trst);
    for kind in [
        BlockType::Burn,
        BlockType::Endorse,
        BlockType::Challenge,
        BlockType::VerificationVote,
    ] {
        let mut block = template.clone();
        block.block_type = kind;
        block.brn_spent = 10;
        if block.block_type != BlockType::Burn {
            block.link =
                BlockHash::new(burst_crypto::decode_address(template.account.as_str()).unwrap());
        }
        let mut full_brn = brn.clone();
        let mut full_trst = trst.clone();
        let expected = economic_candidate::apply(
            &block,
            context(&account, &params),
            &mut full_brn,
            &mut full_trst,
        )
        .unwrap();
        let mut staged = StagedEngines::new(&brn, &trst);
        let actual = staged.apply(&block, context(&account, &params)).unwrap();
        assert_eq!(
            format!("{:?}", actual.effect),
            format!("{:?}", expected.effect)
        );

        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        // Sentinels expose accidental writes even when real snapshots would
        // compare equal and LMDB's undo recorder would skip them.
        batch
            .put_meta(TrstEngine::meta_key(), b"untouched-wallets")
            .unwrap();
        batch.put_meta("merger_graph", b"untouched-graph").unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        staged.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            b"untouched-wallets"
        );
        assert_eq!(
            store.meta_store().get_meta("merger_graph").unwrap(),
            b"untouched-graph"
        );
        let (next_brn, next_trst) = staged.into_staged();
        assert!(next_trst.is_none(), "BRN-only effect copied TRST");
        assert_eq!(
            snapshot(&next_brn.unwrap(), &trst),
            snapshot(&full_brn, &full_trst)
        );
        assert_eq!(snapshot(&brn, &trst), before);
    }
}
