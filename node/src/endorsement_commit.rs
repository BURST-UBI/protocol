//! Endorsement burns, verification state and accrual share the block transaction.

use burst_brn::BrnEngine;
use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::TrstEngine;
use burst_types::{NetworkId, ProtocolParams};
use burst_verification::{VerificationEvent, VerificationOrchestrator, VerificationResult};

pub(crate) const SNAPSHOT_KEY: &str = "verification_orchestrator";

pub(crate) fn restore(
    batch: &LmdbWriteTransaction<'_>,
) -> Result<VerificationOrchestrator, String> {
    let bytes = batch
        .undo_value_ref(
            burst_store_lmdb::block_undo::UndoTable::Meta,
            SNAPSHOT_KEY.as_bytes(),
        )
        .map_err(|error| error.to_string())?
        .ok_or("missing persisted verification orchestrator; authenticated replay required")?;
    let snapshot = burst_verification::OrchestratorSnapshot::decode(bytes)
        .map_err(|error| format!("invalid persisted verification orchestrator: {error}"))?;
    Ok(VerificationOrchestrator::restore(snapshot))
}

pub(crate) fn validate_target_head(
    block: &StateBlock,
    target: &burst_types::WalletAddress,
    dependency: &StateBlock,
) -> Result<(), String> {
    if dependency.account != *target
        || dependency.hash != block.target_head
        || dependency.compute_hash() != block.target_head
        || dependency
            .timestamp
            .as_secs()
            .saturating_sub(block.timestamp.as_secs())
            > crate::block_processor::FUTURE_TIMESTAMP_TOLERANCE_SECS
    {
        return Err("endorsement target head does not match its target".into());
    }
    Ok(())
}

pub(crate) fn apply(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    burn_amount: u128,
    network: NetworkId,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
) -> Result<(), String> {
    let (brn, trst) = engines;
    let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
        .ok_or("endorsement target is invalid")?;
    if !block.target_head.is_zero() {
        let bytes = batch
            .get_block(&block.target_head)
            .map_err(|e| e.to_string())?;
        let dependency: StateBlock = bincode::deserialize(&bytes).map_err(|e| e.to_string())?;
        validate_target_head(block, &target, &dependency)?;
        batch
            .block_height(&target, &block.target_head)
            .map_err(|e| e.to_string())?;
    }
    let genesis = crate::genesis_key::genesis_address(network);
    let count = batch.verified_account_count().map_err(|e| e.to_string())?;
    let prior = match batch.get_account(&target) {
        Ok(account) => Some(account),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(error.to_string()),
    };
    if prior
        .as_ref()
        .is_some_and(|account| account.state.is_verified())
    {
        return Err("wallet is already verified".into());
    }
    let bootstrap = crate::bootstrap_phase::observe(batch, params)?;
    if block.account == genesis
        && bootstrap == burst_verification::bootstrap::BootstrapPhase::Active
    {
        orchestrator
            .genesis_verify_at(
                &target,
                &genesis,
                count,
                params.bootstrap_exit_threshold as u64,
                block.timestamp,
            )
            .map_err(|e| e.to_string())?;
    } else {
        orchestrator
            .process_endorsement_at(
                &target,
                &block.account,
                burn_amount,
                params,
                block.timestamp,
            )
            .map_err(|e| e.to_string())?;
    }
    for event in orchestrator.drain_events() {
        match event {
            VerificationEvent::EndorsementComplete { .. } => {}
            VerificationEvent::VerificationComplete {
                wallet,
                result: VerificationResult::Verified,
                outcomes,
            } if wallet == target && outcomes.verifiers.is_empty() => {
                crate::verification_commit::verify_wallet(
                    batch,
                    &target,
                    block.timestamp,
                    brn,
                    trst,
                )?;
                crate::verification_commit::dependencies::record_identity(
                    batch, &target, block.hash,
                )?;
            }
            _ => return Err("endorsement produced an unsupported verification transition".into()),
        }
    }
    crate::bootstrap_phase::observe(batch, params)?;
    let bytes = orchestrator
        .snapshot()
        .encode()
        .map_err(|e| e.to_string())?;
    batch
        .put_meta(SNAPSHOT_KEY, &bytes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, CURRENT_BLOCK_VERSION};
    use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
    use burst_store_lmdb::LmdbStore;
    use burst_types::BlockHash;
    use burst_types::{Signature, Timestamp, TxHash};

    mod bootstrap_exit;
    mod identity;

    fn endorsement() -> StateBlock {
        let target = burst_crypto::keypair_from_seed(&[42; 32]);
        let genesis = crate::genesis_key::genesis_address(NetworkId::Dev);
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Endorse,
            account: genesis.clone(),
            previous: BlockHash::ZERO,
            representative: genesis,
            brn_balance: 10,
            trst_balance: 0,
            link: BlockHash::new(target.public.0),
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(1234),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn verification_restore_reads_staged_state_and_preserves_abort() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut state = VerificationOrchestrator::new();
        let initial = state.snapshot().encode().unwrap();
        let address = crate::genesis_key::genesis_address(NetworkId::Dev);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(restore(&batch).is_err());
        batch.put_meta(SNAPSHOT_KEY, &initial).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        state.initialize_genesis(&address);
        let mut changed = state.snapshot().encode().unwrap();
        batch.put_meta(SNAPSHOT_KEY, &changed).unwrap();
        assert!(restore(&batch).unwrap().get_state(&address).is_some());
        changed.push(0);
        batch.put_meta(SNAPSHOT_KEY, &changed).unwrap();
        assert!(restore(&batch).is_err());
        drop(batch);
        let batch = store.tx_begin_write().unwrap();
        assert!(restore(&batch).unwrap().get_state(&address).is_none());
        drop(batch);
        assert_eq!(store.meta_store().get_meta(SNAPSHOT_KEY).unwrap(), initial);
    }

    #[test]
    fn target_dependency_checks_account_and_preserves_clock_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let original = endorsement();
        let target = crate::ledger_bridge::extract_receiver_from_link(&original.link).unwrap();
        for (correct_account, timestamp, valid) in [
            (false, 1200, false),
            (true, 1295, false),
            (true, 1200, true),
            (true, 1235, true),
            (true, 1294, true),
        ] {
            let mut dependency = original.clone();
            dependency.block_type = BlockType::Open;
            dependency.account = if correct_account {
                target.clone()
            } else {
                original.account.clone()
            };
            dependency.representative = dependency.account.clone();
            dependency.link = BlockHash::ZERO;
            dependency.timestamp = Timestamp::new(timestamp);
            dependency.hash = dependency.compute_hash();
            let mut block = original.clone();
            block.target_head = dependency.hash;
            block.hash = block.compute_hash();
            let mut batch = store.tx_begin_write().unwrap();
            crate::bootstrap_phase::initialize(&mut batch, &ProtocolParams::default()).unwrap();
            let mut account = crate::genesis_state::account_to_write(
                None,
                dependency.account.clone(),
                dependency.hash,
            )
            .unwrap();
            account.state = burst_types::WalletState::Unverified;
            account.verified_at = None;
            batch.put_account_info(&account, false).unwrap();
            batch
                .put_block_with_account(
                    &dependency.hash,
                    &bincode::serialize(&dependency).unwrap(),
                    &dependency.account,
                    1,
                )
                .unwrap();
            let result = apply(
                &mut batch,
                &block,
                10,
                NetworkId::Dev,
                &ProtocolParams::default(),
                &mut VerificationOrchestrator::new(),
                (&mut BrnEngine::new(), &mut TrstEngine::new()),
            );
            assert_eq!(result.is_ok(), valid, "{result:?}");
        }
    }

    #[test]
    fn endorsement_and_verification_commit_or_abort_together() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let block = endorsement();
        let target = crate::ledger_bridge::extract_receiver_from_link(&block.link).unwrap();
        let live = VerificationOrchestrator::new();
        let mut batch = store.tx_begin_write().unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &ProtocolParams::default()).unwrap();
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut staged = live.clone();
            let mut brn = BrnEngine::new();
            let mut trst = TrstEngine::new();
            let mut batch = store.tx_begin_write().unwrap();
            crate::ledger_updater::stage_block(&mut batch, &block, None, 0, None, None).unwrap();
            apply(
                &mut batch,
                &block,
                10,
                NetworkId::Dev,
                &ProtocolParams::default(),
                &mut staged,
                (&mut brn, &mut trst),
            )
            .unwrap();
            crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            assert_eq!(
                staged.get_state(&target).unwrap().started_at,
                block.timestamp
            );
            assert!(live.get_state(&target).is_none());
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(store.block_store().get_block(&block.hash).is_ok(), commit);
            assert_eq!(store.account_store().get_account(&target).is_ok(), commit);
            assert_eq!(store.meta_store().get_meta(SNAPSHOT_KEY).is_ok(), commit);
            assert_eq!(
                store.account_store().verified_account_count().unwrap(),
                u64::from(commit)
            );
        }
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let account = store.account_store().get_account(&target).unwrap();
        assert_eq!(account.verified_at, Some(block.timestamp));
        let brn = BrnEngine::load_from_store(&store.brn_store()).unwrap();
        assert!(brn.wallets.contains_key(&target));
        let snapshot = burst_verification::OrchestratorSnapshot::decode(
            &store.meta_store().get_meta(SNAPSHOT_KEY).unwrap(),
        )
        .unwrap();
        let restored = VerificationOrchestrator::restore(snapshot);
        assert_eq!(
            restored.get_state(&target).unwrap().started_at,
            block.timestamp
        );
    }

    #[test]
    fn duplicate_normal_endorsement_fails_without_publishing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let block = endorsement();
        let params = ProtocolParams {
            bootstrap_exit_threshold: 0,
            endorsement_burn_amount: 10,
            ..ProtocolParams::default()
        };
        let mut orch = VerificationOrchestrator::new();
        let mut brn = BrnEngine::new();
        let mut trst = TrstEngine::new();
        let mut batch = store.tx_begin_write().unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &params).unwrap();
        assert!(apply(
            &mut batch,
            &block,
            9,
            NetworkId::Dev,
            &params,
            &mut orch,
            (&mut brn, &mut trst)
        )
        .is_err());
        assert!(orch
            .get_state(&crate::ledger_bridge::extract_receiver_from_link(&block.link).unwrap())
            .is_none());
        assert!(batch.get_meta(SNAPSHOT_KEY).unwrap().is_none());
        apply(
            &mut batch,
            &block,
            10,
            NetworkId::Dev,
            &params,
            &mut orch,
            (&mut brn, &mut trst),
        )
        .unwrap();
        batch.commit().unwrap();
        let before = store.meta_store().get_meta(SNAPSHOT_KEY).unwrap();
        let mut candidate = orch.clone();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::stage_block(&mut batch, &block, None, 0, None, None).unwrap();
        assert!(apply(
            &mut batch,
            &block,
            10,
            NetworkId::Dev,
            &params,
            &mut candidate,
            (&mut brn, &mut trst)
        )
        .is_err());
        drop(batch);
        assert!(store.block_store().get_block(&block.hash).is_err());
        assert_eq!(store.meta_store().get_meta(SNAPSHOT_KEY).unwrap(), before);
    }
}
