//! Checkpoint, snapshot and historical-witness transfer messages.
//!
//! Bounds checks validate message shape. Receivers must match the active request,
//! verify assembled bytes and authenticate history against their trusted state.
use burst_types::BlockHash;
use serde::{Deserialize, Serialize};
use CheckpointSyncMessage as Message;

pub const CHUNK_BYTES: usize = 16 * 1024;
/// Maximum checkpoint-record transfer, including any bundled replay history.
/// Snapshot archives have a separate receiver-configured byte budget.
pub const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_WITNESS_BYTES: usize = 1024 * 1024;
pub const MAX_WITNESS_BLOCKS: usize = 128;
pub const MAX_HEADS: usize = 64;
/// Witness bytes plus the count and per-block length prefixes.
pub const MAX_BATCH_BYTES: usize = MAX_WITNESS_BYTES + 4 * MAX_WITNESS_BLOCKS + 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CheckpointSyncMessage {
    TipRequest {
        nonce: u64,
    },
    /// Peer-reported tip, correlated by nonce. It is not a checkpoint certificate.
    TipObservation {
        nonce: u64,
        height: u64,
        epoch: BlockHash,
    },
    Request {
        height: u64,
        offset: u32,
        digest: Option<BlockHash>,
        /// Request the record without bundled replay history. The receiver must
        /// supply its own authenticated base and obtain any required witnesses.
        record_only: bool,
    },
    Chunk {
        height: u64,
        offset: u32,
        total: u32,
        digest: BlockHash,
        data: Vec<u8>,
    },
    SnapshotRequest {
        epoch: BlockHash,
        offset: u64,
        digest: Option<BlockHash>,
    },
    SnapshotChunk {
        epoch: BlockHash,
        offset: u64,
        total: u64,
        digest: BlockHash,
        data: Vec<u8>,
    },
    SnapshotQuery {
        minimum_height: u64,
    },
    /// Availability hint. The receiver authenticates the archive before using it.
    SnapshotOffer {
        epoch: BlockHash,
        height: u64,
        total: u64,
        digest: BlockHash,
    },
    WitnessRequest {
        start: BlockHash,
        offset: u32,
        digest: Option<BlockHash>,
    },
    WitnessChunk {
        start: BlockHash,
        offset: u32,
        total: u32,
        digest: BlockHash,
        data: Vec<u8>,
    },
    WitnessHeadsRequest {
        starts: Vec<BlockHash>,
        offset: u32,
        digest: Option<BlockHash>,
    },
}

impl CheckpointSyncMessage {
    /// Check field ranges, chunk alignment and lengths without verifying digests
    /// or authority. Snapshot totals need an additional local byte-budget check.
    pub fn is_bounded(&self) -> bool {
        match self {
            Self::TipRequest { nonce } => *nonce != 0,
            Self::TipObservation { nonce, epoch, .. } => *nonce != 0 && !epoch.is_zero(),
            Self::WitnessRequest { .. }
            | Self::WitnessChunk { .. }
            | Self::WitnessHeadsRequest { .. } => witness_is_bounded(self),
            Self::SnapshotRequest { .. }
            | Self::SnapshotChunk { .. }
            | Self::SnapshotQuery { .. }
            | Self::SnapshotOffer { .. } => snapshot_is_bounded(self),
            Self::Request {
                height,
                offset,
                digest,
                ..
            } => {
                *height != 0
                    && u64::from(*offset) < MAX_TRANSFER_BYTES
                    && (*offset == 0 || digest.is_some())
                    && (*offset as usize).is_multiple_of(CHUNK_BYTES)
            }
            Self::Chunk {
                height,
                offset,
                total,
                data,
                ..
            } => {
                *height != 0
                    && *total > 0
                    && u64::from(*total) <= MAX_TRANSFER_BYTES
                    && *offset < *total
                    && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                    && data.len() == CHUNK_BYTES.min((*total - *offset) as usize)
            }
        }
    }
}

/// Check snapshot message shape without imposing a maximum archive size.
/// Returns false for other message variants.
pub fn snapshot_is_bounded(message: &Message) -> bool {
    match message {
        Message::SnapshotQuery { minimum_height } => *minimum_height > 0,
        Message::SnapshotOffer {
            epoch,
            height,
            total,
            digest,
        } => !epoch.is_zero() && *height > 0 && !digest.is_zero() && *total >= 25,
        Message::SnapshotRequest {
            epoch,
            offset,
            digest,
        } => {
            !epoch.is_zero()
                && offset.is_multiple_of(CHUNK_BYTES as u64)
                && (*offset == 0 || digest.is_some())
                && digest.is_none_or(|hash| !hash.is_zero())
        }
        Message::SnapshotChunk {
            epoch,
            offset,
            total,
            digest,
            data,
        } => {
            !epoch.is_zero()
                && !digest.is_zero()
                && *total >= 25
                && offset < total
                && offset.is_multiple_of(CHUNK_BYTES as u64)
                && data.len() as u64 == (CHUNK_BYTES as u64).min(*total - *offset)
        }
        _ => false,
    }
}
/// Check witness request ordering and transfer sizes, not block membership.
/// Returns false for other message variants.
pub fn witness_is_bounded(message: &Message) -> bool {
    match message {
        Message::WitnessHeadsRequest {
            starts,
            offset,
            digest,
        } => {
            valid_starts(starts)
                && (*offset as usize) < MAX_BATCH_BYTES
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && (*offset == 0 || digest.is_some())
                && digest.is_none_or(|hash| !hash.is_zero())
        }
        Message::WitnessRequest {
            start,
            offset,
            digest,
        } => {
            !start.is_zero()
                && (*offset as usize) < MAX_BATCH_BYTES
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && (*offset == 0 || digest.is_some())
                && digest.is_none_or(|hash| !hash.is_zero())
        }
        Message::WitnessChunk {
            start,
            offset,
            total,
            digest,
            data,
        } => {
            !start.is_zero()
                && !digest.is_zero()
                && (5..=MAX_BATCH_BYTES).contains(&(*total as usize))
                && offset < total
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && data.len() == CHUNK_BYTES.min((*total - *offset) as usize)
        }
        _ => false,
    }
}
/// Require 1–64 nonzero hashes in strictly increasing byte order.
pub fn valid_starts(starts: &[BlockHash]) -> bool {
    !starts.is_empty()
        && starts.len() <= MAX_HEADS
        && starts.iter().all(|hash| !hash.is_zero())
        && starts
            .windows(2)
            .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_bounds_preserve_large_heights_and_reject_invalid_offsets() {
        let digest = BlockHash::new([7; 32]);
        for height in [129, 1000, u64::MAX] {
            assert!(Message::Request {
                height,
                offset: 0,
                digest: None,
                record_only: true
            }
            .is_bounded());
            assert!(!Message::Request {
                height,
                offset: MAX_TRANSFER_BYTES as u32,
                digest: Some(digest),
                record_only: true
            }
            .is_bounded());
            assert!(!Message::Chunk {
                height,
                offset: 0,
                total: MAX_TRANSFER_BYTES as u32 + 1,
                digest,
                data: vec![7; CHUNK_BYTES]
            }
            .is_bounded());
        }
        assert!(!Message::Request {
            height: 0,
            offset: 0,
            digest: None,
            record_only: false
        }
        .is_bounded());
        assert!(!Message::SnapshotRequest {
            epoch: digest,
            offset: CHUNK_BYTES as u64,
            digest: None
        }
        .is_bounded());
        assert!(Message::SnapshotRequest {
            epoch: digest,
            offset: CHUNK_BYTES as u64,
            digest: Some(digest)
        }
        .is_bounded());
        assert!(!Message::SnapshotRequest {
            epoch: digest,
            offset: CHUNK_BYTES as u64 + 1,
            digest: Some(digest)
        }
        .is_bounded());
    }

    #[test]
    fn witness_heads_require_a_nonempty_bounded_canonical_set() {
        let a = BlockHash::new([1; 32]);
        let b = BlockHash::new([2; 32]);
        assert!(valid_starts(&[a, b]));
        for invalid in [
            vec![],
            vec![BlockHash::ZERO],
            vec![a, a],
            vec![b, a],
            vec![a; MAX_HEADS + 1],
        ] {
            assert!(!Message::WitnessHeadsRequest {
                starts: invalid,
                offset: 0,
                digest: None
            }
            .is_bounded());
        }
    }
}
