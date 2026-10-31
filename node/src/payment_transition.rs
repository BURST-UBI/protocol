//! Apply payments and account-local preferences against one LMDB writer's state.
use crate::{
    economic_candidate::{Engines, StagedEngines},
    ledger_updater,
};
use burst_brn::BrnEngine;
use burst_ledger::{BlockType, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::TrstEngine;
use burst_types::{ProtocolParams, PublicKey, Timestamp};

/// Engines staged in the writer; publish them only after its transaction commits.
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

pub(crate) fn restore(
    batch: &LmdbWriteTransaction<'_>,
    params: &ProtocolParams,
) -> Result<PaymentState, StoreError> {
    let brn = batch.read_brn_engine(131_072, 64 * 1024 * 1024)?;
    let trst = batch.read_trst_engine(params.trst_expiry_secs)?;
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

/// Authenticate supported contents without checking timestamps or finality.
/// Historical replay must separately enforce the trusted checkpoint's time bound.
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

/// Stage a newly admitted payment. Abort the transaction on error.
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
/// Does not repeat the admission time check. Abort the transaction on error.
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
    let state = restore(batch, params)?;
    let mut staged = StagedEngines::new(&state.brn, &state.trst);
    crate::economic_candidate::stage(batch, block, params, &mut staged).map_err(invalid)?;
    ledger_updater::refresh_token_counters(batch, &block.account, staged.trst(), block.timestamp)
        .map_err(invalid)?;
    staged.persist(batch).map_err(invalid)?;
    let (brn, trst) = staged.into_staged();
    Ok(PaymentState {
        brn: brn.unwrap_or(state.brn),
        trst: trst.unwrap_or(state.trst),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    mod scaling;
    use burst_store::account::AccountStore;
    use burst_store_lmdb::LmdbStore;
    use burst_types::{BlockHash, Signature, TxHash, WalletAddress};

    #[tokio::test]
    async fn readonly_payment_capture_is_pinned_bounded_and_does_not_wait_for_writer() {
        use crate::economic_candidate::prepared::{CapturedPayment, PreparedStageError};
        use std::sync::Arc;
        use std::time::Duration;
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut block = fixture.next(BlockType::ChangeRepresentative, 104);
        sign(&mut block);
        assert!(CapturedPayment::capture_read(&fixture.store, &block, &fixture.params).unwrap().is_none());
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        restore(&batch, &fixture.params).unwrap();
        batch.commit().unwrap();
        let store = Arc::new(fixture.store);
        let view = store.tx_begin_read().unwrap();
        let expiry = fixture.params.trst_expiry_secs;
        assert!(store.cached_payment_engines(&view, expiry, 131_072, 64 << 20).unwrap().is_some());
        assert!(store.cached_payment_engines(&view, expiry + 1, 131_072, 64 << 20).unwrap().is_none());
        assert!(store.cached_payment_engines(&view, expiry, 0, 64 << 20).is_err());
        assert!(store.cached_payment_engines(&view, expiry, 131_072, 0).is_err());
        let other_dir = tempfile::tempdir().unwrap();
        let other = LmdbStore::open(other_dir.path(), 32, 16 << 20).unwrap();
        assert!(other.cached_payment_engines(&view, expiry, 131_072, 64 << 20).unwrap().is_none());
        drop(view);
        // A live writer must neither block the worker nor leak its uncommitted cutoff.
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta(crate::confirmed_replay::installation::CUTOFF_KEY, &105u64.to_be_bytes()).unwrap();
        let worker_store = store.clone();
        let worker_block = block.clone();
        let params = fixture.params.clone();
        let mut task = tokio::task::spawn_blocking(move || CapturedPayment::capture_read(
            &worker_store, &worker_block, &params));
        let captured = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
        drop(writer);
        let prepared = captured.expect("readonly capture waited for the writer")
            .unwrap().unwrap().expect("warm capture missing").prepare().unwrap();
        // A changed authoritative cutoff rejects the already prepared candidate.
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta(crate::confirmed_replay::installation::CUTOFF_KEY, &105u64.to_be_bytes()).unwrap();
        assert!(matches!(prepared.stage(&mut writer, &fixture.params), Err(PreparedStageError::BeforeWrite(_))));
        assert!(writer.get_block(&block.hash).is_err());
        drop(writer);
        let view = store.tx_begin_read().unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta("readonly-generation-test", &[1]).unwrap();
        writer.commit().unwrap();
        assert!(store.cached_payment_engines(&view, expiry, 131_072, 64 << 20).unwrap().is_none());
        drop(view);
        assert!(CapturedPayment::capture_read(&store, &block, &fixture.params).unwrap().is_some());
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_brn_meta(b"rate_history", b"corrupted").unwrap();
        writer.commit().unwrap();
        assert!(CapturedPayment::capture_read(&store, &block, &fixture.params).unwrap().is_none());
        let writer = store.tx_begin_write().unwrap();
        assert!(matches!(CapturedPayment::capture(&writer, &block, &fixture.params), Err(PreparedStageError::Abort(_))));
    }

    #[tokio::test]
    async fn payment_workers_release_writer_overlap_and_preserve_authentication() {
        use std::sync::{Arc, Condvar, Mutex};
        use std::time::Duration;
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        restore(&batch, &fixture.params).unwrap();
        batch.commit().unwrap();
        let mut first = fixture.next(BlockType::ChangeRepresentative, 104);
        sign(&mut first);
        // Independent accounts must prepare concurrently.
        let key = burst_crypto::keypair_from_seed(&[17; 32]);
        let other = burst_crypto::derive_address(&key.public);
        let mut open = first.clone();
        open.block_type = BlockType::Open;
        open.account = other.clone();
        open.representative = other.clone();
        open.previous = BlockHash::ZERO;
        open.brn_spent = 0;
        open.trst_balance = 0;
        open.hash = open.compute_hash();
        open.signature = burst_crypto::sign_message(open.hash.as_bytes(), &key.private);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        apply(&mut batch, &open, &fixture.params, open.timestamp).unwrap();
        batch.commit().unwrap();
        let mut second = open.clone();
        second.block_type = BlockType::ChangeRepresentative;
        second.previous = open.hash;
        second.timestamp = Timestamp::new(105);
        second.hash = second.compute_hash();
        second.signature = burst_crypto::sign_message(second.hash.as_bytes(), &key.private);
        let mut unsigned = fixture.next(BlockType::ChangeRepresentative, 106);
        sign(&mut unsigned);
        unsigned.signature = Signature([0; 64]);
        let unsigned_hash = unsigned.hash;
        let head = first.previous;
        let store = Arc::new(fixture.store);
        let queue = Arc::new(crate::priority_queue::BlockPriorityQueue::new(3));
        for block in [first, second] { assert!(queue.push(block).await); }
        let processor = crate::block_processor::BlockProcessor::new(0, BlockHash::new([1; 32]));
        let mut validation = crate::block_validation::BlockValidationQueue::with_workers(
            queue.clone(), processor.authentication_rules(), 2);
        validation.set_preparation(store.clone(), fixture.params.clone());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
        validation.before_prepare = Some(Arc::new(move || {
            started.send(()).unwrap();
            let ready = worker_gate.0.lock().unwrap();
            let (ready, _) = worker_gate.1.wait_timeout_while(ready, Duration::from_secs(5),
                |ready| !*ready).unwrap();
            assert!(*ready, "preparation worker was not released");
        }));
        let collect = async {
            let mut received = Vec::new();
            for _ in 0..3 { received.push(validation.next().await.unwrap()); }
            received
        };
        tokio::pin!(collect);
        tokio::select! {
            _ = &mut collect => panic!("preparation unexpectedly completed"),
            _ = async {
                for _ in 0..2 {
                    tokio::time::timeout(Duration::from_secs(2), observed.recv()).await.unwrap().unwrap();
                }
            } => ()
        }
        // Preparation must not hold the database writer.
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, head);
        drop(batch);
        assert!(queue.push(unsigned).await);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let received = collect.await;
        assert!(observed.try_recv().is_err(), "unsigned candidate reached preparation");
        let mut prepared = Vec::new();
        let mut owners = Vec::new();
        for mut item in received {
            if item.block().hash == unsigned_hash {
                assert!(item.result().is_err());
                assert!(item.prepared.is_none());
            } else {
                assert!(item.result().is_ok());
                assert!(item.preparation_fault.is_none());
                assert!(item.preparation_time > Duration::ZERO);
                assert_eq!(item.worker_time(), item.authentication_time() + item.preparation_time);
                prepared.push(item.prepared.take().expect("eligible signed candidate prepared"));
                owners.push(item);
            }
        }
        for prepared in prepared {
            let mut batch = store.tx_begin_write().unwrap();
            prepared.stage(&mut batch, &fixture.params).unwrap().unwrap();
            batch.commit().unwrap();
        }
        let mut batch = store.tx_begin_write().unwrap();
        for item in &owners {
            assert_eq!(batch.get_account(&item.block().account).unwrap().head, item.block().hash);
        }
        drop(owners);
        let head = batch.get_account(&fixture.address).unwrap().head;
        let mut successor: StateBlock = bincode::deserialize(&batch.get_block(&head).unwrap()).unwrap();
        successor.previous = head;
        successor.timestamp = Timestamp::new(107);
        sign(&mut successor);
        batch.put_brn_meta(b"rate_history", b"corrupted").unwrap();
        batch.commit().unwrap();
        let queue = Arc::new(crate::priority_queue::BlockPriorityQueue::new(1));
        assert!(queue.push(successor.clone()).await);
        let mut validation = crate::block_validation::BlockValidationQueue::with_workers(
            queue, processor.authentication_rules(), 1);
        validation.set_preparation(store.clone(), fixture.params.clone());
        let fault = validation.next().await.unwrap();
        assert!(fault.result().is_ok());
        assert!(fault.prepared.is_none());
        assert!(fault.preparation_fault.is_none(), "dirty cache must defer to authoritative admission");
        assert!(fault.preparation_time > Duration::ZERO);
        let batch = store.tx_begin_write().unwrap();
        assert!(matches!(crate::economic_candidate::prepared::CapturedPayment::capture(
            &batch, &successor, &fixture.params),
            Err(crate::economic_candidate::prepared::PreparedStageError::Abort(_))));
        assert!(batch.get_block(&successor.hash).is_err());
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, head);

    }

    #[tokio::test]
    async fn competing_payment_preparation_keeps_reservation_through_cancel_and_commit() {
        use std::sync::{Arc, Condvar, Mutex};
        use std::time::Duration;
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &state.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &state.trst).unwrap();
        restore(&batch, &fixture.params).unwrap();
        batch.commit().unwrap();
        let queue = Arc::new(crate::priority_queue::BlockPriorityQueue::new(4));
        for time in 104..107 {
            let mut block = fixture.next(BlockType::ChangeRepresentative, time);
            sign(&mut block);
            assert!(queue.push(block).await);
        }
        let store = Arc::new(fixture.store);
        let processor = crate::block_processor::BlockProcessor::new(0, BlockHash::new([1; 32]));
        let mut validation = crate::block_validation::BlockValidationQueue::with_workers(
            queue.clone(), processor.authentication_rules(), 3);
        validation.set_preparation(store.clone(), fixture.params.clone());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
        validation.before_prepare = Some(Arc::new(move || {
            started.send(()).unwrap();
            let ready = worker_gate.0.lock().unwrap();
            let (ready, _) = worker_gate.1.wait_timeout_while(ready, Duration::from_secs(5),
                |ready| !*ready).unwrap();
            assert!(*ready, "preparation worker was not released");
        }));
        let mut next = Box::pin(validation.next());
        tokio::select! {
            _ = &mut next => panic!("reserved worker unexpectedly completed"),
            started = tokio::time::timeout(Duration::from_secs(2), observed.recv()) => { started.unwrap().unwrap(); }
        }
        drop(next);
        // The production loop refreshes preparation after a cancelled select.
        validation.set_preparation(store.clone(), fixture.params.clone());
        assert!(observed.try_recv().is_err());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let mut owner = validation.next().await.unwrap();
        assert!(owner._preparation_reservation.is_some());
        assert!(owner.prepared.is_some());
        validation.set_preparation(store.clone(), fixture.params.clone());
        // Completed speculation still owns the account while admission decides
        // whether it will commit. Competing work must not wait behind that guard.
        let mut siblings = Vec::new();
        for _ in 0..2 {
            let other = tokio::time::timeout(Duration::from_secs(2), validation.next()).await.unwrap().unwrap();
            assert!(other.result().is_ok());
            assert!(other.prepared.is_none());
            assert!(Arc::ptr_eq(owner._preparation_reservation.as_ref().unwrap(),
                other._preparation_reservation.as_ref().unwrap()));
            assert_eq!(other.preparation_time, Duration::ZERO);
            siblings.push(other);
        }
        assert!(observed.try_recv().is_err(), "competing preparation ran");
        let mut batch = store.tx_begin_write().unwrap();
        owner.prepared.take().unwrap().stage(&mut batch, &fixture.params).unwrap().unwrap();
        batch.commit().unwrap();
        let mut successor = owner.block().clone();
        successor.previous = successor.hash;
        successor.timestamp = Timestamp::new(107);
        sign(&mut successor);
        drop(owner);
        // Delivered siblings still reserve the account across a settings refresh.
        validation.set_preparation(store.clone(), fixture.params.clone());
        assert!(queue.push(successor.clone()).await);
        let deferred = validation.next().await.unwrap();
        assert!(deferred.prepared.is_none(), "queued siblings lost their reservation");
        assert!(deferred._preparation_reservation.is_some());
        assert!(observed.try_recv().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        apply(&mut batch, &successor, &fixture.params, successor.timestamp).unwrap();
        batch.commit().unwrap();
        drop(deferred);
        drop(siblings);
        successor.previous = successor.hash;
        successor.timestamp = Timestamp::new(108);
        sign(&mut successor);
        assert!(queue.push(successor.clone()).await);
        let next = validation.next().await.unwrap();
        assert!(next.prepared.is_some(), "drained account did not release its reservation");
        assert!(next._preparation_reservation.is_some());
        assert_eq!(next.block().hash, successor.hash);
        assert_eq!(observed.try_recv().unwrap(), ());
    }

    #[test]
    fn prepared_endorsement_persists_later_verification_and_rewinds_after_reopen() {
        use crate::economic_candidate::prepared::CapturedPayment;
        let fixture = Fixture::new();
        let target_key = burst_crypto::keypair_from_seed(&[42; 32]);
        let target = burst_crypto::derive_address(&target_key.public);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        let mut orchestrator = burst_verification::VerificationOrchestrator::new();
        orchestrator.initialize_genesis(&fixture.address);
        batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY,
            &orchestrator.snapshot().encode().unwrap()).unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &fixture.params).unwrap();
        batch.commit().unwrap();
        let mut block = fixture.next(BlockType::Endorse, 100);
        block.brn_spent = 10;
        block.link = BlockHash::new(target_key.public.0);
        sign(&mut block);
        authenticate_contents(&block, &fixture.params).unwrap();
        require_admission_time(&block, block.timestamp).unwrap();
        let mut expected = None;
        // The first transaction is the ordinary serial reference and aborts.
        // Prepared abort and commit must retain the later verification writes.
        for (prepared, commit) in [(false, false), (true, false), (true, true)] {
            let batch = fixture.store.tx_begin_write().unwrap();
            let before = restore(&batch, &fixture.params).unwrap();
            let capture = if prepared {
                Some(CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap())
            } else { None };
            batch.commit().unwrap();
            let candidate = capture.map(|capture| capture.prepare().unwrap());
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let mut engines = StagedEngines::new(&before.brn, &before.trst);
            if let Some(candidate) = candidate {
                let state = candidate.stage(&mut batch, &fixture.params).unwrap().unwrap();
                engines.adopt_persisted(state.brn, state.trst);
            } else {
                crate::economic_candidate::stage(&mut batch, &block, &fixture.params, &mut engines).unwrap();
            }
            let mut orchestrator = crate::endorsement_commit::restore(&batch).unwrap();
            crate::endorsement_commit::apply_staged(&mut batch, &block, 10,
                burst_types::NetworkId::Dev, &fixture.params, &mut orchestrator, &mut engines).unwrap();
            engines.persist(&mut batch).unwrap();
            let state = restore(&batch, &fixture.params).unwrap();
            assert_eq!(batch.get_account(&target).unwrap().state, burst_types::WalletState::Verified);
            let observed = (
                bincode::serialize(state.brn.get_wallet(&fixture.address).unwrap()).unwrap(),
                bincode::serialize(state.brn.get_wallet(&target).unwrap()).unwrap(),
                state.trst.save_wallets(),
                bincode::serialize(&batch.get_account(&target).unwrap()).unwrap(),
            );
            if let Some(expected) = &expected { assert_eq!(&observed, expected); }
            else { expected = Some(observed); }
            if commit { batch.commit().unwrap(); } else { drop(batch); }
            let batch = fixture.store.tx_begin_write().unwrap();
            assert_eq!(batch.get_account(&target).is_ok(), commit);
            assert_eq!(restore(&batch, &fixture.params).unwrap().brn.get_wallet(&target).is_some(), commit);
            assert_eq!(crate::endorsement_commit::restore(&batch).unwrap().get_state(&target).is_some(), commit);
        }
        drop(fixture.store);
        let store = LmdbStore::open(fixture.directory.path(), 32, 32 * 1024 * 1024).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        assert_eq!(bincode::serialize(state.brn.get_wallet(&target).unwrap()).unwrap(), expected.unwrap().1);
        batch.rewind_block(&block.hash).unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        assert!(state.brn.get_wallet(&target).is_none());
        assert!(batch.get_account(&target).is_err());
        assert!(crate::endorsement_commit::restore(&batch).unwrap().get_state(&target).is_none());
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, block.previous);
        batch.commit().unwrap();
    }

    #[test]
    fn prepared_burn_rebases_unrelated_burn_matches_serial_and_rewinds_after_reopen() {
        use crate::economic_candidate::prepared::CapturedPayment;
        for changed_portfolio in [false, true] {
        let fixture = Fixture::new();
        fixture.create_holdings();
        let other = burst_crypto::derive_address(&PublicKey([77; 32]));
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut block = fixture.next(BlockType::Burn, 104);
        block.brn_spent += 7;
        block.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut block);
        authenticate(&block, &fixture.params, block.timestamp).unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        let strict = CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap().prepare().unwrap();
        let abort = CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap();
        let commit = CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap();
        batch.commit().unwrap();
        let abort = abort.prepare().unwrap();
        let commit = commit.prepare().unwrap();
        // Optionally change the acting portfolio without advancing its frontier.
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let current = restore(&batch, &fixture.params).unwrap();
        let mut staged = StagedEngines::new(&current.brn, &current.trst);
        let trst = staged.trst_mut();
        let holder = if changed_portfolio { fixture.address.clone() } else { other.clone() };
        let token = trst.mint(TxHash::new([79; 32]), holder.clone(), 20,
            holder, Timestamp::new(104)).unwrap();
        if changed_portfolio { trst.track_token(token); }
        let before = (trst.save_wallets(), trst.merger_graph.to_bytes(), bincode::serialize(current.brn.get_wallet(&fixture.address).unwrap()).unwrap());
        staged.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        if changed_portfolio {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            assert!(matches!(strict.stage(&mut batch, &fixture.params),
                Err(crate::economic_candidate::prepared::PreparedStageError::BeforeWrite(error))
                if error.contains("prepared TRST portfolio dependency changed")));
            assert!(batch.get_block(&block.hash).is_err());
        }
        let expected = {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let state = apply(&mut batch, &block, &fixture.params, block.timestamp).unwrap();
            (state.trst.save_wallets(), state.trst.merger_graph.to_bytes(),
                bincode::serialize(&batch.get_account(&fixture.address).unwrap()).unwrap(), bincode::serialize(state.brn.get_wallet(&fixture.address).unwrap()).unwrap())
        };
        for (prepared, publish) in [(abort, false), (commit, true)] {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let state = prepared.stage_revalidated(&mut batch, &fixture.params).unwrap().unwrap().state;
            assert_eq!(bincode::serialize(state.brn.get_wallet(&fixture.address).unwrap()).unwrap(), expected.3);
            assert_eq!(state.trst.save_wallets(), expected.0);
            assert_eq!(state.trst.merger_graph.to_bytes(), expected.1);
            assert_eq!(bincode::serialize(&batch.get_account(&fixture.address).unwrap()).unwrap(), expected.2);
            if publish { batch.commit().unwrap(); } else { drop(batch); }
            let batch = fixture.store.tx_begin_write().unwrap();
            assert_eq!(batch.get_block(&block.hash).is_ok(), publish);
        }
        drop(fixture.store);
        let store = LmdbStore::open(fixture.directory.path(), 32, 32 * 1024 * 1024).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let restored = restore(&batch, &fixture.params).unwrap();
        assert_eq!(bincode::serialize(restored.brn.get_wallet(&fixture.address).unwrap()).unwrap(), expected.3);
        assert_eq!(restored.trst.save_wallets(), expected.0);
        assert_eq!(restored.trst.merger_graph.to_bytes(), expected.1);
        batch.rewind_block(&block.hash).unwrap();
        let restored = restore(&batch, &fixture.params).unwrap();
        assert_eq!(bincode::serialize(restored.brn.get_wallet(&fixture.address).unwrap()).unwrap(), before.2);
        assert_eq!(restored.trst.save_wallets(), before.0);
        assert_eq!(restored.trst.merger_graph.to_bytes(), before.1);
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, block.previous);
        batch.commit().unwrap();
        }
    }

    #[test]
    fn prepared_merge_rebases_unrelated_merge_matches_serial_and_rewinds_after_reopen() {
        use crate::economic_candidate::prepared::CapturedPayment;
        let fixture = Fixture::new();
        let ids = fixture.create_holdings();
        let other = burst_crypto::derive_address(&PublicKey([77; 32]));
        let other_ids = [TxHash::new([77; 32]), TxHash::new([78; 32])];
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut initial = restore(&batch, &fixture.params).unwrap();
        for id in other_ids {
            let token = initial.trst.mint(id, other.clone(), 20, other.clone(), Timestamp::new(100)).unwrap();
            initial.trst.track_token(token);
        }
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut block = fixture.next(BlockType::Merge, 104);
        block.merge_sources = ids;
        sign(&mut block);
        authenticate(&block, &fixture.params, block.timestamp).unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        let abort = CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap();
        let commit = CapturedPayment::capture(&batch, &block, &fixture.params).unwrap().unwrap();
        batch.commit().unwrap();
        let abort = abort.prepare().unwrap();
        let commit = commit.prepare().unwrap();
        // Advance a different portfolio and the shared graph after capture.
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let current = restore(&batch, &fixture.params).unwrap();
        let mut staged = StagedEngines::new(&current.brn, &current.trst);
        let trst = staged.trst_mut();
        let inputs = trst.wallets[&other].select_merge_inputs(&other_ids).unwrap();
        let token = trst.merge(&inputs, other.clone(), TxHash::new([79; 32]), Timestamp::new(104)).unwrap();
        trst.bulk_untrack(&other, &other_ids.into_iter().collect());
        trst.track_token(token);
        let before = (trst.save_wallets(), trst.merger_graph.to_bytes());
        staged.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let expected = {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let state = apply(&mut batch, &block, &fixture.params, block.timestamp).unwrap();
            (state.trst.save_wallets(), state.trst.merger_graph.to_bytes(),
                bincode::serialize(&batch.get_account(&fixture.address).unwrap()).unwrap())
        };
        for (prepared, publish) in [(abort, false), (commit, true)] {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let state = prepared.stage(&mut batch, &fixture.params).unwrap().unwrap();
            assert_eq!(state.trst.save_wallets(), expected.0);
            assert_eq!(state.trst.merger_graph.to_bytes(), expected.1);
            assert_eq!(bincode::serialize(&batch.get_account(&fixture.address).unwrap()).unwrap(), expected.2);
            if publish { batch.commit().unwrap(); } else { drop(batch); }
            let batch = fixture.store.tx_begin_write().unwrap();
            assert_eq!(batch.get_block(&block.hash).is_ok(), publish);
        }
        drop(fixture.store);
        let store = LmdbStore::open(fixture.directory.path(), 32, 32 * 1024 * 1024).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let restored = restore(&batch, &fixture.params).unwrap();
        assert_eq!(restored.trst.save_wallets(), expected.0);
        assert_eq!(restored.trst.merger_graph.to_bytes(), expected.1);
        batch.rewind_block(&block.hash).unwrap();
        let restored = restore(&batch, &fixture.params).unwrap();
        assert_eq!(restored.trst.save_wallets(), before.0);
        assert_eq!(restored.trst.merger_graph.to_bytes(), before.1);
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, block.previous);
        batch.commit().unwrap();
    }

    #[test]
    fn prepared_payment_distinguishes_revalidation_from_abort() {
        use crate::economic_candidate::prepared::{CapturedPayment, PreparedStageError};
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut block = fixture.next(BlockType::ChangeRepresentative, 104);
        sign(&mut block);
        let prepare = || {
            let batch = fixture.store.tx_begin_write().unwrap();
            restore(&batch, &fixture.params).unwrap();
            batch.commit().unwrap();
            let captured = CapturedPayment::capture_read(&fixture.store, &block, &fixture.params).unwrap().unwrap();
            captured.prepare().unwrap()
        };
        let prepared = prepare();
        // Advancing confirmation height invalidates the captured account, but
        // this payment remains valid against the updated state.
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut account = batch.get_account(&fixture.address).unwrap();
        account.confirmation_height += 1;
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        assert!(matches!(prepared.stage(&mut batch, &fixture.params), Err(PreparedStageError::BeforeWrite(_))));
        apply(&mut batch, &block, &fixture.params, block.timestamp).unwrap();
        drop(batch);
        for stale_account in [false, true] {
            let prepared = prepare();
            let mut batch = fixture.store.tx_begin_write().unwrap();
            batch.put_brn_meta(b"rate_history", b"corrupted").unwrap();
            if stale_account {
                let mut account = batch.get_account(&fixture.address).unwrap();
                account.confirmation_height += 1;
                batch.put_account_info(&account, false).unwrap();
            }
            assert!(matches!(CapturedPayment::capture(&batch, &block, &fixture.params),
                Err(PreparedStageError::Abort(_))));
            assert!(matches!(prepared.stage_revalidated(&mut batch, &fixture.params),
                Err(PreparedStageError::Abort(_))));
            assert!(batch.get_block(&block.hash).is_err());
            drop(batch);
        }
        // A dirty undo boundary fails in the write phase. Even this early write
        // failure must never fall through into serial staging on the same batch.
        let prepared = prepare();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        batch.put_meta("prepared-write-failure", &[1]).unwrap();
        assert!(matches!(prepared.stage_revalidated(&mut batch, &fixture.params), Err(PreparedStageError::Abort(_))));
        assert!(batch.commit().is_err());
        let batch = fixture.store.tx_begin_write().unwrap();
        assert!(batch.get_block(&block.hash).is_err());
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, block.previous);
    }

    #[test]
    fn prepared_claims_preserve_pending_semantics_and_reject_changed_sources() {
        use crate::economic_candidate::prepared::CapturedPayment;
        for kind in [BlockType::Receive, BlockType::RejectReceive] {
            let fixture = Fixture::new();
            fixture.create_holdings();
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let initial = restore(&batch, &fixture.params).unwrap();
            let origin = initial.trst.wallets[&fixture.address].tokens[0].origin;
            crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
            crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
            batch.commit().unwrap();
            let mut send = fixture.next(BlockType::Send, 104);
            send.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
            send.origin = origin;
            send.trst_balance -= 5;
            fixture.commit(&mut send);
            let mut claim = fixture.next(kind.clone(), 105);
            claim.link = send.hash;
            if kind == BlockType::Receive { claim.trst_balance += 5; }
            sign(&mut claim);
            authenticate(&claim, &fixture.params, claim.timestamp).unwrap();
            let prepare = || {
                let batch = fixture.store.tx_begin_write().unwrap();
                restore(&batch, &fixture.params).unwrap();
                batch.commit().unwrap();
                let captured = CapturedPayment::capture_read(&fixture.store, &claim, &fixture.params).unwrap().unwrap();
                captured.prepare().unwrap()
            };
            for mutation in 0..3 {
                let prepared = prepare();
                let mut batch = fixture.store.tx_begin_write().unwrap();
                if mutation == 0 {
                    batch.delete_pending(&fixture.address, send.hash.as_bytes()).unwrap();
                } else {
                    let mut pending = batch.get_pending(&fixture.address, &send.hash.into_tx_hash()).unwrap();
                    if mutation == 1 { pending.return_credit = true; }
                    else {
                        let mut split = pending.provenance[0].clone();
                        split.amount = 1;
                        pending.provenance[0].amount -= 1;
                        pending.provenance.push(split);
                    }
                    batch.put_pending(&fixture.address, send.hash.as_bytes(), &bincode::serialize(&pending).unwrap()).unwrap();
                }
                assert!(matches!(prepared.stage(&mut batch, &fixture.params),
                    Err(crate::economic_candidate::prepared::PreparedStageError::BeforeWrite(_))));
                assert!(batch.get_block(&claim.hash).is_err());
                assert_eq!(batch.get_account(&fixture.address).unwrap().head, send.hash);
                drop(batch);
                let batch = fixture.store.tx_begin_write().unwrap();
                assert_eq!(batch.get_pending(&fixture.address, &send.hash.into_tx_hash()).unwrap().amount, 5);
            }
            let output = |batch: &LmdbWriteTransaction<'_>, state: &PaymentState| {
                assert!(batch.get_pending(&fixture.address, &send.hash.into_tx_hash()).is_err());
                let returned = if kind == BlockType::RejectReceive {
                    Some(batch.get_pending(&fixture.address, &claim.hash.into_tx_hash()).unwrap())
                } else { None };
                bincode::serialize(&(batch.get_account(&fixture.address).unwrap(),
                    state.trst.save_wallets(), state.brn.get_wallet(&fixture.address), returned)).unwrap()
            };
            let expected = {
                let mut batch = fixture.store.tx_begin_write().unwrap();
                let state = apply(&mut batch, &claim, &fixture.params, claim.timestamp).unwrap();
                output(&batch, &state)
            };
            for commit in [false, true] {
                let prepared = prepare();
                let stale = prepare();
                let mut batch = fixture.store.tx_begin_write().unwrap();
                let state = prepared.stage(&mut batch, &fixture.params).unwrap().unwrap();
                assert_eq!(output(&batch, &state), expected);
                if commit { batch.commit().unwrap(); } else { drop(batch); }
                let mut batch = fixture.store.tx_begin_write().unwrap();
                if commit {
                    assert!(matches!(stale.stage(&mut batch, &fixture.params),
                        Err(crate::economic_candidate::prepared::PreparedStageError::BeforeWrite(_))));
                    assert_eq!(batch.get_account(&fixture.address).unwrap().head, claim.hash);
                } else {
                    assert_eq!(batch.get_account(&fixture.address).unwrap().head, send.hash);
                    assert_eq!(batch.get_pending(&fixture.address, &send.hash.into_tx_hash()).unwrap().amount, 5);
                }
            }
        }
    }

    #[test]
    fn prepared_payment_rebases_unrelated_commits_and_refuses_repeated_consumption() {
        use crate::economic_candidate::prepared::CapturedPayment;
        let fixture = Fixture::new();
        fixture.create_holdings();
        let other = burst_crypto::derive_address(&PublicKey([77; 32]));
        let other_origin = TxHash::new([77; 32]);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut initial = restore(&batch, &fixture.params).unwrap();
        initial.brn.track_wallet(other.clone(), burst_brn::BrnWalletState::new(Timestamp::new(0)));
        let token = initial.trst.mint(other_origin, other.clone(), 20, other.clone(), Timestamp::new(100)).unwrap();
        initial.trst.track_token(token);
        ledger_updater::persist_brn(&mut batch, &initial.brn).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut send = fixture.next(BlockType::Send, 104);
        send.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        send.origin = initial.trst.wallets[&fixture.address].tokens[0].origin;
        send.trst_balance -= 5;
        sign(&mut send);
        authenticate(&send, &fixture.params, send.timestamp).unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        let capture = CapturedPayment::capture(&batch, &send, &fixture.params).unwrap().unwrap();
        let stale = CapturedPayment::capture(&batch, &send, &fixture.params).unwrap().unwrap();
        batch.commit().unwrap();
        let prepared = capture.prepare().unwrap();
        let stale = stale.prepare().unwrap();
        // Independent state advances after capture and before prepared publication.
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let current = restore(&batch, &fixture.params).unwrap();
        let mut changed = StagedEngines::new(&current.brn, &current.trst);
        changed.brn_mut().get_wallet_mut(&other).unwrap().total_burned = 3;
        changed.trst_mut().debit_wallet_with_provenance(&other, &other_origin, 4);
        changed.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let expected = {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let result = apply(&mut batch, &send, &fixture.params, send.timestamp).unwrap();
            (result.trst.save_wallets(), bincode::serialize(result.brn.get_wallet(&other).unwrap()).unwrap())
        };
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let state = prepared.stage(&mut batch, &fixture.params).unwrap().unwrap();
        assert_eq!(state.trst.save_wallets(), expected.0);
        assert_eq!(bincode::serialize(state.brn.get_wallet(&other).unwrap()).unwrap(), expected.1);
        assert_eq!(state.trst.transferable_balance_snapshot(&other), Some(16));
        batch.commit().unwrap();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        assert!(matches!(stale.stage(&mut batch, &fixture.params),
                        Err(crate::economic_candidate::prepared::PreparedStageError::BeforeWrite(_))));
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, send.hash);
        assert_eq!(batch.get_pending(&fixture.address, &send.hash.into_tx_hash()).unwrap().amount, 5);
    }

    #[test]
    fn captured_economic_inputs_reject_writer_changes_before_publication() {
        use crate::economic_candidate::inputs::LedgerInputs;
        let fixture = Fixture::new();
        let mut block = fixture.next(BlockType::ChangeRepresentative, 100);
        sign(&mut block);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let inputs = LedgerInputs::capture(&batch, &block, &fixture.params).unwrap();
        inputs.recheck(&batch, &block, &fixture.params).unwrap();
        let mut account = batch.get_account(&fixture.address).unwrap();
        account.total_brn_staked += 1;
        batch.put_account_info(&account, false).unwrap();
        assert!(inputs.recheck(&batch, &block, &fixture.params).is_err());
        drop(batch);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        inputs.recheck(&batch, &block, &fixture.params).unwrap();
        let mut params = fixture.params.clone();
        params.trst_expiry_secs += 1;
        assert!(inputs.recheck(&batch, &block, &params).is_err());
        let mut altered = block.clone();
        altered.timestamp = Timestamp::new(101);
        assert!(inputs.recheck(&batch, &altered, &fixture.params).is_err());
        batch.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY,
            BlockHash::new([99; 32]).as_bytes()).unwrap();
        assert!(inputs.recheck(&batch, &block, &fixture.params).is_err());
        drop(batch);
        let mut burn = fixture.next(BlockType::Burn, 100);
        burn.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        burn.brn_spent = 1;
        fixture.commit(&mut burn);
        let mut receive = fixture.next(BlockType::Receive, 101);
        receive.link = burn.hash;
        receive.trst_balance = 1;
        sign(&mut receive);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let inputs = LedgerInputs::capture(&batch, &receive, &fixture.params).unwrap();
        let mut pending = batch.get_pending(&fixture.address, &burn.hash.into_tx_hash()).unwrap();
        pending.amount += 1;
        batch.put_pending(&fixture.address, burn.hash.as_bytes(), &bincode::serialize(&pending).unwrap()).unwrap();
        assert!(inputs.recheck(&batch, &receive, &fixture.params).is_err());
        drop(batch);
        let batch = fixture.store.tx_begin_write().unwrap();
        inputs.recheck(&batch, &receive, &fixture.params).unwrap();
        assert!(batch.get_block(&receive.hash).is_err());
    }

    #[test]
    fn grouped_prepared_payments_reopen_and_rewind_each_block_independently() {
        use crate::economic_candidate::prepared::CapturedPayment;
        for stale_follower in [false, true] {
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut first = fixture.next(BlockType::Burn, 104);
        first.brn_spent += 7;
        first.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        sign(&mut first);
        let key = burst_crypto::keypair_from_seed(&[17; 32]);
        let other = burst_crypto::derive_address(&key.public);
        let mut open = first.clone();
        open.block_type = BlockType::Open;
        open.account = other.clone();
        open.representative = other.clone();
        open.previous = BlockHash::ZERO;
        open.link = BlockHash::ZERO;
        open.brn_spent = 0;
        open.trst_balance = 0;
        open.hash = open.compute_hash();
        open.signature = burst_crypto::sign_message(open.hash.as_bytes(), &key.private);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        apply(&mut batch, &open, &fixture.params, open.timestamp).unwrap();
        batch.commit().unwrap();
        let mut second = open.clone();
        second.block_type = BlockType::ChangeRepresentative;
        second.previous = open.hash;
        second.timestamp = Timestamp::new(105);
        second.representative = fixture.address.clone();
        second.hash = second.compute_hash();
        second.signature = burst_crypto::sign_message(second.hash.as_bytes(), &key.private);
        let batch = fixture.store.tx_begin_write().unwrap();
        let before = restore(&batch, &fixture.params).unwrap();
        let prepare = |block: &StateBlock| {
            authenticate(block, &fixture.params, block.timestamp).unwrap();
            CapturedPayment::capture(&batch, block, &fixture.params).unwrap().unwrap().prepare().unwrap()
        };
        let first_prepared = prepare(&first);
        let second_prepared = prepare(&second);
        batch.commit().unwrap();
        if stale_follower {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            let mut account = batch.get_account(&second.account).unwrap();
            account.confirmation_height = account.block_count;
            batch.put_account_info(&account, false).unwrap();
            batch.commit().unwrap();
        }
        let generation = fixture.store.env().info().last_txn_id;
        let mut group = fixture.store.tx_begin_block_group().unwrap();
        first_prepared.stage(&mut group, &fixture.params).unwrap().unwrap();
        second_prepared.stage_revalidated(&mut group, &fixture.params).unwrap().unwrap();
        group.commit().unwrap();
        assert_eq!(fixture.store.env().info().last_txn_id, generation + 1);
        drop(fixture.store);
        let store = LmdbStore::open(fixture.directory.path(), 32, 32 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&first.account).unwrap().head, first.hash);
        assert_eq!(batch.get_account(&second.account).unwrap().head, second.hash);
        for block in [&first, &second] {
            let undo = batch.block_undo(&block.hash).unwrap().unwrap();
            assert_eq!(undo.account, block.account);
            assert_eq!(undo.height, batch.get_account(&block.account).unwrap().block_count);
        }
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&second.hash).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&first.account).unwrap().head, first.hash);
        assert_eq!(batch.get_account(&second.account).unwrap().head, open.hash);
        if stale_follower {
            assert_eq!(batch.get_account(&second.account).unwrap().confirmation_height, 1);
        }
        batch.rewind_block(&first.hash).unwrap();
        batch.commit().unwrap();
        let batch = store.tx_begin_write().unwrap();
        let restored = restore(&batch, &fixture.params).unwrap();
        assert_eq!(restored.trst.save_wallets(), before.trst.save_wallets());
        assert_eq!(restored.trst.merger_graph.to_bytes(), before.trst.merger_graph.to_bytes());
        assert_eq!(bincode::serialize(restored.brn.get_wallet(&first.account).unwrap()).unwrap(),
            bincode::serialize(before.brn.get_wallet(&first.account).unwrap()).unwrap());
        assert_eq!(batch.get_account(&first.account).unwrap().head, first.previous);
        assert!(batch.get_pending(&first.account, &first.hash.into_tx_hash()).is_err());
        }
    }

    #[test]
    fn grouped_prepared_conflict_aborts_prior_members_without_consuming_pending() {
        use crate::economic_candidate::prepared::{CapturedPayment, PreparedStageError};
        let fixture = Fixture::new();
        fixture.create_holdings();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let initial = restore(&batch, &fixture.params).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &initial.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &initial.trst).unwrap();
        batch.commit().unwrap();
        let mut send = fixture.next(BlockType::Send, 104);
        send.link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
        send.origin = initial.trst.wallets[&fixture.address].tokens[0].origin;
        send.trst_balance -= 5;
        sign(&mut send);
        let batch = fixture.store.tx_begin_write().unwrap();
        let first_prepared = CapturedPayment::capture(&batch, &send, &fixture.params).unwrap().unwrap().prepare().unwrap();
        let second_prepared = CapturedPayment::capture(&batch, &send, &fixture.params).unwrap().unwrap().prepare().unwrap();
        batch.commit().unwrap();
        let generation = fixture.store.env().info().last_txn_id;
        let mut group = fixture.store.tx_begin_block_group().unwrap();
        first_prepared.stage(&mut group, &fixture.params).unwrap().unwrap();
        assert!(matches!(second_prepared.stage_revalidated(&mut group, &fixture.params), Err(PreparedStageError::BeforeWrite(_))));
        drop(group); // The admission caller aborts before serial isolation.
        assert_eq!(fixture.store.env().info().last_txn_id, generation);
        let mut batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&send.account).unwrap().head, send.previous);
        assert!(batch.block_undo(&send.hash).unwrap().is_none());
        assert!(batch.get_pending(&send.account, &send.hash.into_tx_hash()).is_err());
        apply(&mut batch, &send, &fixture.params, send.timestamp).unwrap();
        batch.commit().unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(batch.get_pending(&send.account, &send.hash.into_tx_hash()).unwrap().amount, 5);
    }

    struct Fixture {
        directory: tempfile::TempDir,
        store: LmdbStore,
        params: ProtocolParams,
        address: WalletAddress,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_capacity(1000, 32 * 1024 * 1024)
        }

        fn with_capacity(expiry_secs: u64, map_size: usize) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, map_size).unwrap();
            let params = ProtocolParams {
                min_work_difficulty: 0,
                trst_expiry_secs: expiry_secs,
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
                brn_spent: 0,
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

        fn create_holdings(&self) -> Vec<TxHash> {
            let mut ids = Vec::new();
            for (time, amount) in [(100, 20), (102, 30)] {
                let mut burn = self.next(BlockType::Burn, time);
                burn.brn_spent += amount;
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
        burn.brn_spent = 10;
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
        burn.brn_spent = 10;
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
            let error = apply(&mut batch, &stale, &fixture.params, stale.timestamp)
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("signing-device grant is no longer active"),
                "{error}"
            );
            assert_eq!(
                batch.get_account(&fixture.address).unwrap().head,
                handoff.hash
            );
            assert!(batch.get_block(&stale.hash).is_err());
        }
        fixture.commit(&mut successor);
        let batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_account(&fixture.address).unwrap().head,
            successor.hash
        );
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
        block.brn_spent = 10;
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
        valid.brn_spent = 10;
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
        block.brn_spent = 10;
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
        for uses_row_storage in [false, true] {
            check_merge_replacement(uses_row_storage);
        }
    }

    fn check_merge_replacement(uses_row_storage: bool) {
        let fixture = Fixture::new();
        let ids = fixture.create_holdings();
        let unrelated: Vec<_> = (1..=32)
            .map(|seed| {
                let keys = burst_crypto::keypair_from_seed(&[seed; 32]);
                let address = burst_crypto::derive_address(&keys.public);
                let wallet = burst_brn::BrnWalletState::new(Timestamp::new(seed as u64));
                (address, bincode::serialize(&wallet).unwrap())
            })
            .collect();
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            for (address, bytes) in &unrelated {
                batch.put_brn_wallet(address, bytes).unwrap();
            }
            if uses_row_storage {
                let state = restore(&batch, &fixture.params).unwrap();
                crate::trst_storage::stage_rows_for_test(&mut batch, &state.trst).unwrap();
                crate::graph_storage::stage_rows_for_test(
                    &mut batch,
                    &state.trst.merger_graph,
                )
                .unwrap();
            }
            batch.commit().unwrap();
        }
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
        assert_eq!(crate::trst_storage::uses_row_storage(&batch).unwrap(), uses_row_storage);
        assert_eq!(batch.uses_graph_records().unwrap(), uses_row_storage);
        for (address, bytes) in &unrelated {
            assert_eq!(
                bincode::serialize(state.brn.get_wallet(address).unwrap()).unwrap(),
                *bytes,
                "replacement must preserve unrelated wallet state"
            );
        }
        assert_eq!(state.trst.get_portfolio(&address).unwrap().tokens.len(), 2);
        assert!(!state
            .trst
            .merger_graph
            .contains_merge(&merge.hash.into_tx_hash()));
    }

    #[test]
    fn expired_consolidation_keeps_the_amount_but_cannot_restore_spendability() {
        let fixture = Fixture::new();
        let ids = fixture.create_holdings();
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
        let ids = fixture.create_holdings();
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
                block.brn_spent = balances.0;
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
        burn.brn_spent = 20;
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
        burn.brn_spent = 20;
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
