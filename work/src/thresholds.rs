//! Block-type-aware PoW difficulty thresholds.
//!
//! Different block types require different proof-of-work levels:
//! - Receive/Open blocks need HIGHER difficulty (anti-spam for free operations)
//! - Send blocks need BASE difficulty (sender already proved ownership)
//! - Epoch blocks require very high difficulty (only genesis can create)

/// Simplified block kind for PoW threshold selection.
///
/// Avoids a dependency on `burst-ledger::BlockType` (which depends on
/// `burst-work`, creating a cycle). Call sites map from `BlockType` to
/// this enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkBlockKind {
    /// Send, Burn, Split, Merge, governance, delegation, etc.
    Base,
    /// Receive or Open — higher difficulty to deter spam.
    ReceiveOrOpen,
    /// Epoch — very high difficulty, only genesis can produce.
    Epoch,
}

const BASE_THRESHOLD: u64 = 0xFFFFFE00_00000000;
const RECEIVE_MULTIPLIER: f64 = 8.0;
const EPOCH_MULTIPLIER: f64 = 64.0;

/// Per-block-type PoW thresholds.
///
/// Higher threshold values = harder work required.  The `multiply` helper
/// scales difficulty by shrinking the "inverse gap" (`u64::MAX - threshold`)
/// which raises the bar the work nonce must clear.
pub struct WorkThresholds {
    pub base: u64,
    pub receive_multiplier: f64,
    pub epoch_multiplier: f64,
}

impl WorkThresholds {
    pub fn new() -> Self {
        Self {
            base: BASE_THRESHOLD,
            receive_multiplier: RECEIVE_MULTIPLIER,
            epoch_multiplier: EPOCH_MULTIPLIER,
        }
    }

    /// Construct with a custom base (useful in tests or low-difficulty devnets).
    pub fn with_base(base: u64) -> Self {
        Self {
            base,
            receive_multiplier: RECEIVE_MULTIPLIER,
            epoch_multiplier: EPOCH_MULTIPLIER,
        }
    }

    /// Get the required work difficulty for a specific block kind.
    pub fn threshold_for(&self, kind: WorkBlockKind) -> u64 {
        match kind {
            WorkBlockKind::ReceiveOrOpen => self.multiply(self.base, self.receive_multiplier),
            WorkBlockKind::Epoch => self.multiply(self.base, self.epoch_multiplier),
            WorkBlockKind::Base => self.base,
        }
    }

    /// Scale difficulty: higher threshold = harder work.
    ///
    /// The "difficulty inverse" is `u64::MAX - threshold`. Dividing that by the
    /// multiplier shrinks the gap, raising the threshold.  When `base` is 0
    /// (PoW disabled), all derived thresholds are also 0.
    fn multiply(&self, base: u64, multiplier: f64) -> u64 {
        if base == 0 {
            return 0;
        }
        let difficulty_inv = u64::MAX - base;
        let scaled_inv = (difficulty_inv as f64 / multiplier) as u64;
        u64::MAX - scaled_inv
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
    fn receive_harder_than_send() {
        let thresholds = WorkThresholds::new();
        let send = thresholds.threshold_for(WorkBlockKind::Base);
        let receive = thresholds.threshold_for(WorkBlockKind::ReceiveOrOpen);
        assert!(receive > send, "receive threshold ({receive}) must exceed send ({send})");
    }

    #[test]
    fn epoch_hardest() {
        let thresholds = WorkThresholds::new();
        let epoch = thresholds.threshold_for(WorkBlockKind::Epoch);
        let receive = thresholds.threshold_for(WorkBlockKind::ReceiveOrOpen);
        assert!(epoch > receive, "epoch threshold ({epoch}) must exceed receive ({receive})");
    }

    #[test]
    fn base_is_unchanged() {
        let thresholds = WorkThresholds::new();
        assert_eq!(thresholds.threshold_for(WorkBlockKind::Base), BASE_THRESHOLD);
    }

    #[test]
    fn custom_base_propagates() {
        let thresholds = WorkThresholds::with_base(1000);
        assert_eq!(thresholds.threshold_for(WorkBlockKind::Base), 1000);
        let recv = thresholds.threshold_for(WorkBlockKind::ReceiveOrOpen);
        assert!(recv > 1000);
    }

    #[test]
    fn zero_base_stays_zero_for_base() {
        let thresholds = WorkThresholds::with_base(0);
        assert_eq!(thresholds.threshold_for(WorkBlockKind::Base), 0);
    }
}
