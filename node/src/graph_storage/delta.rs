use super::*;

/// Write changed graph rows, checking stored values and preserving edge slots.
/// Aggregate storage falls back to a full snapshot. Abort on error.
pub(crate) fn persist_delta(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &MergerGraph,
    after: &MergerGraph,
) -> Result<(), StoreError> {
    if !batch.uses_graph_records()? {
        return persist(batch, after);
    }
    batch.persist_graph_record_delta(before, after)
}
