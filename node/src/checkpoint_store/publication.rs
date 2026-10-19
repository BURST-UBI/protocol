use super::*;
use crate::payment_authority::PaymentAuthority;
use burst_consensus::checkpoint::Checkpoint;
use burst_types::BlockHash;

/// Locate an installed descendant in the already authenticated accepted path.
/// This is not certificate authentication: recovery and admission establish the
/// path and its strictly increasing cutoffs. All reads share the caller's writer.
pub(super) fn parameter_hash(
    selection: &Checkpoint,
    mut low: u64,
    mut high: u64,
    installed: &PaymentAuthority,
    mut read: impl FnMut(u64) -> Result<Checkpoint, StoreError>,
) -> Result<Option<BlockHash>, StoreError> {
    if installed.cutoff < selection.cutoff {
        return Ok(None);
    }
    while low <= high {
        let height = low + (high - low) / 2;
        let record = read(height)?;
        if record.height != height
            || record.network != selection.network
            || record.genesis != selection.genesis
        {
            return Err(invalid("selection chain record has the wrong identity"));
        }
        match record.cutoff.cmp(&installed.cutoff) {
            std::cmp::Ordering::Less => {
                let Some(next) = height.checked_add(1) else {
                    break;
                };
                low = next;
            }
            std::cmp::Ordering::Greater => {
                let Some(previous) = height.checked_sub(1) else {
                    break;
                };
                high = previous;
            }
            std::cmp::Ordering::Equal => {
                if record.hash() != installed.epoch
                    || record.next_roster != installed.roster.commitment()
                    || record.next_quorum_bps != installed.quorum_bps
                    || record.params_hash.is_zero()
                    || height < selection.height
                {
                    return Err(invalid(
                        "installed payment authority differs from its checkpoint",
                    ));
                }
                return Ok(Some(record.params_hash));
            }
        }
    }
    Err(invalid(
        "installed payment checkpoint is missing from accepted history",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(anchor: &CheckpointAnchor, height: u64) -> Checkpoint {
        Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height,
            cutoff: height,
            ledger_root: BlockHash::new([1; 32]),
            params_hash: BlockHash::new([2; 32]),
            next_roster: anchor.roster().commitment(),
            selection_root: BlockHash::new([3; 32]),
            next_quorum_bps: anchor.quorum_bps(),
        }
    }

    #[test]
    fn installed_checkpoint_lookup_has_no_lifetime_scan_or_height_overflow() {
        let anchor =
            ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &ProtocolParams::default()).unwrap();
        let selection = checkpoint(&anchor, 1);
        // Synthetic ordered descriptors exercise lookup bounds, not economic replay.
        for height in [1, 129, 1 << 32, u64::MAX - 1, u64::MAX] {
            let record = checkpoint(&anchor, height);
            let installed = PaymentAuthority {
                epoch: record.hash(),
                roster: anchor.roster().clone(),
                quorum_bps: anchor.quorum_bps(),
                cutoff: height,
            };
            let mut reads = 0;
            let result = parameter_hash(&selection, 1, u64::MAX, &installed, |at| {
                reads += 1;
                Ok(checkpoint(&anchor, at))
            })
            .unwrap();
            assert_eq!(result, Some(record.params_hash));
            assert!(reads <= 64, "{reads} reads for height {height}");
        }
    }

    #[test]
    fn installed_checkpoint_lookup_rejects_inconsistent_identity_and_authority() {
        let anchor =
            ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &ProtocolParams::default()).unwrap();
        let selection = checkpoint(&anchor, 2);
        let record = checkpoint(&anchor, 5);
        let mut installed = PaymentAuthority {
            epoch: record.hash(),
            roster: anchor.roster().clone(),
            quorum_bps: anchor.quorum_bps(),
            cutoff: 5,
        };
        for case in 0..7 {
            let result = parameter_hash(&selection, 2, 8, &installed, |at| {
                let mut value = checkpoint(&anchor, at);
                match case {
                    0 => return Err(invalid("missing record")),
                    1 => value.height += 1,
                    2 => value.genesis = BlockHash::new([9; 32]),
                    3 => value.network = NetworkId::Live,
                    4 => value.next_quorum_bps = 10000,
                    5 => value.params_hash = BlockHash::ZERO,
                    _ => value.cutoff = 99,
                }
                Ok(value)
            });
            assert!(result.is_err(), "case {case}");
        }
        installed.cutoff = 1;
        assert_eq!(
            parameter_hash(&selection, 2, 8, &installed, |_| panic!(
                "older authority needs no lookup"
            ))
            .unwrap(),
            None
        );
    }
}
