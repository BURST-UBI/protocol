use super::*;

/// Read-only persisted map entries; this interface grants no activation authority.
pub trait SnapshotMap: Serialize {
    type Key: Serialize;
    type Value: Serialize;
    fn len(&self) -> usize;
    fn entries(&self) -> impl Iterator<Item = (&Self::Key, &Self::Value)>;
}
impl<K: Ord + Clone + Serialize, V: Clone + Serialize> SnapshotMap for OrdMap<K, V> {
    type Key = K;
    type Value = V;
    fn len(&self) -> usize { self.len() }
    fn entries(&self) -> impl Iterator<Item = (&K, &V)> { self.iter() }
}

impl<K: Eq + std::hash::Hash + Clone + Serialize, V: Clone + Serialize> SnapshotMap for imbl::HashMap<K, V> {
    type Key = K;
    type Value = V;
    fn len(&self) -> usize { self.len() }
    fn entries(&self) -> impl Iterator<Item = (&K, &V)> { self.iter() }
}

/// Field order matches GovernanceEngine's persisted representation.
#[derive(Serialize)]
pub struct SnapshotView<'a> {
    proposals: &'a OrdMap<TxHash, Proposal>,
    exploration_votes: &'a OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    promotion_votes: &'a OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    endorsement_counts: &'a OrdMap<TxHash, (u32, u128)>,
    pending_changes: &'a [(crate::params::GovernableParam, u128)],
    pending_amendments: &'a [ProposalContent],
    pending_evictions: &'a [(WalletAddress, bool)],
    timings: &'a OrdMap<TxHash, crate::GovernanceTiming>,
    key_ballots: &'a OrdMap<(TxHash, WalletAddress, burst_types::BlockHash), GovernanceVote>,
}
impl GovernanceEngine {
    pub fn snapshot_view(&self) -> SnapshotView<'_> {
        SnapshotView {
            proposals: &self.proposals, exploration_votes: &self.exploration_votes,
            promotion_votes: &self.promotion_votes, endorsement_counts: &self.endorsement_counts,
            pending_changes: &self.pending_changes, pending_amendments: &self.pending_amendments,
            pending_evictions: &self.pending_evictions, timings: &self.timings, key_ballots: &self.key_ballots,
        }
    }
}
impl SnapshotView<'_> {
    pub fn proposals(&self) -> &impl SnapshotMap { self.proposals }
    pub fn exploration_votes(&self) -> &impl SnapshotMap<Value: SnapshotMap> { self.exploration_votes }
    pub fn promotion_votes(&self) -> &impl SnapshotMap<Value: SnapshotMap> { self.promotion_votes }
    pub fn endorsements(&self) -> &impl SnapshotMap { self.endorsement_counts }
    pub fn timings(&self) -> &impl SnapshotMap { self.timings }
    pub fn key_ballots(&self) -> &impl SnapshotMap { self.key_ballots }
    pub fn pending_changes(&self) -> &[(crate::params::GovernableParam, u128)] { self.pending_changes }
    pub fn pending_amendments(&self) -> &[ProposalContent] { self.pending_amendments }
    pub fn pending_evictions(&self) -> &[(WalletAddress, bool)] { self.pending_evictions }
}
