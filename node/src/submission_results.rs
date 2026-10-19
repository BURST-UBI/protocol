//! Bounded in-memory replies for local RPC submissions. A full-payload digest
//! prevents an invalid signature/work variant from poisoning another submission
//! with the same declared block hash. Receipts are not evidence of finality.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use burst_rpc::ProcessResult;
use tokio::sync::Notify;

type PayloadId = [u8; 32];

pub(crate) struct SubmissionResults {
    capacity: usize,
    entries: Mutex<Entries>,
    changed: Notify,
}

#[derive(Default)]
struct Entries {
    results: HashMap<PayloadId, Option<ProcessResult>>,
    order: VecDeque<PayloadId>,
}

impl SubmissionResults {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            entries: Mutex::new(Entries::default()),
            changed: Notify::new(),
        }
    }

    pub fn track(&self, payload: &[u8]) {
        let id = burst_crypto::blake2b_256(payload);
        let mut entries = self
            .entries
            .lock()
            .expect("submission results lock poisoned");
        if !entries.results.contains_key(&id) {
            if entries.results.len() == self.capacity {
                if let Some(oldest) = entries.order.pop_front() {
                    entries.results.remove(&oldest);
                }
            }
            entries.order.push_back(id);
        }
        entries.results.insert(id, None);
    }

    pub fn complete(&self, payload: &[u8], result: ProcessResult) {
        if !matches!(
            result,
            ProcessResult::Accepted | ProcessResult::Fork | ProcessResult::Rejected(_)
        ) {
            return;
        }
        let id = burst_crypto::blake2b_256(payload);
        let mut entries = self
            .entries
            .lock()
            .expect("submission results lock poisoned");
        if let Some(entry) = entries.results.get_mut(&id) {
            *entry = Some(result);
            self.changed.notify_waiters();
        }
    }

    pub async fn wait(&self, payload: &[u8]) -> Option<ProcessResult> {
        let id = burst_crypto::blake2b_256(payload);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let entries = self
                    .entries
                    .lock()
                    .expect("submission results lock poisoned");
                match entries.results.get(&id) {
                    Some(Some(result)) => return Some(result.clone()),
                    None => return None,
                    Some(None) => {}
                }
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[tokio::test]
    async fn fork_receipt_completes_without_claiming_acceptance() {
        let results = SubmissionResults::new(1);
        results.track(b"fork");
        results.complete(b"fork", ProcessResult::Fork);
        assert_eq!(results.wait(b"fork").await, Some(ProcessResult::Fork));
    }

    #[tokio::test]
    async fn completion_before_or_after_wait_is_not_lost() {
        let results = Arc::new(SubmissionResults::new(2));
        results.track(b"first");
        results.complete(b"first", ProcessResult::Accepted);
        assert_eq!(results.wait(b"first").await, Some(ProcessResult::Accepted));
        results.track(b"second");
        let pending = results.clone();
        let waiter = tokio::spawn(async move { pending.wait(b"second").await });
        tokio::task::yield_now().await;
        results.complete(b"second", ProcessResult::Rejected("reason".into()));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .unwrap()
                .unwrap(),
            Some(ProcessResult::Rejected("reason".into()))
        );
    }

    #[tokio::test]
    async fn untracked_and_evicted_payloads_do_not_allocate_or_claim_rejection() {
        let results = SubmissionResults::new(1);
        results.complete(b"peer-only", ProcessResult::Rejected("bad".into()));
        assert_eq!(results.wait(b"peer-only").await, None);
        results.track(b"a");
        results.track(b"b");
        assert_eq!(results.wait(b"a").await, None);
        assert_eq!(results.entries.lock().unwrap().results.len(), 1);
    }

    #[tokio::test]
    async fn different_payloads_and_nonterminal_outcomes_cannot_reject_a_submission() {
        let results = SubmissionResults::new(2);
        results.track(b"same hash, valid signature");
        results.track(b"same hash, forged signature");
        results.complete(
            b"same hash, forged signature",
            ProcessResult::Rejected("bad signature".into()),
        );
        results.complete(b"same hash, valid signature", ProcessResult::Gap);
        assert!(tokio::time::timeout(
            Duration::from_millis(5),
            results.wait(b"same hash, valid signature")
        )
        .await
        .is_err());
        results.complete(b"same hash, valid signature", ProcessResult::Accepted);
        assert_eq!(
            results.wait(b"same hash, valid signature").await,
            Some(ProcessResult::Accepted)
        );
    }
}
