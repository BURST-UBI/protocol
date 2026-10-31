use super::*;
use crate::block_processor::{BlockProcessor, ProcessResult};
use burst_crypto::{derive_address, keypair_from_seed, sign_message};
use burst_ledger::{BlockType, DagFrontier, StateBlock};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

fn block(sequence: u64) -> StateBlock {
    let keys = keypair_from_seed(&[17; 32]);
    let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
    block.account = derive_address(&keys.public);
    block.representative = block.account.clone();
    block.payment_epoch = BlockHash::new([1; 32]);
    block.timestamp = Timestamp::now();
    block.transaction = TxHash::new(burst_crypto::blake2b_256(&sequence.to_le_bytes()));
    resign(&mut block);
    block
}

fn resign(block: &mut StateBlock) {
    block.hash = block.compute_hash();
    block.signature = sign_message(block.hash.as_bytes(), &keypair_from_seed(&[17; 32]).private);
}

fn processor() -> BlockProcessor {
    BlockProcessor::new(0, BlockHash::new([1; 32]))
}

fn rejected(result: ProcessResult, message: &str) {
    assert!(
        matches!(result, ProcessResult::Rejected(ref reason) if reason.contains(message)),
        "{result:?}"
    );
}

#[test]
fn authentication_cannot_be_reused_for_modified_contents_or_signatures() {
    let mut processor = processor();
    let frontier = DagFrontier::new();
    let original = block(0);
    let rules = processor.authentication_rules();
    let valid = rules.authenticate(original.clone());
    assert_eq!(
        processor.validate_candidate_with_authentication(&valid, &frontier),
        ProcessResult::Accepted
    );
    let mut changed = original.clone();
    changed.trst_balance += 1;
    rejected(
        processor.validate_candidate_with_authentication(&rules.authenticate(changed), &frontier),
        "hash",
    );
    for signature in [Signature([0; 64]), Signature([7; 64])] {
        let mut changed = original.clone();
        changed.signature = signature;
        assert_eq!(changed.hash, original.hash);
        rejected(
            processor.validate_candidate_with_authentication(&rules.authenticate(changed), &frontier),
            "signature",
        );
    }
    let mut changed = original;
    changed.version = 0;
    resign(&mut changed);
    rejected(
        processor.validate_candidate_with_authentication(&rules.authenticate(changed), &frontier),
        "version",
    );
    assert_eq!(processor.unchecked_count(), 0);
}

#[test]
fn current_epoch_params_clock_and_work_are_checked_after_authentication() {
    let candidate = block(0);
    let mut processor = processor();
    let frontier = DagFrontier::new();
    let authenticated = processor
        .authentication_rules()
        .authenticate(candidate.clone());
    processor.install_payment_epoch(BlockHash::new([2; 32]), 0);
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &frontier),
        "epoch",
    );
    processor.install_payment_epoch(candidate.payment_epoch, candidate.timestamp.as_secs() + 1);
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &frontier),
        "cutoff",
    );
    processor.install_payment_epoch(candidate.payment_epoch, 0);
    let mut params = ProtocolParams {
        min_work_difficulty: 0,
        ..ProtocolParams::default()
    };
    processor.install_params(&params);
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &frontier),
        "parameters",
    );

    let mut candidate = candidate;
    candidate.params_hash = params.params_hash();
    resign(&mut candidate);
    let authenticated = processor
        .authentication_rules()
        .authenticate(candidate.clone());
    let time = Arc::new(AtomicU64::new(candidate.timestamp.as_secs() + 3601));
    processor.test_time = Some(time.clone());
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &frontier),
        "past",
    );
    time.store(candidate.timestamp.as_secs(), Ordering::SeqCst);
    params.min_work_difficulty = u64::MAX;
    processor.install_params(&params);
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &frontier),
        "proof-of-work",
    );

    // A threshold reduction must not preserve an old worker's rejection.
    let old_rejection = processor.authentication_rules().authenticate(candidate);
    assert!(old_rejection.result().is_err());
    params.min_work_difficulty = 0;
    processor.install_params(&params);
    assert_eq!(
        processor.validate_candidate_with_authentication(&old_rejection, &frontier),
        ProcessResult::Accepted
    );
}

#[test]
fn signature_policy_changes_cannot_reuse_test_only_authentication() {
    let mut processor = processor();
    let mut candidate = block(0);
    candidate.signature = Signature([7; 64]);
    processor.set_verify_signatures(false);
    let authenticated = processor.authentication_rules().authenticate(candidate);
    assert!(authenticated.result().is_ok());
    processor.set_verify_signatures(true);
    rejected(
        processor.validate_candidate_with_authentication(&authenticated, &DagFrontier::new()),
        "signature",
    );
}

#[test]
fn preauthentication_does_not_publish_or_bypass_forks_gaps_or_release_gates() {
    let mut processor = processor();
    let mut frontier = DagFrontier::new();
    let first = processor.authentication_rules().authenticate(block(0));
    let competitor = processor.authentication_rules().authenticate(block(1));
    assert_eq!(
        processor.validate_candidate_with_authentication(&first, &frontier),
        ProcessResult::Accepted
    );
    assert!(frontier.get_head(&first.block().account).is_none());
    processor.publish_committed(first.block(), &mut frontier);
    assert_eq!(
        processor.validate_candidate_with_authentication(&competitor, &frontier),
        ProcessResult::Fork
    );
    assert_eq!(
        processor.validate_candidate_with_authentication(&first, &frontier),
        ProcessResult::Duplicate
    );

    let mut dependent = block(2);
    dependent.block_type = BlockType::Send;
    dependent.previous = BlockHash::new([99; 32]);
    resign(&mut dependent);
    let dependent = processor.authentication_rules().authenticate(dependent);
    assert_eq!(
        processor.validate_candidate_with_authentication(&dependent, &frontier),
        ProcessResult::Gap
    );
    assert_eq!(processor.unchecked_count(), 1);
    let mut appeal = block(3);
    appeal.block_type = BlockType::Challenge;
    appeal.target_head = BlockHash::new([99; 32]);
    resign(&mut appeal);
    let appeal = processor.authentication_rules().authenticate(appeal);
    rejected(
        processor.validate_candidate_with_authentication(&appeal, &frontier),
        "meta-challenge",
    );
}

#[tokio::test]
async fn retention_budget_follows_authentication_gap_and_requeue() {
    let budget = crate::block_retention::BlockRetention::new(2);
    let queue = Arc::new(BlockPriorityQueue::with_retention(2, budget.clone()));
    let mut processor = processor();
    processor.set_retention(budget);
    let parent = BlockHash::new([99; 32]);
    let mut dependent = block(2);
    dependent.block_type = BlockType::Send;
    dependent.previous = parent;
    resign(&mut dependent);
    assert!(queue.push(dependent).await);
    let mut validation =
        BlockValidationQueue::with_workers(queue.clone(), processor.authentication_rules(), 1);
    let authenticated = validation.next().await.unwrap();
    assert!(queue.push(block(3)).await);
    assert!(
        !queue.push(block(4)).await,
        "authentication must retain its slot"
    );
    assert_eq!(
        processor.validate_candidate_with_authentication(&authenticated, &DagFrontier::new()),
        ProcessResult::Gap
    );
    drop(authenticated);
    assert!(
        !queue.push(block(4)).await,
        "unchecked must inherit the slot"
    );
    let mut released = processor.release_retained(&parent);
    assert_eq!(released.len(), 1);
    assert!(
        !queue.push(block(4)).await,
        "recovery batch must retain the slot"
    );
    assert!(
        queue.push_retained(released.pop().unwrap()).await,
        "moving a slot must not allocate twice"
    );
    drop(queue.try_pop_retained().await.unwrap());
    assert!(
        queue.push(block(5)).await,
        "completed work returns capacity"
    );
    drop(queue.try_pop_retained().await.unwrap());
    drop(queue.try_pop_retained().await.unwrap());
    assert!(queue.push(block(6)).await);
    assert!(queue.push(block(7)).await);
    assert!(!queue.push(block(8)).await);
}

#[tokio::test]
async fn queue_matches_priority_order_and_serial_authentication() {
    let queue = Arc::new(BlockPriorityQueue::new(32));
    let expected = BlockPriorityQueue::new(32);
    let rules = processor().authentication_rules();
    for i in 0..32 {
        let mut candidate = block(i);
        if i % 7 == 0 {
            candidate.signature = Signature([7; 64]);
        }
        expected.push(candidate.clone()).await;
        queue.push(candidate).await;
    }
    let mut validation = BlockValidationQueue::with_workers(queue.clone(), rules.clone(), 4);
    for _ in 0..32 {
        let expected = rules.authenticate(expected.pop().await);
        let actual = validation.next().await.unwrap();
        assert_eq!(actual.block().hash, expected.block().hash);
        assert_eq!(actual.result(), expected.result());
        assert!(validation.pending.len() <= 4);
    }
    assert!(validation.pending.is_empty());
    assert!(queue.is_empty().await);
}

#[tokio::test]
async fn workers_overlap_are_bounded_and_survive_cancelled_next() {
    let queue = Arc::new(BlockPriorityQueue::new(5));
    for i in 0..5 {
        queue.push(block(i)).await;
    }
    let mut validation =
        BlockValidationQueue::with_workers(queue.clone(), processor().authentication_rules(), 2);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = gate.clone();
    validation.before_authenticate = Some(Arc::new(move || {
        started.send(()).unwrap();
        let (lock, wake) = &*worker_gate;
        let ready = lock.lock().unwrap();
        // Bound the test's blocking workers even if an assertion fails.
        let (ready, _) = wake
            .wait_timeout_while(ready, Duration::from_secs(5), |ready| !*ready)
            .unwrap();
        assert!(*ready, "test did not release workers");
    }));
    let mut next = Box::pin(validation.next());
    tokio::select! {
        result = &mut next => panic!("blocked workers returned: {}", result.is_ok()),
        _ = async {
            for _ in 0..2 {
                tokio::time::timeout(Duration::from_secs(2), observed.recv()).await.unwrap().unwrap();
            }
        } => {}
    }
    drop(next); // Models a checkpoint or shutdown winning the node's select.
    assert_eq!(validation.pending.len(), 2);
    assert_eq!(queue.len().await, 3);
    assert!(observed.try_recv().is_err());
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let mut received = std::collections::HashSet::new();
    for _ in 0..5 {
        let candidate = validation.next().await.unwrap();
        assert!(candidate.result().is_ok());
        assert!(received.insert(candidate.block().hash));
    }
    assert!(validation.pending.is_empty());
    assert!(queue.is_empty().await);
}

#[tokio::test]
async fn worker_panic_is_reported_not_treated_as_valid_authentication() {
    let queue = Arc::new(BlockPriorityQueue::new(1));
    queue.push(block(0)).await;
    let mut validation =
        BlockValidationQueue::with_workers(queue, processor().authentication_rules(), 1);
    validation.before_authenticate = Some(Arc::new(|| panic!("injected worker failure")));
    assert!(validation.next().await.err().unwrap().is_panic());
    assert!(validation.in_flight.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identical_payload_is_coalesced_through_worker_and_admission_then_retryable() {
    let queue = Arc::new(BlockPriorityQueue::new(4));
    let mut validation =
        BlockValidationQueue::with_workers(queue.clone(), processor().authentication_rules(), 2);
    let candidate = block(0);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (release, wait) = std::sync::mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    validation.before_authenticate = Some(Arc::new(move || {
        started.send(()).unwrap();
        wait.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap();
    }));
    queue.push(candidate.clone()).await;
    validation.refill_now();
    tokio::time::timeout(Duration::from_secs(2), observed.recv()).await.unwrap().unwrap();
    queue.push(candidate.clone()).await;
    validation.refill_now();
    assert_eq!(validation.pending.len(), 1);
    assert_eq!(validation.in_flight.lock().unwrap().len(), 1);
    release.send(()).unwrap();
    let admitted = validation.next().await.unwrap();
    assert!(admitted.result().is_ok());
    validation.before_authenticate = None;
    queue.push(candidate.clone()).await;
    validation.refill_now();
    assert!(validation.pending.is_empty(), "admission still owns the payload");
    drop(admitted);
    assert!(validation.in_flight.lock().unwrap().is_empty());
    queue.push(candidate).await;
    let retry = validation.next().await.unwrap();
    assert!(retry.result().is_ok());
    drop(retry);
    assert!(validation.in_flight.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unchecked_payload_coalesces_until_dependency_release_expiry_or_epoch_reset() {
    for source_gap in [false, true] {
    for release in ["dependency", "expiry", "epoch"] {
        let queue = Arc::new(BlockPriorityQueue::new(4));
        let mut processor = processor();
        let mut validation = BlockValidationQueue::with_workers(
            queue.clone(), processor.authentication_rules(), 1);
        let parent = BlockHash::new([99; 32]);
        let mut dependent = block(2);
        if source_gap {
            dependent.link = parent;
        } else {
            dependent.block_type = BlockType::Send;
            dependent.previous = parent;
        }
        resign(&mut dependent);
        queue.push(dependent.clone()).await;
        let candidate = validation.next().await.unwrap();
        assert_eq!(processor.validate_candidate_with_authentication(&candidate, &DagFrontier::new()),
            if source_gap { ProcessResult::GapSource } else { ProcessResult::Gap });
        drop(candidate);
        assert_eq!(processor.unchecked_count(), 1);
        assert_eq!(validation.in_flight.lock().unwrap().len(), 1);
        queue.push(dependent.clone()).await;
        validation.refill_now();
        assert!(validation.pending.is_empty(), "unchecked owns the identical payload");
        let mut invalid = dependent.clone();
        invalid.signature = Signature([0; 64]);
        queue.push(invalid).await;
        let variant = validation.next().await.unwrap();
        assert!(variant.result().is_err(), "different payload still authenticates independently");
        drop(variant);
        assert_eq!(validation.in_flight.lock().unwrap().len(), 1);
        if release == "epoch" {
            processor.install_payment_epoch(BlockHash::new([1; 32]), 0);
            queue.push(dependent.clone()).await;
        } else if release == "expiry" {
            assert_eq!(processor.cleanup_unchecked(0, u64::MAX), 1);
            queue.push(dependent.clone()).await;
        } else {
            let mut released = processor.release_retained(&parent);
            assert_eq!(released.len(), 1);
            assert!(queue.push_retained(released.pop().unwrap()).await);
        }
        assert_eq!(processor.unchecked_count(), 0);
        assert!(validation.in_flight.lock().unwrap().is_empty());
        let retry = validation.next().await.unwrap();
        assert!(retry.result().is_ok());
        assert_eq!(retry.block().hash, dependent.hash);
        drop(retry);
        assert!(validation.in_flight.lock().unwrap().is_empty());
    }
    }
}

#[tokio::test]
async fn in_flight_invalid_signature_does_not_suppress_valid_same_hash() {
    let queue = Arc::new(BlockPriorityQueue::new(4));
    let mut validation =
        BlockValidationQueue::with_workers(queue.clone(), processor().authentication_rules(), 2);
    let valid = block(0);
    let mut invalid = valid.clone();
    invalid.signature = Signature([0; 64]);
    queue.push(invalid).await;
    validation.refill_now();
    queue.push(valid.clone()).await;
    validation.refill_now();
    assert_eq!(validation.pending.len(), 2);
    let first = validation.next().await.unwrap();
    assert!(first.result().is_err());
    let second = validation.next().await.unwrap();
    assert_eq!(second.block().hash, valid.hash);
    assert!(second.result().is_ok());
    assert_eq!(validation.in_flight.lock().unwrap().len(), 2);
    drop((first, second));
    assert!(validation.in_flight.lock().unwrap().is_empty());
}

#[tokio::test]
async fn committed_duplicates_do_not_spend_worker_capacity() {
    use burst_store::block::BlockStore;
    let queue = Arc::new(BlockPriorityQueue::new(2));
    let stored = Arc::new(burst_nullables::NullStore::new());
    let duplicate = block(0);
    stored
        .put_block(&duplicate.hash, &bincode::serialize(&duplicate).unwrap())
        .unwrap();
    queue.push(duplicate).await;
    let fresh = block(1);
    queue.push(fresh.clone()).await;
    let mut validation =
        BlockValidationQueue::new(queue, processor().authentication_rules(), stored);
    let starts = Arc::new(AtomicU64::new(0));
    let worker_starts = starts.clone();
    validation.before_authenticate = Some(Arc::new(move || {
        worker_starts.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(validation.next().await.unwrap().block().hash, fresh.hash);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn opportunistic_refill_skips_duplicates_with_a_bounded_scan() {
    use burst_store::block::BlockStore;
    for duplicate_count in [1, PREPARATION_WINDOW] {
        let queue = Arc::new(BlockPriorityQueue::new(duplicate_count + 1));
        let ordered = BlockPriorityQueue::new(duplicate_count + 1);
        for index in 0..=duplicate_count {
            let candidate = block(index as u64);
            assert!(queue.push(candidate.clone()).await);
            assert!(ordered.push(candidate).await);
        }
        let stored = Arc::new(burst_nullables::NullStore::new());
        for _ in 0..duplicate_count {
            let duplicate = ordered.pop().await;
            stored.put_block(&duplicate.hash, &bincode::serialize(&duplicate).unwrap()).unwrap();
        }
        let fresh = ordered.pop().await;
        let mut validation = BlockValidationQueue::with_workers(
            queue.clone(), processor().authentication_rules(), 1);
        validation.ready_limit = PREPARATION_WINDOW;
        validation.stored = Some(stored);
        let starts = Arc::new(AtomicU64::new(0));
        let worker_starts = starts.clone();
        validation.before_authenticate = Some(Arc::new(move || {
            worker_starts.fetch_add(1, Ordering::SeqCst);
        }));
        validation.refill_now();
        if duplicate_count == PREPARATION_WINDOW {
            assert!(validation.pending.is_empty(), "scan must stop at its examination bound");
            assert_eq!(queue.len().await, 1);
            assert_eq!(starts.load(Ordering::SeqCst), 0);
            validation.refill_now();
        }
        assert_eq!(validation.pending.len(), 1, "duplicate must not consume a worker slot");
        assert!(queue.is_empty().await);
        let authenticated = tokio::time::timeout(Duration::from_secs(2), validation.next())
            .await.unwrap().unwrap();
        assert_eq!(authenticated.block().hash, fresh.hash);
        assert!(authenticated.result().is_ok());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
#[ignore = "manual CPU comparison; not a latency assertion"]
async fn authentication_throughput() {
    let blocks: Vec<_> = (0..4096).map(block).collect();
    let rules = processor().authentication_rules();
    let start = Instant::now();
    for candidate in &blocks {
        assert!(rules.authenticate(candidate.clone()).result().is_ok());
    }
    eprintln!(
        "serial authentication: {:?} for {} blocks",
        start.elapsed(),
        blocks.len()
    );
    for workers in [1, 2, 4] {
        let queue = Arc::new(BlockPriorityQueue::new(blocks.len()));
        for candidate in &blocks {
            queue.push(candidate.clone()).await;
        }
        let mut validation = BlockValidationQueue::with_workers(queue, rules.clone(), workers);
        let start = Instant::now();
        for _ in &blocks {
            assert!(validation.next().await.unwrap().result().is_ok());
        }
        eprintln!(
            "{workers} workers: {:?} for {} blocks",
            start.elapsed(),
            blocks.len()
        );
    }
}


#[tokio::test]
async fn one_worker_refills_during_admission_and_ready_delivery_preserves_order() {
    let queue = Arc::new(BlockPriorityQueue::new(3));
    let expected = BlockPriorityQueue::new(3);
    for id in 0..3 {
        let candidate = block(id);
        expected.push(candidate.clone()).await;
        queue.push(candidate).await;
    }
    let mut validation = BlockValidationQueue::with_workers(
        queue.clone(), processor().authentication_rules(), 1);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    validation.before_authenticate = Some(Arc::new(move || { started.send(()).unwrap(); }));
    let first = validation.next().await.unwrap();
    assert_eq!(first.block().hash, expected.pop().await.hash);
    assert_eq!(validation.pending.len(), 1);
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), observed.recv()).await.unwrap().unwrap();
    }
    // No second next() call is needed to start the next authentication job.
    let second = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(result) = validation.try_next_ready() { break result.unwrap(); }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(second.block().hash, expected.pop().await.hash);
    assert_eq!(validation.pending.len(), 1);
    let third = validation.next().await.unwrap();
    assert_eq!(third.block().hash, expected.pop().await.hash);
    assert!(validation.try_next_ready().is_none());
    assert!(queue.is_empty().await);
}

#[tokio::test]
async fn ready_delivery_never_skips_an_unfinished_fifo_head() {
    let queue = Arc::new(BlockPriorityQueue::new(2));
    let mut validation = BlockValidationQueue::with_workers(
        queue, processor().authentication_rules(), 2);
    let rules = processor().authentication_rules();
    let first_rules = rules.clone();
    let first = block(1);
    let first_hash = first.hash;
    let second = block(2);
    let second_hash = second.hash;
    let (release, released) = tokio::sync::oneshot::channel();
    validation.pending.push_back(tokio::spawn(async move {
        released.await.unwrap();
        first_rules.authenticate(first)
    }));
    validation.pending.push_back(tokio::spawn(async move { rules.authenticate(second) }));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !validation.pending[1].is_finished() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(validation.try_next_ready().is_none());
    assert_eq!(validation.pending.len(), 2);
    release.send(()).unwrap();
    assert_eq!(validation.next().await.unwrap().block().hash, first_hash);
    assert_eq!(validation.try_next_ready().unwrap().unwrap().block().hash, second_hash);
}

#[tokio::test]
async fn preparation_window_batches_existing_work_without_more_workers_or_reordering() {
    let queue = Arc::new(BlockPriorityQueue::new(40));
    let expected = BlockPriorityQueue::new(40);
    for i in 0..35 {
        let candidate = block(i);
        assert!(expected.push(candidate.clone()).await);
        assert!(queue.push(candidate).await);
    }
    let mut validation = BlockValidationQueue::with_workers(
        queue.clone(), processor().authentication_rules(), 1);
    validation.ready_limit = PREPARATION_WINDOW;
    let first = validation.next().await.unwrap();
    assert_eq!(first.block().hash, expected.pop().await.hash);
    assert_eq!(validation.ready.len(), PREPARATION_WINDOW - 1);
    assert!(validation.pending.is_empty());
    assert_eq!(queue.len().await, 3);
    for _ in 1..PREPARATION_WINDOW {
        let next = validation.try_next_ready().unwrap().unwrap();
        assert_eq!(next.block().hash, expected.pop().await.hash);
        assert!(validation.pending.len() <= 1);
    }
    for _ in PREPARATION_WINDOW..35 {
        assert_eq!(validation.next().await.unwrap().block().hash, expected.pop().await.hash);
    }
    assert!(validation.ready.is_empty());
    assert!(validation.pending.is_empty());
    assert!(queue.is_empty().await);
    // A single arrival must return without waiting for the remaining window.
    let lone = block(100);
    assert!(queue.push(lone.clone()).await);
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), validation.next())
        .await.unwrap().unwrap().block().hash, lone.hash);
}

#[tokio::test]
async fn cancelled_preparation_window_retains_completed_prefix_and_fixed_budget() {
    let queue = Arc::new(BlockPriorityQueue::new(40));
    let expected = BlockPriorityQueue::new(40);
    for i in 0..3 {
        let candidate = block(i);
        assert!(expected.push(candidate.clone()).await);
        assert!(queue.push(candidate).await);
    }
    let mut validation = BlockValidationQueue::with_workers(
        queue.clone(), processor().authentication_rules(), 1);
    validation.ready_limit = PREPARATION_WINDOW;
    let entered = Arc::new(AtomicU64::new(0));
    let worker_entered = entered.clone();
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = gate.clone();
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    validation.before_authenticate = Some(Arc::new(move || {
        if worker_entered.fetch_add(1, Ordering::SeqCst) == 1 {
            started.send(()).unwrap();
            let guard = worker_gate.0.lock().unwrap();
            let (released, _) = worker_gate.1.wait_timeout_while(
                guard, Duration::from_secs(5), |released| !*released).unwrap();
            assert!(*released, "test did not release second worker");
        }
    }));
    let mut next = Box::pin(validation.next());
    tokio::select! {
        result = &mut next => panic!("window returned before second worker: {}", result.is_ok()),
        result = tokio::time::timeout(Duration::from_secs(2), observed.recv()) => {
            result.unwrap().unwrap();
        }
    }
    drop(next);
    assert_eq!(validation.ready.len(), 1);
    assert_eq!(validation.pending.len(), 1);
    assert_eq!(validation.gathering, Some(3));
    assert!(validation.try_next_ready().is_none());
    // An arrival during cancellation must not enlarge the in-progress budget.
    let late = block(90);
    assert!(queue.push(late.clone()).await);
    assert!(expected.push(late).await);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    // The late arrival can affect remaining dequeue order. Check the fixed
    // window size above and eventual delivery of every distinct payload below.
    let first = validation.next().await.unwrap();
    assert_eq!(validation.ready.len(), 2);
    assert_eq!(entered.load(Ordering::SeqCst), 3);
    let second = validation.try_next_ready().unwrap().unwrap();
    let third = validation.try_next_ready().unwrap().unwrap();
    let fourth = validation.next().await.unwrap();
    let hashes = [first, second, third, fourth].map(|item| item.block().hash);
    let unique: std::collections::HashSet<_> = hashes.into_iter().collect();
    assert_eq!(unique.len(), 4);
    for _ in 0..4 { assert!(unique.contains(&expected.pop().await.hash)); }
}

#[tokio::test]
async fn preparation_window_keeps_worker_failure_after_completed_prefix() {
    let queue = Arc::new(BlockPriorityQueue::new(3));
    for i in 0..3 { assert!(queue.push(block(i)).await); }
    let mut validation = BlockValidationQueue::with_workers(
        queue, processor().authentication_rules(), 1);
    validation.ready_limit = PREPARATION_WINDOW;
    let calls = Arc::new(AtomicU64::new(0));
    let worker_calls = calls.clone();
    validation.before_authenticate = Some(Arc::new(move || {
        assert_ne!(worker_calls.fetch_add(1, Ordering::SeqCst), 1, "injected worker failure");
    }));
    assert!(validation.next().await.unwrap().result().is_ok());
    assert!(validation.try_next_ready().unwrap().err().unwrap().is_panic());
    assert!(validation.next().await.unwrap().result().is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cold_preparation_never_waits_for_writer_with_matching_or_stale_head() {
    use burst_store::account::{AccountInfo, AccountStore};
    for matches_head in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let mut candidate = block(0);
        candidate.block_type = BlockType::ChangeRepresentative;
        candidate.previous = BlockHash::new([7; 32]);
        resign(&mut candidate);
        let mut account = AccountInfo::unopened(candidate.account.clone());
        account.block_count = 1;
        account.head = if matches_head { candidate.previous } else { BlockHash::new([8; 32]) };
        store.account_store().put_account(&account).unwrap();
        let queue = Arc::new(BlockPriorityQueue::new(1));
        queue.push(candidate.clone()).await;
        let mut validation = BlockValidationQueue::with_workers(
            queue, processor().authentication_rules(), 1);
        validation.set_preparation(store.clone(), ProtocolParams::default());
        assert!(processor().authentication_rules().authenticate(candidate).result().is_ok());
        let writer = store.tx_begin_write().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), validation.next()).await;
        drop(writer); // Unblock a broken implementation before reporting failure.
        let authenticated = result.expect("cold preparation waited for the writer").unwrap();
        assert!(authenticated.result().is_ok());
        assert!(authenticated.preparation_fault.is_none());
        assert!(authenticated.prepared.is_none());
    }
}
