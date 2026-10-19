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
        processor.validate_authenticated_candidate(&valid, &frontier),
        ProcessResult::Accepted
    );
    let mut changed = original.clone();
    changed.trst_balance += 1;
    rejected(
        processor.validate_authenticated_candidate(&rules.authenticate(changed), &frontier),
        "hash",
    );
    for signature in [Signature([0; 64]), Signature([7; 64])] {
        let mut changed = original.clone();
        changed.signature = signature;
        assert_eq!(changed.hash, original.hash);
        rejected(
            processor.validate_authenticated_candidate(&rules.authenticate(changed), &frontier),
            "signature",
        );
    }
    let mut changed = original;
    changed.version = 0;
    resign(&mut changed);
    rejected(
        processor.validate_authenticated_candidate(&rules.authenticate(changed), &frontier),
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
        processor.validate_authenticated_candidate(&authenticated, &frontier),
        "epoch",
    );
    processor.install_payment_epoch(candidate.payment_epoch, candidate.timestamp.as_secs() + 1);
    rejected(
        processor.validate_authenticated_candidate(&authenticated, &frontier),
        "cutoff",
    );
    processor.install_payment_epoch(candidate.payment_epoch, 0);
    let mut params = ProtocolParams {
        min_work_difficulty: 0,
        ..ProtocolParams::default()
    };
    processor.install_params(&params);
    rejected(
        processor.validate_authenticated_candidate(&authenticated, &frontier),
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
        processor.validate_authenticated_candidate(&authenticated, &frontier),
        "past",
    );
    time.store(candidate.timestamp.as_secs(), Ordering::SeqCst);
    params.min_work_difficulty = u64::MAX;
    processor.install_params(&params);
    rejected(
        processor.validate_authenticated_candidate(&authenticated, &frontier),
        "proof-of-work",
    );

    // A threshold reduction must not preserve an old worker's rejection.
    let old_rejection = processor.authentication_rules().authenticate(candidate);
    assert!(old_rejection.result().is_err());
    params.min_work_difficulty = 0;
    processor.install_params(&params);
    assert_eq!(
        processor.validate_authenticated_candidate(&old_rejection, &frontier),
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
        processor.validate_authenticated_candidate(&authenticated, &DagFrontier::new()),
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
        processor.validate_authenticated_candidate(&first, &frontier),
        ProcessResult::Accepted
    );
    assert!(frontier.get_head(&first.block().account).is_none());
    processor.publish_committed(first.block(), &mut frontier);
    assert_eq!(
        processor.validate_authenticated_candidate(&competitor, &frontier),
        ProcessResult::Fork
    );
    assert_eq!(
        processor.validate_authenticated_candidate(&first, &frontier),
        ProcessResult::Duplicate
    );

    let mut dependent = block(2);
    dependent.block_type = BlockType::Send;
    dependent.previous = BlockHash::new([99; 32]);
    resign(&mut dependent);
    let dependent = processor.authentication_rules().authenticate(dependent);
    assert_eq!(
        processor.validate_authenticated_candidate(&dependent, &frontier),
        ProcessResult::Gap
    );
    assert_eq!(processor.unchecked_count(), 1);
    let mut appeal = block(3);
    appeal.block_type = BlockType::Challenge;
    appeal.target_head = BlockHash::new([99; 32]);
    resign(&mut appeal);
    let appeal = processor.authentication_rules().authenticate(appeal);
    rejected(
        processor.validate_authenticated_candidate(&appeal, &frontier),
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
        processor.validate_authenticated_candidate(&authenticated, &DagFrontier::new()),
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
        assert!(validation.pending.len() < 4);
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
