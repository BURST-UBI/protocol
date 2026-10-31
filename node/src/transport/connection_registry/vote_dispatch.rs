//! Batch incoming votes under the election write lock after caller authentication.
//! Queue capacity counts pending requests, excluding the active batch and callers
//! waiting to enqueue.
use super::*;
use crate::wire_message::WireVote;
use tokio::sync::oneshot;

const QUEUE_CAPACITY: usize = 256;
const MAX_BATCH: usize = 32;

#[derive(Debug)]
struct Request {
    peer: String,
    vote: WireVote,
    received_at: Timestamp,
    span: tracing::Span,
    done: oneshot::Sender<()>,
}

#[derive(Clone)]
pub(super) struct Dispatcher {
    sender: mpsc::Sender<Request>,
    elections: Arc<RwLock<ActiveElections>>,
}

impl Dispatcher {
    /// Wait for routing or skip a vote already known to have no effect.
    /// Success does not imply acceptance or quorum. Cancelling after enqueue
    /// does not remove the queued request.
    pub(super) async fn dispatch(&self, peer: &str, vote: &WireVote) -> std::io::Result<()> {
        if let Ok(active) = self.elections.try_read() {
            if vote.block_hashes.iter().all(|hash| {
                active.signed_vote_has_no_effect(hash, &vote.voter, vote.is_final, vote.sequence)
            }) {
                return Ok(());
            }
        }
        let (done, completed) = oneshot::channel();
        self.sender
            .send(Request {
                peer: peer.to_owned(),
                vote: vote.clone(),
                received_at: Timestamp::new(unix_now_secs()),
                span: tracing::Span::current(),
                done,
            })
            .await
            .map_err(|_| closed())?;
        // Preserve local routing before relay/history. Queue acceptance alone is
        // not completion, and closing the worker must not acknowledge lost work.
        completed.await.map_err(|_| closed())
    }
}

fn closed() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "vote dispatcher closed")
}

pub(super) fn spawn(
    elections: Arc<RwLock<ActiveElections>>,
    weights: Arc<RwLock<RepWeightCache>>,
) -> (Dispatcher, tokio::task::JoinHandle<()>) {
    let (sender, mut incoming) = mpsc::channel(QUEUE_CAPACITY);
    let dispatcher = Dispatcher {
        sender,
        elections: elections.clone(),
    };
    let task = tokio::spawn(async move {
        let diagnostics = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
        while let Some(first) = incoming.recv().await {
            let started = diagnostics.then(std::time::Instant::now);
            let routed = route_batch(first, &mut incoming, &elections, &weights).await;
            if let Some(started) = started {
                tracing::debug!(
                    count = routed.len(),
                    elapsed_us = started.elapsed().as_micros(),
                    "incoming vote batch routed"
                );
            }
            // No election or weight guard survives route_batch. Cancellation
            // while waiting for those locks drops requests without a false ack.
            for request in routed {
                let _ = request.done.send(());
            }
        }
    });
    (dispatcher, task)
}

async fn route_batch(
    first: Request,
    incoming: &mut mpsc::Receiver<Request>,
    elections: &RwLock<ActiveElections>,
    weights: &RwLock<RepWeightCache>,
) -> Vec<Request> {
    // Match authority publication's weights -> elections order. No awaits after
    // both guards are acquired. Frozen per-root authority still overrides weight.
    let weights = weights.read().await;
    let mut active = elections.write().await;
    let mut batch = Vec::with_capacity(MAX_BATCH);
    batch.push(first);
    while batch.len() < MAX_BATCH {
        let Ok(next) = incoming.try_recv() else {
            break;
        };
        batch.push(next);
    }
    for request in &batch {
        let _span = request.span.enter();
        route_vote(
            &request.peer,
            &request.vote,
            weights.weight(&request.vote.voter),
            &mut active,
            request.received_at,
        );
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        hash: BlockHash,
        voter: &burst_types::WalletAddress,
        sequence: u64,
        is_final: bool,
    ) -> (Request, oneshot::Receiver<()>) {
        let (done, completed) = oneshot::channel();
        (
            Request {
                peer: "peer".into(),
                vote: WireVote {
                    voter: voter.clone(),
                    block_hashes: vec![hash],
                    is_final,
                    sequence,
                    timestamp: unix_now_secs() * 1_000,
                    signature: burst_types::Signature([0; 64]),
                },
                received_at: Timestamp::new(101),
                span: tracing::Span::none(),
                done,
            },
            completed,
        )
    }

    #[tokio::test]
    async fn batches_are_bounded_and_use_current_frozen_weights_and_signed_order() {
        let voter = burst_types::WalletAddress::new("brst_batch");
        let other = burst_types::WalletAddress::new("brst_other");
        let hash = BlockHash::new([41; 32]);
        let early = BlockHash::new([42; 32]);
        // Processing happens long after this synthetic arrival. Queue delay
        // must not replace receipt time and age a timely vote out of the election.
        let now = Timestamp::new(100);
        let mut active = ActiveElections::new(8, 100);
        active
            .set_authority(HashMap::from([(voter.clone(), 50), (other, 50)]))
            .unwrap();
        active.register_candidate(hash, hash, now).unwrap();
        let active = RwLock::new(active);
        // Live weight zero cannot override the frozen electorate's fifty.
        let weights = RwLock::new(RepWeightCache::new());
        let (sender, mut incoming) = mpsc::channel(QUEUE_CAPACITY);
        let mut completions = Vec::new();
        for (sequence, final_) in [(10, false), (9, false), (8, true), (11, false)] {
            let (request, completion) = request(hash, &voter, sequence, final_);
            sender.try_send(request).unwrap();
            completions.push(completion);
        }
        for _ in 4..=MAX_BATCH {
            let (request, completion) = request(early, &voter, 12, true);
            sender.try_send(request).unwrap();
            completions.push(completion);
        }
        let first = incoming.recv().await.unwrap();
        let batch = route_batch(first, &mut incoming, &active, &weights).await;
        assert_eq!(batch.len(), MAX_BATCH);
        assert_eq!(
            incoming.len(),
            1,
            "the next arrival stays for another bounded batch"
        );
        assert!(completions
            .iter_mut()
            .all(|done| matches!(done.try_recv(), Err(oneshot::error::TryRecvError::Empty))));
        let mut active = active.write().await;
        let final_vote = &active.get_election(&hash).unwrap().last_votes[&voter];
        assert_eq!(
            (final_vote.sequence, final_vote.is_final, final_vote.weight),
            (8, true, 50)
        );
        active.register_candidate(early, early, now).unwrap();
        assert_eq!(
            active.get_election(&early).unwrap().last_votes[&voter].weight,
            50
        );
        drop(active);
        for request in batch {
            request.done.send(()).unwrap();
        }
        for completion in completions.iter_mut().take(MAX_BATCH) {
            completion.try_recv().unwrap();
        }
    }

    #[tokio::test]
    async fn full_queue_backpressures_and_shutdown_never_acknowledges_unrouted_votes() {
        let elections = Arc::new(RwLock::new(ActiveElections::new(8, 100)));
        let voter = burst_types::WalletAddress::new("brst_batch");
        let hash = BlockHash::new([43; 32]);
        let (sender, incoming) = mpsc::channel(QUEUE_CAPACITY);
        let dispatcher = Dispatcher {
            sender,
            elections: elections.clone(),
        };
        let mut completions = Vec::new();
        for _ in 0..QUEUE_CAPACITY {
            let (request, completion) = request(hash, &voter, 1, true);
            dispatcher.sender.try_send(request).unwrap();
            completions.push(completion);
        }
        let (request, _) = request(hash, &voter, 1, true);
        let waiting = dispatcher.dispatch("peer", &request.vote);
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            result = &mut waiting => panic!("full queue completed: {result:?}"),
            _ = tokio::task::yield_now() => {},
        }
        drop(incoming);
        assert!(waiting.await.is_err());
        for completion in completions {
            assert!(completion.await.is_err());
        }

        let held = elections.write().await;
        let (dispatcher, task) = spawn(
            elections.clone(),
            Arc::new(RwLock::new(RepWeightCache::new())),
        );
        let waiting = dispatcher.dispatch("peer", &request.vote);
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            result = &mut waiting => panic!("routing completed while writer held: {result:?}"),
            _ = tokio::task::yield_now() => {},
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(waiting.await.is_err());
        assert_eq!(held.election_count(), 0);
    }
}
