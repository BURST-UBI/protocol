//! Exact-block historical admission from final votes in the installed epoch.
//! This authorizes only the age exception; ordinary validation still applies.
use crate::{payment_authority::PaymentAuthority, wire_message::WireVote};
use burst_ledger::StateBlock;
use burst_types::{BlockHash, WalletAddress};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};

const CAPACITY: usize = 256;
const BYTE_BUDGET: usize = 8 * 1024 * 1024;
const RETENTION: Duration = Duration::from_secs(120);

struct Pending {
    block: StateBlock,
    authority: PaymentAuthority,
    voters: HashSet<WalletAddress>,
    weight: u64,
    accepted: bool,
    created: Instant,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct HistoricalFinality {
    pending: Mutex<HashMap<BlockHash, Pending>>,
}

impl HistoricalFinality {
    #[cfg(test)]
    pub(crate) fn collected_weight(&self, hash: &BlockHash) -> Option<u64> {
        self.pending
            .lock()
            .unwrap()
            .get(hash)
            .map(|entry| entry.weight)
    }

    pub(crate) fn request(&self, block: &StateBlock, authority: PaymentAuthority) -> bool {
        if block.payment_epoch != authority.epoch || block.timestamp.as_secs() < authority.cutoff {
            return false;
        }
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        pending.retain(|_, entry| entry.created.elapsed() < RETENTION);
        if pending.contains_key(&block.hash) {
            return false;
        }
        let Ok(block_bytes) = bincode::serialized_size(block) else {
            return false;
        };
        let Ok(roster_bytes) = bincode::serialized_size(authority.roster.humans()) else {
            return false;
        };
        let Some(bytes) = roster_bytes
            .checked_mul(2)
            .and_then(|size| size.checked_add(block_bytes))
        else {
            return false;
        };
        let Ok(bytes) = usize::try_from(bytes) else {
            return false;
        };
        if pending.len() >= CAPACITY
            || bytes > BYTE_BUDGET.saturating_sub(pending.values().map(|entry| entry.bytes).sum())
        {
            return false;
        }
        pending.insert(
            block.hash,
            Pending {
                block: block.clone(),
                authority,
                voters: HashSet::new(),
                weight: 0,
                accepted: false,
                created: Instant::now(),
                bytes,
            },
        );
        true
    }

    pub(crate) fn contains(&self, hash: &BlockHash) -> bool {
        self.pending.lock().is_ok_and(|pending| {
            pending
                .get(hash)
                .is_some_and(|entry| entry.accepted && entry.created.elapsed() < RETENTION)
        })
    }

    pub(crate) fn awaiting_votes(&self) -> Vec<BlockHash> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending.retain(|_, entry| entry.created.elapsed() < RETENTION);
        pending
            .iter()
            .filter_map(|(hash, entry)| (!entry.accepted).then_some(*hash))
            .collect()
    }

    pub(crate) async fn receive_and_enqueue(
        &self,
        vote: &WireVote,
        queue: &crate::priority_queue::BlockPriorityQueue,
    ) {
        for block in self.receive(vote) {
            let hash = block.hash;
            if !queue.push(block).await {
                if let Ok(mut pending) = self.pending.lock() {
                    if let Some(entry) = pending.get_mut(&hash) {
                        entry.accepted = false;
                    }
                }
            }
        }
    }

    pub(crate) fn receive(&self, vote: &WireVote) -> Vec<StateBlock> {
        if !vote.is_final
            || self
                .pending
                .lock()
                .map_or(true, |pending| pending.is_empty())
            || !crate::connection_registry::is_vote_signature_valid(vote)
        {
            return Vec::new();
        }
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        let mut ready = Vec::new();
        for hash in &vote.block_hashes {
            let Some(entry) = pending.get_mut(hash) else {
                continue;
            };
            if entry.accepted || entry.created.elapsed() >= RETENTION {
                continue;
            }
            let weight = entry.authority.roster.weight(&vote.voter);
            if weight == 0 {
                continue;
            }
            if entry.voters.insert(vote.voter.clone()) {
                entry.weight += weight;
            }
            let threshold = (u128::from(entry.authority.roster.total_humans())
                * u128::from(entry.authority.quorum_bps))
            .div_ceil(10_000);
            if u128::from(entry.weight) >= threshold {
                entry.accepted = true;
                ready.push(entry.block.clone());
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::checkpoint::{HumanDelegation, HumanRoster};
    use burst_types::{NetworkId, ProtocolParams, Signature};

    fn authority() -> PaymentAuthority {
        PaymentAuthority {
            epoch: BlockHash::new([8; 32]),
            cutoff: 0,
            quorum_bps: 6667,
            roster: HumanRoster::new(
                (0..3)
                    .map(|seed| {
                        let human = burst_crypto::derive_address(
                            &burst_crypto::keypair_from_seed(&[seed; 32]).public,
                        );
                        HumanDelegation {
                            representative: human.clone(),
                            human,
                        }
                    })
                    .collect(),
            )
            .unwrap(),
        }
    }

    fn block() -> StateBlock {
        let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        block.payment_epoch = authority().epoch;
        block.hash = block.compute_hash();
        block
    }

    fn vote(seed: u8, hash: BlockHash, is_final: bool) -> WireVote {
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        let voter = burst_crypto::derive_address(&key.public);
        let bytes = burst_consensus::vote_signing_payload(&voter, is_final, 1000, 1, &[hash]);
        WireVote {
            voter,
            is_final,
            timestamp: 1000,
            sequence: 1,
            block_hashes: vec![hash],
            signature: burst_crypto::sign_message(&bytes, &key.private),
        }
    }

    #[test]
    fn only_exact_final_human_quorum_authorizes_an_age_exception() {
        let cache = std::sync::Arc::new(HistoricalFinality::default());
        let block = block();
        let mut wrong = authority();
        wrong.epoch = BlockHash::new([9; 32]);
        assert!(!cache.request(&block, wrong));
        assert!(cache.receive(&vote(0, block.hash, true)).is_empty());
        assert!(cache.request(&block, authority()));
        assert!(!cache.request(&block, authority()));
        assert!(cache.receive(&vote(0, block.hash, false)).is_empty());
        assert!(cache.receive(&vote(9, block.hash, true)).is_empty());
        let mut forged = vote(0, block.hash, true);
        forged.signature = Signature([0; 64]);
        assert!(cache.receive(&forged).is_empty());
        assert!(cache
            .receive(&vote(0, BlockHash::new([99; 32]), true))
            .is_empty());
        for seed in [0, 0, 1, 1] {
            assert!(cache.receive(&vote(seed, block.hash, true)).is_empty());
            assert!(!cache.contains(&block.hash));
        }
        let ready = cache.receive(&vote(2, block.hash, true));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].hash, block.hash);
        assert!(cache.contains(&block.hash));
        assert!(cache.receive(&vote(2, block.hash, true)).is_empty());
        let mut processor = crate::block_processor::BlockProcessor::new(0, block.payment_epoch);
        processor.historical_finality = cache.clone();
        processor.test_time = Some(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(100)));
        assert!(
            matches!(processor.validate_candidate(&block, &burst_ledger::DagFrontier::new()),
            crate::block_processor::ProcessResult::Rejected(reason) if reason.contains("unsigned"))
        );
        processor.install_payment_epoch(BlockHash::new([9; 32]), 0);
        assert!(
            matches!(processor.validate_candidate(&block, &burst_ledger::DagFrontier::new()),
            crate::block_processor::ProcessResult::Rejected(reason) if reason.contains("epoch"))
        );
        cache
            .pending
            .lock()
            .unwrap()
            .get_mut(&block.hash)
            .unwrap()
            .created = Instant::now() - RETENTION;
        assert!(!cache.contains(&block.hash));
        assert!(cache.request(&block, authority()));
        assert!(!cache.contains(&block.hash));
    }

    #[test]
    fn pending_history_is_bounded_and_never_authorized_by_eviction() {
        let cache = HistoricalFinality::default();
        let mut block = block();
        for index in 0..CAPACITY {
            block.work = index as u64;
            block.hash = BlockHash::new(burst_crypto::blake2b_256(&index.to_be_bytes()));
            assert!(cache.request(&block, authority()));
            assert!(!cache.contains(&block.hash));
        }
        block.hash = BlockHash::new([99; 32]);
        assert!(!cache.request(&block, authority()));
        assert!(!cache.contains(&block.hash));
    }

    #[tokio::test]
    async fn full_admission_queue_retries_without_counting_votes_twice() {
        let cache = HistoricalFinality::default();
        let block = block();
        let queue = crate::priority_queue::BlockPriorityQueue::new(1);
        assert!(queue.push(block.clone()).await);
        assert!(cache.request(&block, authority()));
        for seed in 0..3 {
            cache
                .receive_and_enqueue(&vote(seed, block.hash, true), &queue)
                .await;
        }
        assert!(!cache.contains(&block.hash));
        assert_eq!(cache.awaiting_votes(), vec![block.hash]);
        assert_eq!(cache.collected_weight(&block.hash), Some(3));
        queue.pop().await;
        cache
            .receive_and_enqueue(&vote(0, block.hash, true), &queue)
            .await;
        assert!(cache.contains(&block.hash));
        assert!(cache.awaiting_votes().is_empty());
        assert_eq!(cache.collected_weight(&block.hash), Some(3));
        assert_eq!(queue.pop().await.hash, block.hash);
    }
}
