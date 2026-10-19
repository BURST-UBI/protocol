use super::scratch::ScratchRows;
use super::*;

// The existing beacon limit plus the generated key and fixed record fields.
pub(crate) type RecoveryReferences = ScratchRows<RecoveryRecordRef, 8192>;

impl RecoveryReferences {
    pub(crate) fn first(&self) -> Result<Option<RecoveryRecordRef>, StoreError> {
        let mut first = None;
        self.visit(|row| {
            if first.is_none() {
                first = Some(row);
            }
            Ok(())
        })?;
        Ok(first)
    }

    /// Check the complete spool, including records after the requested target.
    /// Keep only the exact requested prefix, never the latest accepted tip.
    pub(crate) fn select_through(
        &mut self,
        base: &CheckpointAnchor,
        target: Option<burst_types::BlockHash>,
    ) -> Result<u64, StoreError> {
        let mut height = base.height();
        let Some(target) = target else {
            self.visit(|row| {
                height = row.height;
                Ok(())
            })?;
            return Ok(height);
        };
        let mut selected = self.empty_like()?;
        let mut found = target == base.hash();
        self.visit(|row| {
            if !found {
                found = row.checkpoint == target;
                height = row.height;
                selected.push(row)?;
            }
            Ok(())
        })?;
        if !found {
            return Err(invalid(
                "requested recovery epoch is not the base or a stored successor",
            ));
        }
        *self = selected;
        Ok(height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(height: u64) -> burst_types::BlockHash {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&height.to_be_bytes());
        burst_types::BlockHash::new(bytes)
    }

    #[test]
    fn successor_stream_keeps_exact_target_and_order_without_lifetime_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let base =
            ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &ProtocolParams::default()).unwrap();
        let mut refs = RecoveryReferences::new(dir.path()).unwrap();
        for height in 1..=131_073 {
            refs.push(RecoveryRecordRef {
                height,
                checkpoint: hash(height),
                key: format!("record/{height}"),
                digest: hash(height + 1),
                beacon: None,
            })
            .unwrap();
        }
        assert_eq!(refs.select_through(&base, None).unwrap(), 131_073);
        assert!(refs.select_through(&base, Some(hash(131_074))).is_err());
        assert_eq!(refs.len(), 131_073);
        assert_eq!(refs.first().unwrap().unwrap().height, 1);
        assert_eq!(refs.select_through(&base, Some(hash(19))).unwrap(), 19);
        assert_eq!(refs.len(), 19);
        let mut height = 1;
        for row in refs.into_cursor().unwrap() {
            let row = row.unwrap();
            assert_eq!(row.height, height);
            assert_eq!(row.checkpoint, hash(height));
            assert_eq!(row.digest, hash(height + 1));
            height += 1;
        }
        assert_eq!(height, 20);
        let mut empty = RecoveryReferences::new(dir.path()).unwrap();
        assert!(empty.first().unwrap().is_none());
        assert_eq!(empty.select_through(&base, Some(base.hash())).unwrap(), 0);
        assert!(empty.into_cursor().unwrap().next().is_none());
    }
}
