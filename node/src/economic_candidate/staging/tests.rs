use super::*;
use crate::{economic_candidate, genesis_state, ledger_bridge::EconomicResult, ledger_updater};
use burst_brn::BrnWalletState;
use burst_ledger::BlockType;
use burst_store::{account::AccountInfo, meta::MetaStore, pending::PendingInfo};
use burst_store_lmdb::LmdbStore;
use burst_types::{
    BlockHash, NetworkId, ProtocolParams, PublicKey, Timestamp, TxHash, WalletState,
};
use std::time::Instant;

mod mutations;
mod persistence;

fn fixture(wallets: usize) -> (BrnEngine, TrstEngine, StateBlock, AccountInfo) {
    let mut brn = BrnEngine::with_rate(1_000_000, Timestamp::EPOCH);
    let mut trst = TrstEngine::new();
    let mut block = genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
    block.timestamp = Timestamp::new(200);
    block.payment_epoch = BlockHash::new([1; 32]);
    let mut account = AccountInfo::unopened(block.account.clone());
    account.state = WalletState::Verified;
    account.verified_at = Some(Timestamp::EPOCH);
    brn.track_wallet(block.account.clone(), BrnWalletState::new(Timestamp::EPOCH));
    for n in 0..wallets {
        let hash = burst_crypto::blake2b_256(&(n as u64).to_le_bytes());
        let address = burst_crypto::derive_address(&PublicKey(hash));
        brn.track_wallet(address.clone(), BrnWalletState::new(Timestamp::EPOCH));
        let token = trst
            .mint(
                TxHash::new(hash),
                address.clone(),
                100,
                address,
                Timestamp::new(100),
            )
            .unwrap();
        trst.track_token(token);
    }
    (brn, trst, block, account)
}

fn context<'a>(account: &'a AccountInfo, params: &'a ProtocolParams) -> EconomicContext<'a> {
    EconomicContext {
        account: Some(account),
        previous_brn: 0,
        pending: None,
        params,
    }
}

fn snapshot(brn: &BrnEngine, trst: &TrstEngine) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut wallets: Vec<_> = brn.wallets.iter().collect();
    wallets.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    (
        bincode::serialize(&(wallets, &brn.rate_history, brn.next_stake_id())).unwrap(),
        trst.save_wallets(),
        trst.merger_graph.to_bytes(),
    )
}

#[test]
fn account_only_effects_match_full_evaluation_without_copying_engines() {
    let (brn, trst, mut block, account) = fixture(128);
    let before = snapshot(&brn, &trst);
    let params = ProtocolParams::default();
    for kind in [BlockType::Open, BlockType::ChangeRepresentative] {
        block.block_type = kind;
        let mut baseline_brn = brn.clone();
        let mut baseline_trst = trst.clone();
        let expected = economic_candidate::apply(
            &block,
            context(&account, &params),
            &mut baseline_brn,
            &mut baseline_trst,
        )
        .unwrap();
        let mut staged = StagedEngines::new(&brn, &trst);
        let actual = staged.apply(&block, context(&account, &params)).unwrap();
        assert_eq!(
            format!("{:?}", actual.effect),
            format!("{:?}", expected.effect)
        );
        assert!(actual.pending.is_none() && actual.indices.is_none());
        let (staged_brn, staged_trst) = staged.into_staged();
        assert!(
            staged_brn.is_none() && staged_trst.is_none(),
            "account-only operation copied global state"
        );
        assert_eq!(snapshot(&baseline_brn, &baseline_trst), before);
        assert_eq!(snapshot(&brn, &trst), before);
    }
}

#[test]
fn representative_change_with_stake_release_still_mutates_only_candidate() {
    let (mut brn, trst, mut block, account) = fixture(2);
    let params = ProtocolParams::default();
    let outcome = BlockHash::new([9; 32]);
    brn.get_wallet_mut(&block.account)
        .unwrap()
        .record_release(outcome, 10, &[outcome], Timestamp::new(100))
        .unwrap();
    let before = snapshot(&brn, &trst);
    block.block_type = BlockType::ChangeRepresentative;
    block.release_sources = vec![outcome];
    let mut staged = StagedEngines::new(&brn, &trst);
    assert!(matches!(
        staged
            .apply(&block, context(&account, &params))
            .unwrap()
            .effect,
        EconomicResult::RepChange { .. }
    ));
    let (next_brn, next_trst) = staged.into_staged();
    assert!(next_trst.is_none(), "BRN acknowledgement copied TRST");
    let next_brn = next_brn.expect("stake acknowledgement must be staged");
    assert!(next_brn
        .get_wallet(&block.account)
        .unwrap()
        .pending_releases
        .is_empty());
    assert_eq!(snapshot(&brn, &trst), before);
    block.release_sources = vec![BlockHash::new([8; 32])];
    let mut staged = StagedEngines::new(&brn, &trst);
    assert!(staged.apply(&block, context(&account, &params)).is_err());
    drop(staged);
    assert_eq!(snapshot(&brn, &trst), before);
}

#[test]
fn pending_context_cannot_enter_the_account_only_path() {
    let (brn, trst, block, account) = fixture(2);
    let params = ProtocolParams::default();
    let malformed_pending = PendingInfo {
        source: block.account.clone(),
        amount: 1,
        timestamp: block.timestamp,
        provenance: vec![],
        return_credit: false,
    };
    let mut context = context(&account, &params);
    context.pending = Some(&malformed_pending);
    let before = snapshot(&brn, &trst);
    let mut staged = StagedEngines::new(&brn, &trst);
    assert!(staged.apply(&block, context).is_err());
    let (staged_brn, staged_trst) = staged.into_staged();
    assert!(staged_brn.is_none() && staged_trst.is_none());
    assert_eq!(snapshot(&brn, &trst), before);
}

#[test]
fn burn_matches_full_evaluation_and_explicit_later_mutation_is_staged() {
    let (brn, trst, mut block, account) = fixture(2);
    let params = ProtocolParams::default();
    block.block_type = BlockType::Burn;
    block.brn_balance = 10;
    block.link = BlockHash::new(burst_crypto::decode_address(block.account.as_str()).unwrap());
    block.hash = block.compute_hash();
    let before = snapshot(&brn, &trst);
    let mut baseline_brn = brn.clone();
    let mut baseline_trst = trst.clone();
    let expected = economic_candidate::apply(
        &block,
        context(&account, &params),
        &mut baseline_brn,
        &mut baseline_trst,
    )
    .unwrap();
    let mut staged = StagedEngines::new(&brn, &trst);
    let actual = staged.apply(&block, context(&account, &params)).unwrap();
    assert_eq!(
        format!("{:?}", actual.effect),
        format!("{:?}", expected.effect)
    );
    assert_eq!(
        format!("{:?}", actual.pending),
        format!("{:?}", expected.pending)
    );
    let (next_brn, next_trst) = staged.into_staged();
    assert_eq!(
        snapshot(&next_brn.unwrap(), &next_trst.unwrap()),
        snapshot(&baseline_brn, &baseline_trst)
    );
    assert_eq!(snapshot(&brn, &trst), before);

    block.block_type = BlockType::ChangeRepresentative;
    let mut staged = StagedEngines::new(&brn, &trst);
    staged.apply(&block, context(&account, &params)).unwrap();
    staged
        .mutable()
        .0
        .apply_rate_change(2_000_000, block.timestamp)
        .unwrap();
    let (next_brn, next_trst) = staged.into_staged();
    assert_eq!(next_brn.unwrap().rate_history.current_rate(), 2_000_000);
    assert_eq!(next_trst.unwrap().save_wallets(), trst.save_wallets());
    assert_eq!(snapshot(&brn, &trst), before);
}

#[test]
fn account_only_persistence_preserves_snapshots_and_mutation_failure_is_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let (brn, trst, mut block, account) = fixture(16);
    let params = ProtocolParams::default();
    let mut batch = store.tx_begin_write().unwrap();
    ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
    batch.commit().unwrap();
    let wallets = store.meta_store().get_meta(TrstEngine::meta_key()).unwrap();
    let graph = store.meta_store().get_meta("merger_graph").unwrap();
    let mut staged = StagedEngines::new(&brn, &trst);
    staged.apply(&block, context(&account, &params)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(BlockHash::new([99; 32]), block.account.clone(), 1)
        .unwrap();
    batch.put_meta("state-only-probe", &[1]).unwrap();
    staged.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let (staged_brn, staged_trst) = staged.into_staged();
    assert!(staged_brn.is_none() && staged_trst.is_none());
    assert_eq!(
        store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
        wallets
    );
    assert_eq!(store.meta_store().get_meta("merger_graph").unwrap(), graph);
    let batch = store.tx_begin_write().unwrap();
    let undo = batch
        .block_undo(&BlockHash::new([99; 32]))
        .unwrap()
        .unwrap();
    assert_eq!(undo.changes.len(), 1, "unchanged economics entered undo");
    drop(batch);

    let before = snapshot(&brn, &trst);
    block.block_type = BlockType::Burn;
    block.brn_balance = 10;
    block.link = BlockHash::new(burst_crypto::decode_address(block.account.as_str()).unwrap());
    block.hash = block.compute_hash();
    let mut staged = StagedEngines::new(&brn, &trst);
    staged.apply(&block, context(&account, &params)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    staged.persist(&mut batch).unwrap();
    assert!(batch
        .put_meta("force-map-full", &vec![0; 8 * 1024 * 1024])
        .is_err());
    assert!(batch.commit().is_err());
    drop(staged);
    assert_eq!(snapshot(&brn, &trst), before);
    assert_eq!(
        store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
        wallets
    );
    assert_eq!(store.meta_store().get_meta("merger_graph").unwrap(), graph);
    assert!(store.meta_store().get_meta("force-map-full").is_err());
}

#[test]
#[ignore = "manual copy/persistence measurement; not transaction throughput"]
fn account_only_preparation_cost() {
    let params = ProtocolParams::default();
    for count in [100, 1_000, 10_000] {
        let (brn, trst, mut block, account) = fixture(count);
        block.block_type = BlockType::ChangeRepresentative;
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 128 * 1024 * 1024).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
        for lazy in [false, true] {
            let start = Instant::now();
            for iteration in 1..=5u8 {
                let mut batch = store.tx_begin_write().unwrap();
                batch
                    .begin_block_undo(
                        BlockHash::new([iteration + if lazy { 10 } else { 0 }; 32]),
                        block.account.clone(),
                        1,
                    )
                    .unwrap();
                if lazy {
                    let mut staged = StagedEngines::new(&brn, &trst);
                    std::hint::black_box(staged.apply(&block, context(&account, &params)).unwrap());
                    staged.persist(&mut batch).unwrap();
                    let (staged_brn, staged_trst) = staged.into_staged();
                    assert!(staged_brn.is_none() && staged_trst.is_none());
                } else {
                    let mut brn = brn.clone();
                    let mut trst = trst.clone();
                    std::hint::black_box(
                        economic_candidate::apply(
                            &block,
                            context(&account, &params),
                            &mut brn,
                            &mut trst,
                        )
                        .unwrap(),
                    );
                    ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
                }
                batch.put_meta("cost-probe", &[iteration]).unwrap();
                batch.commit().unwrap();
            }
            eprintln!(
                "{count} portfolios, {}: {:?} per preparation/persistence",
                if lazy { "borrowed" } else { "full copies" },
                start.elapsed() / 5
            );
        }
    }
}
