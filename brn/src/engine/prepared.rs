//! Merge prepared wallet changes into a copy of the current BRN engine after
//! checking captured dependencies. These helpers neither publish nor persist it.

use super::BrnEngine;
use burst_types::WalletAddress;

impl BrnEngine {
    /// Return a candidate combining prepared changes with unrelated current wallets.
    /// Callers must include every wallet read, even when it was not modified.
    /// Ledger, authority and TRST dependencies require separate validation.
    /// Rate changes and stake allocation remain global barriers.
    pub fn rebase_wallet_changes(
        &self,
        before: &Self,
        after: &Self,
        read_wallets: &[WalletAddress],
    ) -> Result<Self, String> {
        self.rebase_wallet_changes_inner(before, after, read_wallets, 0)
    }

    /// Return a candidate containing exactly one prepared stake allocation.
    /// Require the captured counter to match; competing allocations conflict.
    /// Preserve the prepared ID. The caller still validates the operation and
    /// supplies every read dependency, as with `rebase_wallet_changes`.
    pub fn rebase_wallet_stake(
        &self, before: &Self, after: &Self, read_wallets: &[WalletAddress],
    ) -> Result<Self, String> {
        self.rebase_wallet_changes_inner(before, after, read_wallets, 1)
    }

    fn rebase_wallet_changes_inner(
        &self, before: &Self, after: &Self, read_wallets: &[WalletAddress], allocated: u64,
    ) -> Result<Self, String> {
        let next_stake_id = before.next_stake_id.checked_add(allocated)
            .ok_or("prepared stake allocator overflow")?;
        if self.rate_history != before.rate_history || after.rate_history != before.rate_history
            || self.next_stake_id != before.next_stake_id
            || after.next_stake_id != next_stake_id
        {
            return Err("prepared BRN global context changed".into());
        }
        for address in read_wallets {
            if self.wallets.get(address) != before.wallets.get(address) {
                return Err("prepared BRN wallet dependency changed".into());
            }
        }
        let mut rebased = self.clone();
        rebased.next_stake_id = next_stake_id;
        after.wallets.visit_changes(&before.wallets, |address, old, new| {
            if self.wallets.get(address) != old {
                return Err("prepared BRN write conflicts with current wallet".into());
            }
            match new {
                Some(wallet) => { rebased.wallets.insert(address.clone(), wallet.clone()); }
                None => { rebased.wallets.remove(address); }
            }
            Ok(())
        })?;
        Ok(rebased)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrnWalletState;
    use burst_types::Timestamp;

    #[test]
    fn prepared_stake_preserves_id_and_conflicts_with_competing_allocations() {
        let a = WalletAddress::new("brst_staker");
        let b = WalletAddress::new("brst_other");
        let mut before = BrnEngine::with_rate(10, Timestamp::EPOCH);
        for address in [&a, &b] { before.track_wallet(address.clone(), BrnWalletState::new(Timestamp::EPOCH)); }
        let mut after = before.clone();
        let mut wallet = after.wallets.remove(&a).unwrap();
        let stake = after.stake(&a, &mut wallet, 5, crate::StakeKind::Verification {
            target_wallet: b.clone(),
        }, Timestamp::new(10)).unwrap();
        after.wallets.insert(a.clone(), wallet);
        let mut current = before.clone();
        current.get_wallet_mut(&b).unwrap().total_burned = 3;
        let result = current.rebase_wallet_stake(&before, &after, &[a.clone()]).unwrap();
        assert_eq!(stake.id, before.next_stake_id);
        assert_eq!(result.next_stake_id, stake.id + 1);
        assert_eq!(result.get_wallet(&a).unwrap().total_staked, 5);
        assert_eq!(result.get_wallet(&b).unwrap().total_burned, 3);
        assert!(current.rebase_wallet_changes(&before, &after, &[a.clone()]).is_err());
        current.next_stake_id += 1;
        assert!(current.rebase_wallet_stake(&before, &after, &[a.clone()]).is_err());
        assert!(before.rebase_wallet_stake(&before, &before, &[a.clone()]).is_err());
        let mut too_many = after.clone();
        too_many.next_stake_id += 1;
        assert!(before.rebase_wallet_stake(&before, &too_many, &[a.clone()]).is_err());
        let mut changed_wallet = before.clone();
        changed_wallet.get_wallet_mut(&a).unwrap().total_burned = 1;
        assert!(changed_wallet.rebase_wallet_stake(&before, &after, &[a]).is_err());
        let mut changed_rate = before.clone();
        changed_rate.apply_rate_change(20, Timestamp::new(5)).unwrap();
        assert!(changed_rate.rebase_wallet_stake(&before, &after, &[]).is_err());
        let mut exhausted = before.clone();
        exhausted.next_stake_id = u64::MAX;
        assert!(exhausted.rebase_wallet_stake(&exhausted, &exhausted, &[]).is_err());
        assert_eq!(before.get_wallet(&WalletAddress::new("brst_staker")).unwrap().total_staked, 0);
    }

    #[test]
    fn disjoint_changes_preserve_other_commits_and_read_dependencies_conflict() {
        let a = WalletAddress::new("brst_a");
        let b = WalletAddress::new("brst_b");
        let mut base = BrnEngine::with_rate(10, Timestamp::EPOCH);
        for address in [&a, &b] {
            base.track_wallet(address.clone(), BrnWalletState::new(Timestamp::EPOCH));
        }
        let mut current = base.clone();
        current.get_wallet_mut(&a).unwrap().total_burned = 3;
        let mut prepared = base.clone();
        prepared.get_wallet_mut(&b).unwrap().total_burned = 7;
        let result = current.rebase_wallet_changes(&base, &prepared, &[b.clone()]).unwrap();
        assert_eq!(result.get_wallet(&a).unwrap().total_burned, 3);
        assert_eq!(result.get_wallet(&b).unwrap().total_burned, 7);
        assert!(std::ptr::eq(result.get_wallet(&a).unwrap(), current.get_wallet(&a).unwrap()));
        assert_eq!(base.get_wallet(&a).unwrap().total_burned, 0);
        assert_eq!(current.get_wallet(&b).unwrap().total_burned, 0);
        assert!(current.rebase_wallet_changes(&base, &prepared, &[a.clone()]).is_err());
        prepared.wallets.remove(&a);
        assert!(current.rebase_wallet_changes(&base, &prepared, &[]).is_err());
        assert!(current.get_wallet(&a).is_some());
        let removed = base.rebase_wallet_changes(&base, &prepared, &[]).unwrap();
        assert!(removed.get_wallet(&a).is_none());
        assert!(base.rebase_wallet_changes(&removed, &base, &[]).is_err());
        assert_eq!(removed.rebase_wallet_changes(&removed, &base, &[]).unwrap().wallets, base.wallets);
        for candidate_changed in [false, true] {
            let mut changed = base.clone();
            changed.apply_rate_change(20, Timestamp::new(10)).unwrap();
            assert!(if candidate_changed { base.rebase_wallet_changes(&base, &changed, &[]) }
                else { changed.rebase_wallet_changes(&base, &base, &[]) }.is_err());
            changed = base.clone();
            changed.next_stake_id += 1;
            assert!(if candidate_changed { base.rebase_wallet_changes(&base, &changed, &[]) }
                else { changed.rebase_wallet_changes(&base, &base, &[]) }.is_err());
        }
    }
}
