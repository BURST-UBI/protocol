use super::*;

/// Borrow the exact stored fields, including empty vote buckets and conflicts.
#[derive(serde::Serialize)]
pub struct SnapshotView<'a> {
    pub amendments: &'a HashMap<TxHash, Amendment>,
    pub votes: &'a HashMap<TxHash, HashMap<WalletAddress, ConstiVote>>,
    pub document: &'a ConstiDocument,
    pub conflicts: &'a HashMap<TxHash, AmendmentConflict>,
}
impl ConstiEngine {
    pub fn snapshot_view(&self) -> SnapshotView<'_> {
        SnapshotView { amendments: &self.amendments, votes: &self.votes,
            document: &self.document, conflicts: &self.conflicts }
    }
}
