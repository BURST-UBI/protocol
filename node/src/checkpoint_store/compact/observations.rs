use super::scratch::ScratchRows;
use super::*;

pub(crate) type RecordObservations = ScratchRows<RecordObservation, 4096>;

impl RecordObservations {
    pub(crate) fn fork(&self) -> Result<Self, StoreError> {
        let mut copy = self.empty_like()?;
        self.visit(|row| copy.push(row))?;
        Ok(copy)
    }

    pub(crate) fn verify(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.visit(|row| row.verify(batch))
    }

    pub(crate) fn retain_through(&mut self, height: u64) -> Result<(), StoreError> {
        let mut retained = self.empty_like()?;
        self.visit(|row| {
            if row.height.is_none_or(|observed| observed <= height) {
                retained.push(row)?;
            }
            Ok(())
        })?;
        *self = retained;
        Ok(())
    }

    pub(crate) fn verify_relocated(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        selections: &(impl crate::confirmed_replay::snapshot::SelectionSource + ?Sized),
    ) -> Result<Self, StoreError> {
        let mut retained = self.empty_like()?;
        self.visit(|mut row| {
            if row.height.is_some() {
                if let Some(absence) = row.verify_relocated(batch, anchor, selections)? {
                    retained.push(absence)?;
                }
                retained.push(row)?;
            }
            Ok(())
        })?;
        Ok(retained)
    }

    /// read_after includes active pre-base records and all selection beacons,
    /// but omits retired pre-base bodies and their active-key absences.
    pub(crate) fn retain_retired_from(
        &mut self,
        previous: &Self,
        base: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        previous.visit(|row| {
            if let Some(height) = row.height {
                if height <= base.height()
                    && (row.key == archived_record_key(base, height)
                        || (row.key == format!("{}/{height}", prefix(base))
                            && matches!(row.value, ObservedValue::Record(None))))
                {
                    self.push(row)?;
                }
            }
            Ok(())
        })
    }
}
