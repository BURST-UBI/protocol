//! Returned stakes remain unavailable for spending until the account acknowledges
//! complete outcome evidence. These helpers track that hold; they do not verify
//! certificates, signatures or the settlement that returned the stake.

use std::collections::BTreeSet;

use burst_types::{BlockHash, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{BrnError, BrnWalletState, RateHistory};

mod pending;
pub use pending::PendingReleases;

/// Returned amount held pending acknowledgment, keyed by outcome in the wallet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StakeRelease {
    pub amount: u128,
    /// Complete dependency set, including the outcome. Empty means no proof is
    /// available for acknowledgment; the amount still reduces spendable BRN.
    pub sources: Vec<BlockHash>,
    pub completed_at: Timestamp,
}

impl BrnWalletState {
    /// Hold a returned amount without claimable evidence. This does not settle
    /// the stake or change burned/staked totals. Duplicate outcomes are rejected.
    pub fn record_unproven_release(
        &mut self,
        outcome: BlockHash,
        amount: u128,
        completed_at: Timestamp,
    ) -> Result<(), BrnError> {
        if outcome.is_zero() || amount == 0 || self.pending_releases.contains_key(&outcome) {
            return Err(BrnError::Other(
                "invalid or duplicate unproven stake release".into(),
            ));
        }
        self.pending_releases.insert(
            outcome,
            StakeRelease {
                amount,
                sources: Vec::new(),
                completed_at,
            },
        );
        Ok(())
    }

    /// Add returned BRN under an outcome and its complete dependency set.
    /// Matching repeated calls add amounts, so callers must prevent duplicate
    /// settlement. Source hashes are normalized and checked, not authenticated.
    pub fn record_release(
        &mut self,
        outcome: BlockHash,
        amount: u128,
        sources: &[BlockHash],
        completed_at: Timestamp,
    ) -> Result<(), BrnError> {
        let mut sources = sources.to_vec();
        sources.sort_unstable();
        if amount == 0
            || outcome.is_zero()
            || !sources.contains(&outcome)
            || sources.iter().any(BlockHash::is_zero)
            || sources.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(BrnError::Other("invalid returned-stake evidence".into()));
        }
        if let Some(release) = self.pending_releases.get(&outcome) {
            if release.sources != sources || release.completed_at != completed_at {
                return Err(BrnError::Other(
                    "conflicting returned-stake evidence".into(),
                ));
            }
            let amount = release
                .amount
                .checked_add(amount)
                .ok_or(BrnError::Overflow)?;
            self.pending_releases.insert(
                outcome,
                StakeRelease {
                    amount,
                    sources,
                    completed_at,
                },
            );
        } else {
            self.pending_releases.insert(
                outcome,
                StakeRelease {
                    amount,
                    sources,
                    completed_at,
                },
            );
        }
        Ok(())
    }

    /// Greedily select complete proofs in outcome-key order, returning sorted
    /// unique hashes up to `limit`. Skip proofs that do not fit; this does not
    /// maximize the number of released stakes. Empty proofs are not selectable.
    pub fn release_sources(&self, limit: usize) -> Vec<BlockHash> {
        select_release_sources(self.pending_releases.selectable_values(), limit)
    }

    /// Whether any nonempty proof has a source absent from the sorted selection.
    /// False does not imply the absence of held amounts with empty proofs.
    pub fn has_more_releases(&self, selected: &[BlockHash]) -> bool {
        self.pending_releases.has_more_sources(selected)
    }

    /// Remove holds fully covered by sorted, unique, nonzero source hashes.
    /// Reject early, partial or unrelated evidence before changing the wallet.
    /// The caller authenticates the signed acknowledgment and dependencies.
    /// This only removes pending rows; it does not credit BRN a second time.
    pub fn acknowledge_releases(
        &mut self,
        sources: &[BlockHash],
        at: Timestamp,
    ) -> Result<(), BrnError> {
        if sources.is_empty() {
            return Ok(());
        }
        if sources.iter().any(BlockHash::is_zero)
            || sources.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(BrnError::Other(
                "noncanonical returned-stake evidence".into(),
            ));
        }
        let supplied: BTreeSet<_> = sources.iter().copied().collect();
        let mut covered = BTreeSet::new();
        let mut outcomes = Vec::new();
        // Every valid complete proof names its outcome, which is already a map key.
        // Raw malformed rows without that invariant retain the legacy scan scope.
        let candidates: BTreeSet<_> = sources
            .iter()
            .copied()
            .chain(self.pending_releases.unindexed_outcomes())
            .collect();
        for outcome in candidates {
            let Some(release) = self.pending_releases.get(&outcome) else {
                continue;
            };
            if !release.sources.is_empty()
                && release.sources.iter().all(|hash| supplied.contains(hash))
            {
                if at < release.completed_at {
                    return Err(BrnError::Other("stake release predates its outcome".into()));
                }
                covered.extend(release.sources.iter().copied());
                outcomes.push(outcome);
            }
        }
        if covered != supplied {
            return Err(BrnError::Other(
                "missing, partial or already consumed stake release".into(),
            ));
        }
        for outcome in outcomes {
            self.pending_releases.remove(&outcome);
        }
        Ok(())
    }

    /// Subtract all unacknowledged returns, including those without proofs, from
    /// available BRN. Return None on arithmetic failure. This keeps returned
    /// stakes unavailable until the account records their release dependencies.
    pub fn spendable_balance_checked(&self, rates: &RateHistory, now: Timestamp) -> Option<u128> {
        let unacknowledged = self.pending_releases.total_checked()?;
        self.available_balance_checked(rates, now)?
            .checked_sub(unacknowledged)
    }
}

fn select_release_sources<'a>(
    mut releases: impl Iterator<Item = &'a StakeRelease>,
    limit: usize,
) -> Vec<BlockHash> {
    let mut selected = BTreeSet::new();
    while selected.len() < limit {
        let Some(release) = releases.next() else {
            break;
        };
        let remaining = limit - selected.len();
        // Preserve whole-proof greedy selection. Once this proof exceeds the
        // remaining space, its unseen sources cannot make it fit again.
        let extra = release
            .sources
            .iter()
            .filter(|hash| !selected.contains(*hash))
            .take(remaining.saturating_add(1))
            .count();
        if extra <= remaining {
            selected.extend(release.sources.iter().copied());
        }
    }
    selected.into_iter().collect()
}

#[cfg(test)]
mod indexed_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(seed: u8) -> BlockHash {
        BlockHash::new([seed; 32])
    }

    fn returned() -> BrnWalletState {
        let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
        wallet.total_burned = 10;
        wallet
            .record_release(hash(3), 30, &[hash(3), hash(1), hash(2)], Timestamp::new(9))
            .unwrap();
        wallet
    }

    #[test]
    fn unlocked_remaining_brn_is_spendable_without_releasing_other_stakes() {
        let rates = RateHistory::new(10, Timestamp::EPOCH);
        let mut before = BrnWalletState::new(Timestamp::EPOCH);
        before.total_staked = 40;
        let mut after = returned();
        assert_eq!(
            before.spendable_balance_checked(&rates, Timestamp::new(10)),
            Some(60)
        );
        assert_eq!(
            after.available_balance_checked(&rates, Timestamp::new(10)),
            Some(90)
        );
        assert_eq!(
            after.spendable_balance_checked(&rates, Timestamp::new(10)),
            Some(60)
        );
        after
            .acknowledge_releases(&[hash(1), hash(2), hash(3)], Timestamp::new(10))
            .unwrap();
        assert_eq!(
            after.spendable_balance_checked(&rates, Timestamp::new(10)),
            Some(90)
        );
    }

    #[test]
    fn release_rejects_partial_unrelated_duplicate_early_and_reused_proofs_atomically() {
        let mut wallet = returned();
        let before = bincode::serialize(&wallet).unwrap();
        for sources in [
            vec![hash(3)],
            vec![hash(1), hash(2)],
            vec![hash(1), hash(2), hash(3), hash(4)],
            vec![hash(1), hash(1), hash(3)],
            vec![hash(3), hash(2), hash(1)],
        ] {
            assert!(wallet
                .acknowledge_releases(&sources, Timestamp::new(10))
                .is_err());
            assert_eq!(bincode::serialize(&wallet).unwrap(), before);
        }
        let sources = [hash(1), hash(2), hash(3)];
        assert!(wallet
            .acknowledge_releases(&sources, Timestamp::new(8))
            .is_err());
        assert_eq!(bincode::serialize(&wallet).unwrap(), before);
        assert!(BrnWalletState::new(Timestamp::EPOCH)
            .acknowledge_releases(&sources, Timestamp::new(10))
            .is_err());
        wallet
            .acknowledge_releases(&sources, Timestamp::new(9))
            .unwrap();
        assert!(wallet
            .acknowledge_releases(&sources, Timestamp::new(10))
            .is_err());
    }

    #[test]
    fn proofs_survive_restart_and_are_never_split_to_fit_a_block() {
        let mut wallet = returned();
        wallet
            .record_release(hash(5), 20, &[hash(4), hash(5)], Timestamp::new(10))
            .unwrap();
        let bytes = bincode::serialize(&wallet).unwrap();
        let mut restored = BrnWalletState::from_persisted_bytes(&bytes).unwrap();
        assert!(restored.release_sources(1).is_empty());
        assert_eq!(restored.release_sources(2), vec![hash(4), hash(5)]);
        assert_eq!(restored.release_sources(4), vec![hash(1), hash(2), hash(3)]);
        assert!(restored.has_more_releases(&restored.release_sources(4)));
        assert!(!restored.has_more_releases(&restored.release_sources(5)));
        let first = restored.release_sources(4);
        restored
            .acknowledge_releases(&first, Timestamp::new(10))
            .unwrap();
        assert_eq!(restored.release_sources(4), vec![hash(4), hash(5)]);
        assert_eq!(restored.pending_releases[&hash(5)].amount, 20);
    }

    #[test]
    fn unproven_deadlines_cannot_be_claimed_through_another_panels_proof() {
        let mut wallet = returned();
        wallet
            .record_unproven_release(hash(9), 40, Timestamp::new(10))
            .unwrap();
        let sources = wallet.release_sources(10);
        wallet
            .acknowledge_releases(&sources, Timestamp::new(11))
            .unwrap();
        assert!(wallet.release_sources(10).is_empty());
        assert!(!wallet.has_more_releases(&[]));
        assert_eq!(wallet.pending_releases[&hash(9)].amount, 40);
        assert!(wallet
            .acknowledge_releases(&[hash(9)], Timestamp::new(11))
            .is_err());
        assert_eq!(
            wallet.spendable_balance_checked(
                &RateHistory::new(10, Timestamp::EPOCH),
                Timestamp::new(11)
            ),
            Some(60)
        );
    }
}
