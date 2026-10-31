//! Graph row updates checked against expected bytes in the active transaction.
use super::*;
use std::collections::HashMap;
use burst_trst::MergerGraph;

impl LmdbWriteTransaction<'_> {
    /// Stage changed graph rows, checking each against `before`. New edges receive
    /// durable positions after the last stored edge in their group.
    /// This does not validate the complete graph or its holdings. Abort on error:
    /// a later conflict can leave earlier row changes staged in this transaction.
    pub fn persist_graph_record_delta(&mut self, before: &MergerGraph, after: &MergerGraph) -> Result<(), StoreError> {
        if !self.uses_graph_records()? { return Err(invalid("graph delta requires row storage")); }
    let mut highest = HashMap::<String, u64>::new();
    after
        .visit_record_delta(before, |old, mut new| {
            let old = old.map(|record| record.storage_entry()).transpose()?;
            let (key, mut new_bytes) = if let Some(record) = &new {
                let (key, bytes) = record.storage_entry()?;
                (key, Some(bytes))
            } else {
                (old.as_ref().ok_or("empty graph delta")?.0.clone(), None)
            };
            if old.as_ref().is_some_and(|(old_key, _)| *old_key != key) {
                return Err("graph delta changes its primary key".into());
            }
            let stored = self.get_meta(&key).map_err(|error| error.to_string())?;
            if stored.as_deref() != old.as_ref().map(|(_, bytes)| bytes.as_slice()) {
                return Err("graph delta conflicts with persisted state".into());
            }
            if let Some(record) = &mut new {
                if let Some((group, _)) = record.edge_group_position() {
                    if old.is_some() {
                        return Err("ordinary graph delta modifies an existing edge".into());
                    }
                    let position = if let Some(highest) = highest.get(&group) {
                        highest
                            .checked_add(1)
                            .ok_or("graph edge ordering exhausted")?
                    } else {
                        let (parent, downstream) = match record {
                            GraphRecord::OriginEdge { origin, .. } => (origin, false),
                            GraphRecord::DownstreamEdge { merge, .. } => (merge, true),
                            _ => unreachable!("edge variant"),
                        };
                        self
                            .next_graph_edge_position(parent, downstream)
                            .map_err(|error| error.to_string())?
                    };
                    highest.insert(group, position);
                    record.set_edge_position(position);
                    // Durable edge position changes the value, never its key.
                    new_bytes = Some(record.storage_entry()?.1);
                }
                let bytes = new_bytes.as_ref().expect("new graph row was encoded");
                self
                    .put_meta(&key, bytes)
                    .map_err(|error| error.to_string())
            } else {
                self.delete_meta(&key).map_err(|error| error.to_string())
            }
        })
        .map_err(invalid)
    }
}
