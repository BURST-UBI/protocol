use super::*;
use std::collections::{HashMap, HashSet};

impl SnapshotContinuation {
    /// Select a dependency-closed cut from a bounded, proof-checked confirmed
    /// suffix. This does not replay economics or authorize a signature.
    pub fn cemented_heads_at(
        &self,
        cemented: &super::super::tail::cemented::CementedTail,
        cutoff: u64,
    ) -> Result<Vec<HistoryHead>, ReplayError> {
        self.replayed_heads_at(cemented.tail(), cutoff)
    }

    pub(super) fn replayed_heads_at(
        &self, tail: &SnapshotTail, cutoff: u64,
    ) -> Result<Vec<HistoryHead>, ReplayError> {
        if tail.context() != self.context || cutoff < self.anchor.cutoff() || cutoff > tail.cutoff()
        {
            return Err(ReplayError::Invalid(
                "confirmed cut is outside its authenticated base".into(),
            ));
        }
        let mut heads: HashMap<_, _> = self
            .state
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
            .map(|account| {
                (
                    account.address.clone(),
                    HistoryHead {
                        account: account.address.clone(),
                        height: account.block_count,
                        hash: account.head,
                    },
                )
            })
            .collect();
        let mut included: HashSet<_> = heads.values().map(|head| head.hash).collect();
        included.extend(tail.retained().hashes());
        // The finished tail is in causal order. Excluding a future dependency
        // also excludes its descendants, even when their timestamps are older.
        for block in tail.blocks() {
            let block = block.map_err(|error| ReplayError::Invalid(error.to_string()))?;
            if block.timestamp.as_secs() > cutoff
                || block.dependencies().any(|hash| !included.contains(&hash))
            {
                continue;
            }
            let (height, previous) = heads
                .get(&block.account)
                .map_or((0, BlockHash::ZERO), |head| (head.height, head.hash));
            if block.previous != previous {
                return Err(ReplayError::Invalid(
                    "confirmed cut does not extend its account head".into(),
                ));
            }
            let height = height
                .checked_add(1)
                .ok_or_else(|| ReplayError::Invalid("confirmed cut height overflow".into()))?;
            heads.insert(
                block.account.clone(),
                HistoryHead {
                    account: block.account.clone(),
                    height,
                    hash: block.hash,
                },
            );
            included.insert(block.hash);
        }
        let mut heads: Vec<_> = heads.into_values().collect();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        Ok(heads)
    }
}
