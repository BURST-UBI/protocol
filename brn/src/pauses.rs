use burst_types::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

type Interval = (Timestamp, Timestamp);

/// Pause intervals in insertion order, backed by a shared persistent tree.
/// Serialization stores only the interval sequence. Neither insertion nor
/// decoding checks timestamp order, overlap or interval direction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PauseHistory {
    rows: imbl::OrdMap<usize, Interval>,
}

impl PauseHistory {
    pub fn len(&self) -> usize { self.rows.len() }
    pub fn is_empty(&self) -> bool { self.rows.is_empty() }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Interval> { self.rows.values() }
    pub fn last(&self) -> Option<&Interval> { self.rows.get(&self.len().checked_sub(1)?) }
    /// Append without validating the interval against earlier entries.
    pub fn push(&mut self, interval: Interval) { self.rows.insert(self.len(), interval); }
    pub fn clear(&mut self) { self.rows.clear(); }

    /// Compare by sequence position, skipping shared tree nodes. Independently
    /// loaded histories may require a full comparison. Callback errors stop
    /// traversal but do not undo callbacks already made.
    pub fn visit_changes<E>(
        &self,
        before: &Self,
        mut emit: impl FnMut(usize, Option<&Interval>, Option<&Interval>) -> Result<(), E>,
    ) -> Result<(), E> {
        use imbl::ordmap::DiffItem;
        for change in before.rows.diff(&self.rows) {
            match change {
                DiffItem::Add(key, value) => emit(*key, None, Some(value))?,
                DiffItem::Remove(key, value) => emit(*key, Some(value), None)?,
                DiffItem::Update { old: (_, old), new: (key, new) } => emit(*key, Some(old), Some(new))?,
            }
        }
        Ok(())
    }
}

impl From<Vec<Interval>> for PauseHistory {
    fn from(intervals: Vec<Interval>) -> Self {
        Self { rows: intervals.into_iter().enumerate().collect() }
    }
}

impl PartialEq<Vec<Interval>> for PauseHistory {
    fn eq(&self, other: &Vec<Interval>) -> bool { self.iter().eq(other.iter()) }
}

impl Serialize for PauseHistory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for PauseHistory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<Interval>::deserialize(deserializer)?.into())
    }
}

#[cfg(test)]
mod tests;
