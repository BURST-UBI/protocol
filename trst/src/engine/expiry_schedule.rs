use super::{Shared, SharedMap, Timestamp, WalletAddress, WalletPortfolio};
use crate::SharedSet;

type Wallets = SharedMap<WalletAddress, Shared<WalletPortfolio>>;

/// Derived from portfolio deadlines; never serialized or used as balance authority.
/// Keeping a shared prior root discovers mutations made through any wallet API.
#[derive(Clone, Default)]
pub(super) struct ExpirySchedule {
    baseline: Wallets,
    deadlines: SharedSet<(Timestamp, WalletAddress)>,
}

impl ExpirySchedule {
    /// Refresh deadlines for changed portfolios and retain this wallet snapshot.
    /// Returns the number of changed wallets, not the number already due. Cached
    /// deadlines are trusted here; this does not validate or expire holdings.
    pub(super) fn synchronize(&mut self, wallets: &Wallets) -> usize {
        let mut changed = 0;
        for owner in wallets.changed_keys(&self.baseline) {
            changed += 1;
            if let Some(at) = self.baseline.get(owner).and_then(|p| p.cached_expiry()) {
                self.deadlines.remove(&(at, owner.clone()));
            }
            if let Some(at) = wallets.get(owner).and_then(|p| p.cached_expiry()) {
                self.deadlines.insert((at, owner.clone()));
            }
        }
        self.baseline = wallets.clone();
        changed
    }

    /// Collect deadlines at or before now, ordered by time then wallet. Entries
    /// remain scheduled until synchronize observes an updated portfolio.
    pub(super) fn due(&self, now: Timestamp) -> Vec<WalletAddress> {
        self.deadlines
            .iter()
            .take_while(|(at, _)| *at <= now)
            .map(|(_, owner)| owner.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_tracks_only_changes_and_orders_shared_deadlines() {
        let mut wallets = Wallets::default();
        for n in 0..4096 {
            wallets.insert(
                WalletAddress::new(format!("brst_{n:060}")),
                Shared::from(WalletPortfolio::from_parts(Vec::new(), 0, Some(Timestamp::new(100 + n)))),
            );
        }
        let mut schedule = ExpirySchedule::default();
        assert_eq!(schedule.synchronize(&wallets), 4096);
        assert_eq!(schedule.synchronize(&wallets), 0);
        assert!(schedule.due(Timestamp::new(99)).is_empty());
        let first = WalletAddress::new(format!("brst_{:060}", 0));
        let second = WalletAddress::new(format!("brst_{:060}", 1));
        assert_eq!(schedule.due(Timestamp::new(100)), vec![first.clone()]);
        let old = schedule.clone();
        wallets.get_mut(&second).unwrap().earliest_expiry = Some(Timestamp::new(100));
        assert_eq!(schedule.synchronize(&wallets), 1);
        assert_eq!(
            schedule.due(Timestamp::new(100)),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(old.due(Timestamp::new(100)), vec![first.clone()]);
        wallets.remove(&first);
        wallets.get_mut(&second).unwrap().earliest_expiry = None;
        assert_eq!(schedule.synchronize(&wallets), 2);
        assert!(schedule.due(Timestamp::new(100)).is_empty());
        wallets.clear();
        schedule.synchronize(&wallets);
        assert!(schedule.due(Timestamp::new(u64::MAX)).is_empty());
    }
}
