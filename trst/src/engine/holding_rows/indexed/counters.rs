//! Persistent time-prefix sums. One leaf per distinct nonrevoked effective origin time and
//! one branch per split; compressed radix paths have at most 64 branches.
use std::sync::Arc;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Sum {
    low: u128,
    high: u128,
}
impl Sum {
    fn add(self, other: Self) -> Self {
        let (low, carry) = self.low.overflowing_add(other.low);
        Self {
            low,
            high: self
                .high
                .checked_add(other.high)
                .and_then(|n| n.checked_add(carry as u128))
                .expect("in-memory token count bounds the 256-bit face sum"),
        }
    }
    fn sub(self, amount: u128) -> Self {
        let (low, borrow) = self.low.overflowing_sub(amount);
        Self {
            low,
            high: self
                .high
                .checked_sub(borrow as u128)
                .expect("existing counter amount"),
        }
    }
    fn amount(amount: u128) -> Self {
        Self {
            low: amount,
            high: 0,
        }
    }
}

#[derive(Clone)]
struct Node {
    min: u64,
    max: u64,
    sum: Sum,
    kind: Kind,
}
#[derive(Clone)]
enum Kind {
    Leaf {
        count: usize,
    },
    Branch {
        bit: u32,
        left: Arc<Node>,
        right: Arc<Node>,
    },
}
impl Node {
    fn leaf(key: u64, amount: u128) -> Arc<Self> {
        Arc::new(Self {
            min: key,
            max: key,
            sum: Sum::amount(amount),
            kind: Kind::Leaf { count: 1 },
        })
    }
    fn branch(bit: u32, left: Arc<Self>, right: Arc<Self>) -> Arc<Self> {
        debug_assert!(left.max < right.min);
        Arc::new(Self {
            min: left.min,
            max: right.max,
            sum: left.sum.add(right.sum),
            kind: Kind::Branch { bit, left, right },
        })
    }
    fn insert(root: &Arc<Self>, key: u64, amount: u128) -> Arc<Self> {
        let differing = root.min ^ key;
        let split = (differing != 0).then(|| 63 - differing.leading_zeros());
        let above = match &root.kind {
            Kind::Leaf { .. } => split.is_some(),
            Kind::Branch { bit, .. } => split.is_some_and(|split| split > *bit),
        };
        if above {
            let bit = split.unwrap();
            let leaf = Self::leaf(key, amount);
            return if key & (1u64 << bit) == 0 {
                Self::branch(bit, leaf, root.clone())
            } else {
                Self::branch(bit, root.clone(), leaf)
            };
        }
        match &root.kind {
            Kind::Leaf { count } => Arc::new(Self {
                min: key,
                max: key,
                sum: root.sum.add(Sum::amount(amount)),
                kind: Kind::Leaf {
                    count: count.checked_add(1).expect("in-memory holding count"),
                },
            }),
            Kind::Branch { bit, left, right } => {
                if key & (1u64 << bit) == 0 {
                    Self::branch(*bit, Self::insert(left, key, amount), right.clone())
                } else {
                    Self::branch(*bit, left.clone(), Self::insert(right, key, amount))
                }
            }
        }
    }
    fn remove(root: &Arc<Self>, key: u64, amount: u128) -> Option<Arc<Self>> {
        match &root.kind {
            Kind::Leaf { count } => {
                assert_eq!(root.min, key, "existing counter deadline");
                let sum = root.sum.sub(amount);
                if *count == 1 {
                    assert_eq!(sum, Sum::default());
                    None
                } else {
                    Some(Arc::new(Self {
                        min: key,
                        max: key,
                        sum,
                        kind: Kind::Leaf { count: count - 1 },
                    }))
                }
            }
            Kind::Branch { bit, left, right } => {
                if key & (1u64 << bit) == 0 {
                    Some(match Self::remove(left, key, amount) {
                        Some(next) => Self::branch(*bit, next, right.clone()),
                        None => right.clone(),
                    })
                } else {
                    Some(match Self::remove(right, key, amount) {
                        Some(next) => Self::branch(*bit, left.clone(), next),
                        None => left.clone(),
                    })
                }
            }
        }
    }
    fn prefix(&self, now: u64) -> (Sum, Option<u64>) {
        if now < self.min {
            return (Sum::default(), Some(self.min));
        }
        if now >= self.max {
            return (self.sum, None);
        }
        match &self.kind {
            Kind::Leaf { .. } => unreachable!("leaf covered by range checks"),
            Kind::Branch { left, right, .. } => {
                if now < right.min {
                    let (sum, next) = left.prefix(now);
                    (sum, next.or(Some(right.min)))
                } else {
                    let (sum, next) = right.prefix(now);
                    (left.sum.add(sum), next)
                }
            }
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Counters {
    root: Option<Arc<Node>>,
    revoked: Sum,
}
impl Counters {
    #[cfg(test)]
    pub(super) fn shares_storage_with(&self, other: &Self) -> bool {
        self.revoked == other.revoked
            && match (&self.root, &other.root) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
    pub(super) fn insert(&mut self, origin: u64, amount: u128, revoked: bool) {
        if revoked {
            self.revoked = self.revoked.add(Sum::amount(amount));
        } else {
            self.root = Some(match &self.root {
                Some(root) => Node::insert(root, origin, amount),
                None => Node::leaf(origin, amount),
            });
        }
    }
    pub(super) fn remove(&mut self, origin: u64, amount: u128, revoked: bool) {
        if revoked {
            self.revoked = self.revoked.sub(amount);
        } else {
            self.root = Node::remove(
                self.root.as_ref().expect("existing counter root"),
                origin,
                amount,
            );
        }
    }
    /// Return expired/revoked sums and the next deadline under this period.
    /// Translate time to an origin-time prefix without rebuilding the tree.
    /// At u64::MAX every saturating deadline has been reached.
    pub(super) fn period_window(
        &self,
        now: u64,
        period: u64,
    ) -> Option<((u128, u128), Option<u64>)> {
        let threshold = if now == u64::MAX {
            Some(u64::MAX)
        } else {
            now.checked_sub(period)
        };
        let (totals, next) = match threshold {
            Some(at) => self.window(at)?,
            None => {
                if self.revoked.high != 0 {
                    return None;
                }
                (
                    (0, self.revoked.low),
                    self.root.as_ref().map(|root| root.min),
                )
            }
        };
        Some((totals, next.map(|origin| origin.saturating_add(period))))
    }

    /// Return None when either total exceeds u128. The caller then uses its
    /// ordered token scan to report the first checked-sum error. Wide internal
    /// sums allow later removals to bring a total back into range.
    pub(super) fn window(&self, now: u64) -> Option<((u128, u128), Option<u64>)> {
        let (expired, next) = self
            .root
            .as_ref()
            .map_or((Sum::default(), None), |root| root.prefix(now));
        (expired.high == 0 && self.revoked.high == 0)
            .then_some(((expired.low, self.revoked.low), next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn period_windows_match_saturating_deadlines_without_rebuilding() {
        let mut index = Counters::default();
        let mut entries = Vec::new();
        for n in 0..128u64 {
            let origin = n.wrapping_mul(0x9e3779b97f4a7c15);
            for amount in [0, n as u128] {
                index.insert(origin, amount, false);
                entries.push((origin, amount));
            }
        }
        index.insert(u64::MAX, 0, false);
        entries.push((u64::MAX, 0));
        index.insert(0, 7, true);
        let retained = index.clone();
        for period in [0, 1, 100, u64::MAX / 2, u64::MAX - 1, u64::MAX] {
            for now in (0..128u64)
                .map(|n| n.wrapping_mul(0xd1342543de82ef95))
                .chain([0, 1, 99, 100, u64::MAX - 1, u64::MAX])
            {
                let expired = entries
                    .iter()
                    .filter(|(origin, _)| origin.saturating_add(period) <= now)
                    .map(|(_, amount)| amount)
                    .sum();
                let next = entries
                    .iter()
                    .map(|(origin, _)| origin.saturating_add(period))
                    .filter(|deadline| *deadline > now)
                    .min();
                assert_eq!(index.period_window(now, period), Some(((expired, 7), next)));
            }
        }
        assert!(index.shares_storage_with(&retained));
        let mut wide = Counters::default();
        wide.insert(0, u128::MAX, false);
        wide.insert(1, 1, false);
        assert_eq!(wide.period_window(0, 1), Some(((0, 0), Some(1))));
        assert_eq!(wide.period_window(1, 1), Some(((u128::MAX, 0), Some(2))));
        assert_eq!(wide.period_window(2, 1), None);
        assert_eq!(
            wide.period_window(u64::MAX - 1, u64::MAX),
            Some(((0, 0), Some(u64::MAX)))
        );
        assert_eq!(wide.period_window(u64::MAX, u64::MAX), None);
        wide.insert(0, u128::MAX, true);
        wide.insert(1, 1, true);
        assert_eq!(wide.period_window(0, 1), None);
    }

    #[test]
    fn prefix_matches_scan_across_insert_remove_ties_and_extreme_times() {
        let mut index = Counters::default();
        let mut entries = Vec::new();
        for n in 0..256u64 {
            let key = n.wrapping_mul(0x9e3779b97f4a7c15);
            for (at, amount) in [(key, n as u128), (key, 0), (0, 1), (u64::MAX, 2)] {
                index.insert(at, amount, false);
                entries.push((at, amount));
            }
        }
        let retained = index.clone();
        for pass in 0..2 {
            for now in (0..256u64)
                .map(|n| n.wrapping_mul(0xd1342543de82ef95))
                .chain([0, u64::MAX])
            {
                let sum = entries
                    .iter()
                    .filter(|(at, _)| *at <= now)
                    .map(|(_, amount)| amount)
                    .sum();
                let next = entries
                    .iter()
                    .filter(|(at, _)| *at > now)
                    .map(|(at, _)| *at)
                    .min();
                assert_eq!(index.window(now), Some(((sum, 0), next)));
            }
            if pass == 0 {
                let removed: Vec<_> = entries.drain(..512).collect();
                for (at, amount) in removed {
                    index.remove(at, amount, false);
                }
            }
        }
        assert_ne!(index.window(u64::MAX), retained.window(u64::MAX));
        for (at, amount) in entries {
            index.remove(at, amount, false);
        }
        assert!(index.root.is_none());
        assert_eq!(index.window(0), Some(((0, 0), None)));
    }
    #[test]
    fn wide_totals_recover_after_overflow_and_zero_deadlines_remain() {
        let mut index = Counters::default();
        index.insert(10, u128::MAX, false);
        index.insert(20, 1, false);
        assert_eq!(index.window(10), Some(((u128::MAX, 0), Some(20))));
        assert_eq!(index.window(20), None);
        index.remove(10, u128::MAX, false);
        index.insert(30, 0, false);
        assert_eq!(index.window(20), Some(((1, 0), Some(30))));
        index.insert(0, u128::MAX, true);
        index.insert(0, 1, true);
        assert_eq!(index.window(0), None);
        index.remove(0, 1, true);
        assert_eq!(index.window(0), Some(((0, u128::MAX), Some(20))));
    }
}

#[cfg(test)]
mod footprint_tests {
    use super::*;

    fn shape(node: &Node) -> (usize, usize) {
        match &node.kind {
            Kind::Leaf { .. } => (1, 1),
            Kind::Branch { left, right, .. } => {
                let (left_nodes, left_depth) = shape(left);
                let (right_nodes, right_depth) = shape(right);
                (
                    1 + left_nodes + right_nodes,
                    1 + left_depth.max(right_depth),
                )
            }
        }
    }

    fn footprint(count: usize) {
        let mut origin = Counters::default();
        let mut saturated = Counters::default();
        let mut distinct = 0usize;
        for n in 0..count {
            let at = 100 + n as u64;
            let amount = u128::from(n % 7 != 0);
            let revoked = n % 5 == 0;
            distinct += usize::from(!revoked);
            origin.insert(at, amount, revoked);
            // Compare origin-time keys with deadlines saturated to one timestamp.
            saturated.insert(at.saturating_add(u64::MAX), amount, revoked);
        }
        for now in [0, 1, u64::MAX - 1, u64::MAX] {
            assert_eq!(origin.period_window(now, u64::MAX), saturated.window(now));
        }
        let retained = origin.clone();
        assert!(retained.shares_storage_with(&origin));
        let (origin_nodes, depth) = origin.root.as_ref().map_or((0, 0), |node| shape(node));
        let (saturated_nodes, _) = saturated.root.as_ref().map_or((0, 0), |node| shape(node));
        assert_eq!(origin_nodes, distinct.saturating_mul(2).saturating_sub(1));
        assert_eq!(saturated_nodes, usize::from(distinct != 0));
        assert!(depth <= 65);
        let node_bytes = std::mem::size_of::<Node>();
        println!("counter_footprint {{\"holdings\":{count},\"distinct\":{distinct},\"origin_nodes\":{origin_nodes},\"saturated_nodes\":{saturated_nodes},\"depth\":{depth},\"node_value_bytes\":{node_bytes},\"origin_value_bytes\":{},\"saturated_value_bytes\":{}}}", origin_nodes * node_bytes, saturated_nodes * node_bytes);
    }

    #[test]
    fn saturated_counter_footprint_preserves_queries_and_shared_snapshots() {
        for count in [0, 1, 16, 1000] {
            footprint(count);
        }
    }

    #[test]
    #[ignore = "explicit capped-host structural counter footprint; excludes allocator and full-node memory"]
    fn saturated_counter_footprint_scaling() {
        let count = std::env::var("BURST_COUNTER_FOOTPRINT_COUNT")
            .unwrap()
            .parse()
            .unwrap();
        assert!([1000, 100000].contains(&count));
        footprint(count);
    }
}
