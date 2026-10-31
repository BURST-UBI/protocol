//! One configured proof-of-work threshold for every block kind.

/// Callers retain block-kind classification without changing consensus work.
/// This type avoids a dependency cycle with the ledger crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkBlockKind {
    Base,
    ReceiveOrOpen,
    Epoch,
}

const BASE_THRESHOLD: u64 = 0xffff_fff8_0000_0000;

/// One threshold for all block types. Network configuration supplies the base.
pub struct WorkThresholds {
    pub base: u64,
}

impl WorkThresholds {
    pub fn new() -> Self {
        Self::with_base(BASE_THRESHOLD)
    }

    pub fn with_base(base: u64) -> Self {
        Self { base }
    }

    pub fn threshold_for(&self, _kind: WorkBlockKind) -> u64 {
        self.base
    }
}

impl Default for WorkThresholds {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_block_kinds_use_the_exact_configured_threshold() {
        for base in [0, 1, 1000, BASE_THRESHOLD, u64::MAX] {
            let thresholds = WorkThresholds::with_base(base);
            for kind in [
                WorkBlockKind::Base,
                WorkBlockKind::ReceiveOrOpen,
                WorkBlockKind::Epoch,
            ] {
                assert_eq!(thresholds.threshold_for(kind), base);
            }
        }
    }

    #[test]
    fn default_matches_decision_14_2() {
        assert_eq!(WorkThresholds::default().base, 0xffff_fff8_0000_0000);
    }
}
