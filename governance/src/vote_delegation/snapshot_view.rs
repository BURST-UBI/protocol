use super::*;
use crate::engine::SnapshotMap;

/// Sort reverse-source lists lexically when serializing their unordered sets.
pub trait WalletSet: Serialize {
    fn ordered_sources(&self) -> Vec<&WalletAddress>;
}
impl WalletSet for PersistentSet<WalletAddress> {
    fn ordered_sources(&self) -> Vec<&WalletAddress> {
        let mut sources: Vec<_> = self.iter().collect();
        sources.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        sources
    }
}
#[derive(Serialize)]
pub struct DelegationView<'a> {
    delegations: &'a PersistentMap<WalletAddress, WalletAddress>,
    #[serde(serialize_with = "ordered_reverse")]
    reverse_delegations: &'a PersistentMap<WalletAddress, PersistentSet<WalletAddress>>,
    scoped_delegations: &'a PersistentMap<(WalletAddress, DelegationScope), WalletAddress>,
    max_depth: usize,
    key_ballots:
        &'a PersistentMap<WalletAddress, Option<burst_transactions::governance::GovernanceVote>>,
}
impl DelegationEngine {
    pub fn snapshot_view(&self) -> DelegationView<'_> {
        DelegationView {
            delegations: &self.delegations,
            reverse_delegations: &self.reverse_delegations,
            scoped_delegations: &self.scoped_delegations,
            max_depth: self.max_depth,
            key_ballots: &self.key_ballots,
        }
    }
}
impl DelegationView<'_> {
    pub fn delegations(&self) -> &impl SnapshotMap {
        self.delegations
    }
    pub fn reverse(&self) -> &impl SnapshotMap<Value: WalletSet> {
        self.reverse_delegations
    }
    pub fn scoped(&self) -> &impl SnapshotMap {
        self.scoped_delegations
    }
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
    pub fn ballots(&self) -> &impl SnapshotMap {
        self.key_ballots
    }
}
