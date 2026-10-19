use super::*;
use crate::{governance_commit::CandidateGovernance, ledger_bridge::EconomicResult};
use burst_governance::content::{EncodedContent, MAX_AMENDMENT_CHARS};
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore, BrnStore};
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams, Timestamp, TxHash};
use burst_wallet_core::{
    governance_content::build_signed_proposal, transaction_builder::AccountState,
};

fn content() -> ProposalContent {
    ProposalContent::ConstitutionalDiff {
        title: "共同の憲法".into(),
        operations: vec![burst_governance::proposal::AmendmentOp::AddArticle {
            title: "Equal rights".into(),
            text: "🌍".repeat(MAX_AMENDMENT_CHARS),
        }],
    }
}

fn proposal() -> (StateBlock, EncodedContent) {
    let params = ProtocolParams::default();
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let state = AccountState {
        signing_grant: burst_types::BlockHash::ZERO,
        payment_epoch: genesis.hash,
        head: genesis.hash,
        block_count: 1,
        representative: genesis.account,
        brn_balance: 0,
        trst_balance: 0,
        release_sources: Vec::new(),
    };
    build_signed_proposal(
        &state,
        &burst_crypto::keypair_from_seed(&[0; 32]),
        &content(),
        params.params_hash(),
        Timestamp::new(10),
    )
    .unwrap()
}

fn initialize(store: &LmdbStore) {
    let genesis = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
    let mut account =
        crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
            .unwrap();
    account.confirmation_height = 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &genesis.hash,
            &bincode::serialize(&genesis).unwrap(),
            &genesis.account,
            1,
        )
        .unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.put_frontier(&genesis.account, &genesis.hash).unwrap();
    crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
    batch.commit().unwrap();
}

fn candidate() -> CandidateGovernance {
    CandidateGovernance {
        network: NetworkId::Dev,
        genesis: crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default()).hash,
        governance: burst_governance::GovernanceEngine::new(),
        delegation: burst_governance::DelegationEngine::default(),
        constitution: burst_consti::ConstiEngine::bootstrap(),
        params: ProtocolParams::default(),
    }
}

fn apply(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    state: &mut CandidateGovernance,
) -> Result<(), String> {
    let mut brn = burst_brn::BrnEngine::new();
    let mut trst = burst_trst::TrstEngine::new();
    let effect = crate::ledger_bridge::process_block_economics(
        block,
        &mut brn,
        &mut trst,
        block.timestamp,
        state.params.trst_expiry_secs,
        0,
    );
    assert!(matches!(
        effect,
        EconomicResult::GovernanceProposal { content: None, .. }
    ));
    state.apply(batch, block, &effect, &mut brn, &mut trst)
}

#[test]
fn signed_manifest_authenticates_full_content_and_all_signing_context() {
    let (block, encoded) = proposal();
    let manifest = authenticate(&block).unwrap();
    assert_eq!(manifest, encoded.manifest());
    assert_eq!(
        manifest.decode_verified(encoded.bytes()).unwrap(),
        content()
    );
    assert!(block.fits_p2p_message());
    assert!(encoded.bytes().len() > burst_types::MAX_P2P_MESSAGE_BYTES);
    for mutation in 0..8 {
        let mut changed = block.clone();
        match mutation {
            0 => changed.link = BlockHash::new([7; 32]),
            1 => changed.transaction = TxHash::new([7; 32]),
            2 => changed.origin = TxHash::ZERO,
            3 => {
                changed.account =
                    burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[9; 32]).public)
            }
            4 => changed.payment_epoch = BlockHash::new([8; 32]),
            5 => changed.params_hash = BlockHash::new([9; 32]),
            6 => changed.previous = BlockHash::new([10; 32]),
            7 => changed.timestamp = Timestamp::new(11),
            _ => unreachable!(),
        }
        changed.hash = changed.compute_hash();
        assert!(authenticate(&changed).is_err());
    }
    for bad_length in [TxHash::ZERO, TxHash::new([1; 32])] {
        let mut malformed = block.clone();
        malformed.transaction = bad_length;
        malformed.hash = malformed.compute_hash();
        malformed.signature = burst_crypto::sign_message(
            malformed.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
        );
        assert!(
            authenticate(&malformed).is_err(),
            "even a signer must use canonical bounded lengths"
        );
    }
    let mut corrupt = encoded.bytes().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(manifest.decode_verified(&corrupt).is_err());
}

#[test]
fn content_block_and_governance_commit_abort_reopen_and_rewind_together() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    initialize(&store);
    let (block, encoded) = proposal();
    let account = store.account_store().get_account(&block.account).unwrap();
    for commit in [false, true] {
        let mut state = candidate();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::stage_block(&mut batch, &block, Some(&account), 0, None, None)
            .unwrap();
        stage(&mut batch, &block, encoded.bytes()).unwrap();
        apply(&mut batch, &block, &mut state).unwrap();
        assert_eq!(
            state
                .governance
                .get_proposal(&block.hash.into_tx_hash())
                .unwrap()
                .content,
            content()
        );
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        assert_eq!(store.block_store().get_block(&block.hash).is_ok(), commit);
        assert_eq!(
            store.meta_store().get_meta(&key(block.hash)).is_ok(),
            commit
        );
        assert_eq!(
            store
                .brn_store()
                .get_meta(b"governance_engine")
                .unwrap()
                .is_some(),
            commit
        );
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let restored: burst_governance::GovernanceEngine = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        restored
            .get_proposal(&block.hash.into_tx_hash())
            .unwrap()
            .content,
        content()
    );
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(load(&batch, &block).unwrap(), content());
    drop(batch);

    // A separately committed sidecar must not masquerade as atomic admission.
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta("unrelated-write", b"must not commit")
        .unwrap();
    assert!(stage(&mut batch, &block, encoded.bytes()).is_err());
    assert!(batch.commit().is_err());
    assert!(store.meta_store().get_meta("unrelated-write").is_err());

    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block.hash).unwrap();
    batch.commit().unwrap();
    assert!(store.block_store().get_block(&block.hash).is_err());
    assert!(store.meta_store().get_meta(&key(block.hash)).is_err());
    assert!(store
        .brn_store()
        .get_meta(b"governance_engine")
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .account_store()
            .get_account(&block.account)
            .unwrap()
            .head,
        block.previous
    );
}

#[test]
fn missing_or_wrong_content_cannot_publish_a_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    initialize(&store);
    let (block, encoded) = proposal();
    let account = store.account_store().get_account(&block.account).unwrap();
    for poison in [false, true] {
        let mut state = candidate();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::stage_block(&mut batch, &block, Some(&account), 0, None, None)
            .unwrap();
        if poison {
            let mut bytes = encoded.bytes().to_vec();
            bytes[0] ^= 1;
            assert!(stage(&mut batch, &block, &bytes).is_err());
            // Bypass the staging helper to model corrupt local projection data.
            batch.put_meta(&key(block.hash), &bytes).unwrap();
        }
        assert!(apply(&mut batch, &block, &mut state).is_err());
        assert!(state
            .governance
            .get_proposal(&block.hash.into_tx_hash())
            .is_none());
        drop(batch);
        assert!(store.block_store().get_block(&block.hash).is_err());
        assert!(store.meta_store().get_meta(&key(block.hash)).is_err());
    }
}
