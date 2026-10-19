//! Apply payments and account-local preferences against one LMDB writer's state.
use crate::ledger_updater;
use burst_brn::BrnEngine;
use burst_ledger::{BlockType, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, LmdbWriteTransaction};
use burst_trst::TrstEngine;
use burst_types::{ProtocolParams, PublicKey, Timestamp};

pub(crate) struct PaymentState {
    pub brn: BrnEngine,
    pub trst: TrstEngine,
}

pub(crate) fn supports(kind: &BlockType) -> bool {
    matches!(
        kind,
        BlockType::Open
            | BlockType::Burn
            | BlockType::Send
            | BlockType::Receive
            | BlockType::RejectReceive
            | BlockType::ChangeRepresentative
            | BlockType::ChangeSigner
            | BlockType::Merge
            | BlockType::VerifierOptIn
            | BlockType::VerifierOptOut
    )
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Corruption(message.into())
}

fn required<'a>(
    batch: &'a LmdbWriteTransaction<'_>,
    table: UndoTable,
    key: &[u8],
) -> Result<&'a [u8], StoreError> {
    batch
        .undo_value_ref(table, key)?
        .ok_or_else(|| invalid("replacement requires complete economic snapshots"))
}

pub(crate) fn restore(
    batch: &LmdbWriteTransaction<'_>,
    params: &ProtocolParams,
) -> Result<PaymentState, StoreError> {
    let entries = batch.bounded_brn_entries(131_072, 64 * 1024 * 1024)?;
    let ids = required(batch, UndoTable::BrnMeta, b"next_stake_id")?;
    let rates = required(batch, UndoTable::BrnMeta, b"rate_history")?;
    let brn = BrnEngine::from_persisted_entries(
        Some(ids),
        Some(rates),
        entries.map(|entry| entry.map_err(|error| burst_brn::BrnError::Other(error.to_string()))),
    )
    .map_err(|e| invalid(e.to_string()))?;
    let mut trst = crate::trst_storage::load(batch, params.trst_expiry_secs)?;
    trst.merger_graph = crate::graph_storage::load(batch)?;
    trst.validate_bases().map_err(invalid)?;
    trst.rebuild_indexes();
    Ok(PaymentState { brn, trst })
}

pub(crate) fn authenticate(
    block: &StateBlock,
    params: &ProtocolParams,
    now: Timestamp,
) -> Result<(), StoreError> {
    if !supports(&block.block_type) {
        return Err(invalid("unsupported payment fork operation"));
    }
    authenticate_contents(block, params)?;
    require_admission_time(block, now)
}

pub(crate) fn require_admission_time(block: &StateBlock, now: Timestamp) -> Result<(), StoreError> {
    if block.timestamp.as_secs()
        > now
            .as_secs()
            .saturating_add(crate::block_processor::FUTURE_TIMESTAMP_TOLERANCE_SECS)
        || block
            .timestamp
            .as_secs()
            .saturating_add(crate::block_processor::PAST_TIMESTAMP_TOLERANCE_SECS)
            < now.as_secs()
    {
        return Err(invalid("fork timestamp outside admission window"));
    }
    Ok(())
}

/// Historical replay checks a trusted cut's timestamp bound, not today's clock.
/// This function alone does not establish finality or authenticate a manifest.
pub(crate) fn authenticate_contents(
    block: &StateBlock,
    params: &ProtocolParams,
) -> Result<(), StoreError> {
    if !(supports(&block.block_type)
        || matches!(
            block.block_type,
            BlockType::Endorse
                | BlockType::VerifierOptIn
                | BlockType::VerifierOptOut
                | BlockType::VerificationVote
                | BlockType::Challenge
        ))
    {
        return Err(invalid("unsupported block version, contents or parameters"));
    }
    authenticate_signed_contents(block, params)
}

/// Cryptographic and format checks only. The caller must separately select
/// supported transition rules; a valid signature grants no checkpoint authority.
pub(crate) fn authenticate_signed_contents(
    block: &StateBlock,
    params: &ProtocolParams,
) -> Result<(), StoreError> {
    authenticate_with_signer(block, params, &block.account)
}

/// Epoch markers use the configured creator, never the target account's key.
/// State continuity is checked separately against the authenticated predecessor.
pub(crate) fn authenticate_epoch_contents(
    block: &StateBlock,
    params: &ProtocolParams,
    genesis: &burst_types::WalletAddress,
) -> Result<(), StoreError> {
    if block.block_type != BlockType::Epoch {
        return Err(invalid("expected an epoch marker"));
    }
    authenticate_with_signer(block, params, genesis)
}

fn authenticate_with_signer(
    block: &StateBlock,
    params: &ProtocolParams,
    signer: &burst_types::WalletAddress,
) -> Result<(), StoreError> {
    if !block.fits_p2p_message()
        || block.validate_format().is_err()
        || block.hash != block.compute_hash()
        || block.params_hash != params.params_hash()
    {
        return Err(invalid("unsupported block version, contents or parameters"));
    }
    let key = burst_crypto::decode_address(signer.as_str())
        .ok_or_else(|| invalid("invalid signer address"))?;
    if !burst_crypto::verify_signature(block.hash.as_bytes(), &block.signature, &PublicKey(key)) {
        return Err(invalid("invalid block signature"));
    }
    if burst_crypto::decode_address(block.representative.as_str()).is_none() {
        return Err(invalid("invalid representative address"));
    }
    let kind = if matches!(block.block_type, BlockType::Open | BlockType::Receive) {
        burst_work::WorkBlockKind::ReceiveOrOpen
    } else if block.block_type == BlockType::Epoch {
        burst_work::WorkBlockKind::Epoch
    } else {
        burst_work::WorkBlockKind::Base
    };
    let threshold =
        burst_work::WorkThresholds::with_base(params.min_work_difficulty).threshold_for(kind);
    if !block.verify_work(threshold) {
        return Err(invalid("invalid block proof of work"));
    }
    Ok(())
}

pub(crate) fn apply(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    params: &ProtocolParams,
    now: Timestamp,
) -> Result<PaymentState, StoreError> {
    authenticate(block, params, now)?;
    apply_authenticated(batch, block, params)
}

/// The caller must hold the confirmed election for this already-admitted fork.
/// Quorum latency must not impose a second wall-clock admission deadline.
pub(crate) fn apply_elected(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    params: &ProtocolParams,
) -> Result<PaymentState, StoreError> {
    if !supports(&block.block_type) {
        return Err(invalid("unsupported payment fork operation"));
    }
    authenticate_signed_contents(block, params)?;
    apply_authenticated(batch, block, params)
}

fn apply_authenticated(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    params: &ProtocolParams,
) -> Result<PaymentState, StoreError> {
    let mut state = restore(batch, params)?;
    crate::economic_candidate::stage(batch, block, params, &mut (&mut state.brn, &mut state.trst))
        .map_err(invalid)?;
    ledger_updater::refresh_token_counters(batch, &block.account, &state.trst, block.timestamp)
        .map_err(invalid)?;
    ledger_updater::persist_economics(batch, &state.brn, &state.trst).map_err(invalid)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::AccountStore;
    use burst_store_lmdb::LmdbStore;
    use burst_types::{BlockHash, Signature, TxHash, WalletAddress};

    struct Fixture {
        directory: tempfile::TempDir,
        store: LmdbStore,
        params: ProtocolParams,
        address: WalletAddress,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 32 * 1024 * 1024).unwrap();
            let params = ProtocolParams {
                min_work_difficulty: 0,
                trst_expiry_secs: 1000,
                ..ProtocolParams::default()
            };
            let keys = burst_crypto::keypair_from_seed(&[0; 32]);
            let address = burst_crypto::derive_address(&keys.public);
            let mut genesis = StateBlock {
                signing_grant: burst_types::BlockHash::ZERO,
                delegation_payload: Vec::new(),
                version: burst_ledger::CURRENT_BLOCK_VERSION,
                payment_epoch: burst_types::BlockHash::new([1; 32]),
                block_type: BlockType::Open,
                account: address.clone(),
                representative: address.clone(),
                previous: BlockHash::ZERO,
                brn_balance: 0,
                trst_balance: 0,
                link: BlockHash::ZERO,
                origin: TxHash::ZERO,
                transaction: TxHash::ZERO,
                target_head: burst_types::BlockHash::ZERO,
                merge_sources: vec![],
                release_sources: Vec::new(),
                timestamp: Timestamp::new(0),
                params_hash: params.params_hash(),
                work: 0,
                signature: Signature([0; 64]),
                hash: BlockHash::ZERO,
            };
            sign(&mut genesis);
            let mut account =
                crate::genesis_state::account_to_write(None, address.clone(), genesis.hash)
                    .unwrap();
            account.confirmation_height = 1;
            let mut brn = BrnEngine::with_rate(1, Timestamp::new(0));
            brn.track_wallet(
                address.clone(),
                burst_brn::BrnWalletState::new(Timestamp::new(0)),
            );
            let trst = TrstEngine::with_expiry(params.trst_expiry_secs);
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_epoch::initialize(&mut batch, genesis.payment_epoch).unwrap();
            batch
                .put_block_with_account(
                    &genesis.hash,
                    &bincode::serialize(&genesis).unwrap(),
                    &address,
                    1,
                )
                .unwrap();
            batch.put_frontier(&address, &genesis.hash).unwrap();
            batch.put_account_info(&account, false).unwrap();
            ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &account)
                .unwrap();
            batch.commit().unwrap();
            Self {
                directory,
                store,
                params,
                address,
            }
        }

        fn next(&self, kind: BlockType, time: u64) -> StateBlock {
            let batch = self.store.tx_begin_write().unwrap();
            let account = batch.get_account(&self.address).unwrap();
            let mut block: StateBlock =
                bincode::deserialize(&batch.get_block(&account.head).unwrap()).unwrap();
            block.signing_grant = block.active_signing_grant();
            block.previous = block.hash;
            block.block_type = kind;
            block.timestamp = Timestamp::new(time);
            block.link = BlockHash::ZERO;
            block.merge_sources.clear();
            block.origin = TxHash::ZERO;
            block
        }

        fn commit(&self, block: &mut StateBlock) {
            sign(block);
            let mut batch = self.store.tx_begin_write().unwrap();
            apply(&mut batch, block, &self.params, block.timestamp).unwrap();
            batch.commit().unwrap();
        }

        fn holdings(&self) -> Vec<TxHash> {
            let mut ids = Vec::new();
            for (time, amount) in [(100, 20), (102, 30)] {
                let mut burn = self.next(BlockType::Burn, time);
                burn.brn_balance += amount;
                burn.link =
                    BlockHash::new(burst_crypto::decode_address(self.address.as_str()).unwrap());
                self.commit(&mut burn);
                let mut receive = self.next(BlockType::Receive, time + 1);
                receive.link = burn.hash;
                receive.trst_balance += amount;
                self.commit(&mut receive);
                ids.push(receive.hash.into_tx_hash());
            }
            ids
        }
    }

    fn sign(block: &mut StateBlock) {
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(
            block.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
        );
    }

    #[test]
    fn shared_staging_matches_restored_execution_and_preserves_abort() {
        let fixture = Fixture::new();
        let mut burn = fixture.next(BlockType::Burn, 100);
        burn.brn_balance = 10;
        burn.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut burn);
        let (expected_account, expected_pending) = {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let original = restore(&batch, &fixture.params).unwrap();
            let mut staged =
                crate::economic_candidate::StagedEngines::new(&original.brn, &original.trst);
            crate::economic_candidate::stage(&mut batch, &burn, &fixture.params, &mut staged)
                .unwrap();
            ledger_updater::refresh_token_counters(
                &mut batch,
                &burn.account,
                crate::economic_candidate::Engines::trst(&staged),
                burn.timestamp,
            )
            .unwrap();
            staged.persist(&mut batch).unwrap();
            assert_eq!(
                original
                    .brn
                    .get_wallet(&fixture.address)
                    .unwrap()
                    .total_burned,
                0
            );
            assert_eq!(
                restore(&batch, &fixture.params)
                    .unwrap()
                    .brn
                    .get_wallet(&fixture.address)
                    .unwrap()
                    .total_burned,
                10
            );
            let account = batch.get_account(&fixture.address).unwrap();
            let pending = batch
                .get_pending(&fixture.address, &burn.hash.into_tx_hash())
                .unwrap();
            assert_eq!(account.head, burn.hash);
            assert_eq!(pending.amount, 10);
            (
                bincode::serialize(&account).unwrap(),
                bincode::serialize(&pending).unwrap(),
            )
        };
        let mut batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_account(&fixture.address).unwrap().head,
            burn.previous
        );
        assert!(batch.get_block(&burn.hash).is_err());
        assert!(batch
            .get_pending(&fixture.address, &burn.hash.into_tx_hash())
            .is_err());
        assert_eq!(
            restore(&batch, &fixture.params)
                .unwrap()
                .brn
                .get_wallet(&fixture.address)
                .unwrap()
                .total_burned,
            0
        );
        apply(&mut batch, &burn, &fixture.params, burn.timestamp).unwrap();
        assert_eq!(
            bincode::serialize(&batch.get_account(&fixture.address).unwrap()).unwrap(),
            expected_account
        );
        assert_eq!(
            bincode::serialize(
                &batch
                    .get_pending(&fixture.address, &burn.hash.into_tx_hash())
                    .unwrap()
            )
            .unwrap(),
            expected_pending
        );
        batch.commit().unwrap();
    }

    #[test]
    fn shared_staging_rejects_noncanonical_predecessor_before_mutating_engines() {
        let fixture = Fixture::new();
        let mut burn = fixture.next(BlockType::Burn, 100);
        burn.brn_balance = 10;
        burn.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut burn);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut bytes = batch.get_block(&burn.previous).unwrap();
        bytes.push(0);
        batch.put_block(&burn.previous, &bytes).unwrap();
        let original = restore(&batch, &fixture.params).unwrap();
        let mut staged =
            crate::economic_candidate::StagedEngines::new(&original.brn, &original.trst);
        assert!(
            crate::economic_candidate::stage(&mut batch, &burn, &fixture.params, &mut staged)
                .is_err()
        );
        assert!(batch.get_block(&burn.hash).is_err());
        assert_eq!(
            batch.get_account(&fixture.address).unwrap().head,
            burn.previous
        );
        let (brn, trst) = staged.into_staged();
        assert!(brn.is_none() && trst.is_none());
    }

    #[test]
    fn signing_handoff_admits_current_device_and_refuses_stale_device_without_mutation() {
        let fixture = Fixture::new();
        assert!(supports(&BlockType::ChangeSigner));
        let mut handoff = fixture.next(BlockType::ChangeSigner, 100);
        handoff.link = BlockHash::new([77; 32]);
        fixture.commit(&mut handoff);

        let mut successor = fixture.next(BlockType::ChangeRepresentative, 101);
        assert_eq!(successor.signing_grant, handoff.hash);
        let mut stale = successor.clone();
        stale.signing_grant = BlockHash::ZERO;
        sign(&mut stale);
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let error = apply(&mut batch, &stale, &fixture.params, stale.timestamp).err().unwrap();
            assert!(error.to_string().contains("signing-device grant is no longer active"), "{error}");
            assert_eq!(batch.get_account(&fixture.address).unwrap().head, handoff.hash);
            assert!(batch.get_block(&stale.hash).is_err());
        }
        fixture.commit(&mut successor);
        let batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, successor.hash);
    }

    #[test]
    fn fresh_fork_authentication_obeys_the_sixty_second_window() {
        let fixture = Fixture::new();
        for (timestamp, accepted) in [
            (939, false),
            (940, true),
            (1000, true),
            (1060, true),
            (1061, false),
        ] {
            let mut block = fixture.next(BlockType::ChangeRepresentative, timestamp);
            sign(&mut block);
            assert_eq!(
                authenticate(&block, &fixture.params, Timestamp::new(1000)).is_ok(),
                accepted,
                "timestamp {timestamp}"
            );
        }
    }

    #[test]
    fn elected_payment_replay_does_not_repeat_the_admission_deadline() {
        let fixture = Fixture::new();
        let mut block = fixture.next(BlockType::Burn, 100);
        block.brn_balance = 10;
        block.link =
            BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut block);
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            apply(&mut batch, &block, &fixture.params, block.timestamp).unwrap();
        }
        let late = Timestamp::new(
            block.timestamp.as_secs() + crate::block_processor::PAST_TIMESTAMP_TOLERANCE_SECS + 1,
        );
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let error = apply(&mut batch, &block, &fixture.params, late)
            .err()
            .unwrap();
        assert!(error.to_string().contains("outside admission window"));
        assert!(batch.get_block(&block.hash).is_err());
        let state = apply_elected(&mut batch, &block, &fixture.params).unwrap();
        assert_eq!(
            state.brn.get_wallet(&fixture.address).unwrap().total_burned,
            10
        );
        assert_eq!(
            batch.get_account(&fixture.address).unwrap().head,
            block.hash
        );
    }

    #[test]
    fn elected_payment_replay_rechecks_signature_parameters_and_epoch() {
        let fixture = Fixture::new();
        let mut valid = fixture.next(BlockType::Burn, 100);
        valid.brn_balance = 10;
        valid.link =
            BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut valid);
        let mut forged = valid.clone();
        forged.signature = Signature([0; 64]);
        let mut wrong_params = valid.clone();
        wrong_params.params_hash = BlockHash::ZERO;
        sign(&mut wrong_params);
        let mut wrong_epoch = valid.clone();
        wrong_epoch.payment_epoch = BlockHash::ZERO;
        sign(&mut wrong_epoch);
        for (block, message) in [
            (forged, "signature"),
            (wrong_params, "parameters"),
            (wrong_epoch, "payment epoch"),
        ] {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let error = apply_elected(&mut batch, &block, &fixture.params)
                .err()
                .unwrap();
            assert!(error.to_string().contains(message), "{error}");
            assert_eq!(
                batch.get_account(&fixture.address).unwrap().head,
                valid.previous
            );
            assert!(batch.get_block(&block.hash).is_err());
        }
    }

    #[test]
    fn economic_writer_rechecks_epoch_before_changing_accounts_or_balances() {
        let fixture = Fixture::new();
        let before = fixture
            .store
            .account_store()
            .get_account(&fixture.address)
            .unwrap();
        let mut block = fixture.next(BlockType::Burn, 100);
        block.brn_balance = 10;
        block.link =
            BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut block);
        // Simulate context changing after structural admission but before the writer.
        for epoch in [BlockHash::ZERO, BlockHash::new([2; 32])] {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            batch
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes())
                .unwrap();
            batch.commit().unwrap();
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let error = apply(&mut batch, &block, &fixture.params, block.timestamp)
                .err()
                .unwrap();
            assert!(error.to_string().contains("payment epoch"), "{error}");
            assert_eq!(
                batch.get_account(&fixture.address).unwrap().head,
                before.head
            );
            assert!(batch.get_block(&block.hash).is_err());
            assert_eq!(
                restore(&batch, &fixture.params)
                    .unwrap()
                    .brn
                    .get_wallet(&fixture.address)
                    .unwrap()
                    .total_burned,
                0
            );
        }
    }

    #[test]
    fn merge_rejection_is_atomic_and_a_valid_merge_can_be_replaced() {
        let fixture = Fixture::new();
        let ids = fixture.holdings();
        let before = fixture
            .store
            .account_store()
            .get_account(&fixture.address)
            .unwrap();
        for sources in [
            vec![ids[0], TxHash::new([99; 32])],
            vec![ids[0], ids[0]],
            vec![ids[0]],
        ] {
            let mut merge = fixture.next(BlockType::Merge, 104);
            merge.merge_sources = sources;
            sign(&mut merge);
            let mut batch = fixture.store.tx_begin_write().unwrap();
            assert!(apply(&mut batch, &merge, &fixture.params, merge.timestamp).is_err());
            drop(batch);
            let batch = fixture.store.tx_begin_write().unwrap();
            assert_eq!(
                batch.get_account(&fixture.address).unwrap().head,
                before.head
            );
            assert!(batch.get_block(&merge.hash).is_err());
            let state = restore(&batch, &fixture.params).unwrap();
            assert_eq!(
                state
                    .trst
                    .get_portfolio(&fixture.address)
                    .unwrap()
                    .tokens
                    .len(),
                2
            );
            assert!(!state
                .trst
                .merger_graph
                .contains_merge(&merge.hash.into_tx_hash()));
        }
        let mut replacement = fixture.next(BlockType::ChangeRepresentative, 105);
        sign(&mut replacement);
        let mut merge = fixture.next(BlockType::Merge, 104);
        merge.merge_sources = ids;
        fixture.commit(&mut merge);
        {
            let batch = fixture.store.tx_begin_write().unwrap();
            let state = restore(&batch, &fixture.params).unwrap();
            let tokens = &state.trst.get_portfolio(&fixture.address).unwrap().tokens;
            assert_eq!(tokens.len(), 1);
            assert_eq!(tokens[0].amount, 50);
            assert_eq!(tokens[0].origin, merge.hash.into_tx_hash());
            assert_eq!(tokens[0].effective_origin_timestamp, Timestamp::new(100));
            assert_eq!(
                state
                    .trst
                    .merger_graph
                    .get_merge(&tokens[0].origin)
                    .unwrap()
                    .source_origins
                    .len(),
                2
            );
        }
        assert_eq!(
            crate::fork_resolution::evaluate(
                &fixture.store,
                &replacement,
                &fixture.params,
                replacement.timestamp,
                burst_types::NetworkId::Dev,
                false,
                None,
            )
            .unwrap(),
            Some(merge.hash)
        );
        assert_eq!(
            fixture
                .store
                .account_store()
                .get_account(&fixture.address)
                .unwrap()
                .head,
            merge.hash
        );
        crate::rollback_plan::replace_with(
            &fixture.store,
            merge.hash,
            &replacement,
            |batch, block| apply(batch, block, &fixture.params, block.timestamp),
        )
        .unwrap();
        let Fixture {
            directory,
            store,
            params,
            address,
        } = fixture;
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 32 * 1024 * 1024).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&address).unwrap().head, replacement.hash);
        let state = restore(&batch, &params).unwrap();
        assert_eq!(state.trst.get_portfolio(&address).unwrap().tokens.len(), 2);
        assert!(!state
            .trst
            .merger_graph
            .contains_merge(&merge.hash.into_tx_hash()));
    }

    #[test]
    fn expired_consolidation_keeps_the_amount_but_cannot_restore_spendability() {
        let fixture = Fixture::new();
        let ids = fixture.holdings();
        let mut merge = fixture.next(BlockType::Merge, 2000);
        merge.merge_sources = ids;
        fixture.commit(&mut merge);
        let batch = fixture.store.tx_begin_write().unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        let tokens = &state.trst.get_portfolio(&fixture.address).unwrap().tokens;
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].amount, 50);
        assert_eq!(tokens[0].state, burst_types::TrstState::Expired);
        assert_eq!(tokens[0].effective_origin_timestamp, Timestamp::new(100));
        assert_eq!(
            state.trst.candidate_balances(
                &fixture.address,
                &tokens[0].origin,
                Timestamp::new(2000)
            ),
            Some((0, 0))
        );
    }

    #[test]
    fn fork_payload_cleanup_requires_durable_finality_and_keeps_vote_reservations() {
        let fixture = Fixture::new();
        let ids = fixture.holdings();
        let mut competitor = fixture.next(BlockType::ChangeRepresentative, 105);
        sign(&mut competitor);
        let mut merge = fixture.next(BlockType::Merge, 104);
        merge.merge_sources = ids;
        fixture.commit(&mut merge);
        fixture
            .store
            .meta_store()
            .put_fork_candidate(&competitor.hash, &bincode::serialize(&competitor).unwrap())
            .unwrap();
        fixture
            .store
            .meta_store()
            .reserve_consensus_vote(
                &burst_types::BlockHash::new([1; 32]),
                &fixture.address,
                &fixture.address,
                &competitor.previous,
                &merge.hash,
                true,
                0,
            )
            .unwrap();
        assert_eq!(
            crate::fork_resolution::prune_finalized(&fixture.store).unwrap(),
            0
        );
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let height = batch.get_account(&fixture.address).unwrap().block_count + 1;
            batch
                .begin_block_undo(competitor.hash, competitor.account.clone(), height)
                .unwrap();
            assert!(batch
                .prune_finalized_fork_candidate(
                    &competitor.hash,
                    &competitor.account,
                    &competitor.previous
                )
                .is_err());
            assert!(batch.commit().is_err());
        }
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let height = batch.get_account(&fixture.address).unwrap().block_count;
            batch
                .advance_confirmation_height(&fixture.address, height)
                .unwrap();
            assert!(batch
                .prune_finalized_fork_candidate(
                    &competitor.hash,
                    &competitor.account,
                    &competitor.previous
                )
                .unwrap());
            // Dropping the writer must restore both height and cached evidence.
        }
        assert!(fixture
            .store
            .meta_store()
            .fork_candidate(&competitor.hash)
            .unwrap()
            .is_some());
        assert_eq!(
            crate::fork_resolution::prune_finalized(&fixture.store).unwrap(),
            0
        );
        crate::causal_cementation::cement(&fixture.store, merge.hash).unwrap();
        assert_eq!(
            crate::fork_resolution::prune_finalized(&fixture.store).unwrap(),
            1
        );
        assert!(fixture
            .store
            .meta_store()
            .fork_candidate(&competitor.hash)
            .unwrap()
            .is_none());
        assert!(fixture
            .store
            .meta_store()
            .reserve_consensus_vote(
                &burst_types::BlockHash::new([1; 32]),
                &fixture.address,
                &fixture.address,
                &competitor.previous,
                &competitor.hash,
                true,
                0
            )
            .is_err());
        assert_eq!(
            fixture
                .store
                .account_store()
                .get_account(&fixture.address)
                .unwrap()
                .head,
            merge.hash
        );
        let Fixture {
            directory, store, ..
        } = fixture;
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 32 * 1024 * 1024).unwrap();
        assert!(store
            .meta_store()
            .fork_candidate(&competitor.hash)
            .unwrap()
            .is_none());
    }

    #[test]
    fn lifecycle_audit_support_does_not_enable_identity_fork_replacement() {
        let fixture = Fixture::new();
        let mut block = fixture.next(BlockType::Endorse, 100);
        sign(&mut block);
        authenticate_contents(&block, &fixture.params).unwrap();
        assert!(authenticate(&block, &fixture.params, block.timestamp)
            .unwrap_err()
            .to_string()
            .contains("unsupported payment fork operation"));
    }

    #[test]
    fn preference_forks_preserve_eligibility_and_balance_checks() {
        for kind in [BlockType::VerifierOptIn, BlockType::VerifierOptOut] {
            let fixture = Fixture::new();
            let original = fixture
                .store
                .account_store()
                .get_account(&fixture.address)
                .unwrap();
            for balances in [(1, 0), (0, 1)] {
                let mut block = fixture.next(kind.clone(), 100);
                block.brn_balance = balances.0;
                block.trst_balance = balances.1;
                sign(&mut block);
                let mut batch = fixture.store.tx_begin_write().unwrap();
                assert!(apply(&mut batch, &block, &fixture.params, block.timestamp).is_err());
                assert_eq!(
                    batch.get_account(&fixture.address).unwrap().head,
                    original.head
                );
            }
            let mut account = original.clone();
            account.state = burst_types::WalletState::Revoked;
            fixture.store.account_store().put_account(&account).unwrap();
            let mut block = fixture.next(kind.clone(), 100);
            sign(&mut block);
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let result = apply(&mut batch, &block, &fixture.params, block.timestamp);
            if kind == BlockType::VerifierOptIn {
                assert!(result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("verified wallet"));
            } else {
                result.unwrap();
                let account = batch.get_account(&fixture.address).unwrap();
                assert!(account.verifier_opted_in_at.is_none());
                assert_eq!(account.state, burst_types::WalletState::Revoked);
            }
        }
    }

    #[test]
    fn candidate_uses_the_writers_current_account_metadata() {
        let fixture = Fixture::new();
        let mut burn = fixture.next(BlockType::Burn, 100);
        burn.brn_balance = 20;
        sign(&mut burn);
        let mut account = fixture
            .store
            .account_store()
            .get_account(&fixture.address)
            .unwrap();
        account.state = burst_types::WalletState::Revoked;
        account.expired_trst = 77;
        fixture.store.account_store().put_account(&account).unwrap();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let current = ledger_updater::predecessor_account(&batch, &burn)
            .unwrap()
            .unwrap();
        assert_eq!(current.expired_trst, 77);
        assert_eq!(current.state, burst_types::WalletState::Revoked);
        assert!(apply(&mut batch, &burn, &fixture.params, burn.timestamp)
            .err()
            .unwrap()
            .to_string()
            .contains("verified wallet"));
        assert!(batch.get_block(&burn.hash).is_err());
        assert_eq!(
            restore(&batch, &fixture.params)
                .unwrap()
                .brn
                .get_wallet(&fixture.address)
                .unwrap()
                .total_burned,
            0
        );
    }

    #[test]
    fn spending_age_uses_signed_time_not_the_receiving_nodes_clock() {
        let mut fixture = Fixture::new();
        fixture.params.new_wallet_spending_limit = 10;
        fixture.params.new_wallet_rate_limit_duration_secs = 200;
        let mut burn = fixture.next(BlockType::Burn, 199);
        burn.params_hash = fixture.params.params_hash();
        burn.brn_balance = 20;
        sign(&mut burn);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let error = apply(&mut batch, &burn, &fixture.params, Timestamp::new(201))
            .err()
            .unwrap();
        assert!(error.to_string().contains("new wallet spending limit"));
        drop(batch);
        burn.timestamp = Timestamp::new(200);
        fixture.commit(&mut burn);
    }
}
