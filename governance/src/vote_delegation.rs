//! Ordinary governance delegation with scoped, transitive resolution.
//! Callers authenticate graph changes and supply eligibility at ballot counting.
//!
//! This is governance delegation (one-person-one-vote), distinct from
//! consensus delegation (also weighted by verified people, never balances).

use crate::error::GovernanceError;
use burst_types::{TxHash, WalletAddress};
use imbl::{HashMap as PersistentMap, HashSet as PersistentSet};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod retention;
mod snapshot_undo;
mod snapshot_view;
pub use snapshot_view::{DelegationView, WalletSet};
mod ballot_resolver;
pub(crate) use ballot_resolver::BallotResolver;

/// Scope for a delegation — determines which proposals it applies to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DelegationScope {
    /// Applies to all proposals.
    Global,
    /// Applies only to a specific proposal.
    Proposal(TxHash),
    /// Applies to a category of parameters (e.g., "economic", "verification").
    Category(String),
    /// Applies to one named protocol parameter.
    Parameter(crate::GovernableParam),
}

/// A scoped delegation record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopedDelegation {
    pub from: WalletAddress,
    pub to: WalletAddress,
    pub scope: DelegationScope,
}

/// Manages vote delegation for governance, including transitive and scoped delegation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DelegationEngine(std::sync::Arc<DelegationState>);

/// Shared engine representation; fields remain private to the engine.
#[doc(hidden)]
#[derive(Clone, Serialize, Deserialize)]
pub struct DelegationState {
    /// Global delegations: delegator → delegate.
    delegations: PersistentMap<WalletAddress, WalletAddress>,
    /// Reverse index: delegate → set of direct delegators (global only).
    #[serde(serialize_with = "ordered_reverse")]
    reverse_delegations: PersistentMap<WalletAddress, PersistentSet<WalletAddress>>,
    /// Scoped delegations: (delegator, scope) → delegate.
    scoped_delegations: PersistentMap<(WalletAddress, DelegationScope), WalletAddress>,
    /// Maximum visited accounts, including the starting account and terminal.
    max_depth: usize,
    /// Cut-local key-ballot projection. None blocks legacy fallback for an
    /// explicit scoped revocation or a grant with no currently effective vote.
    key_ballots:
        PersistentMap<WalletAddress, Option<burst_transactions::governance::GovernanceVote>>,
}

impl std::ops::Deref for DelegationEngine {
    type Target = DelegationState;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for DelegationEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

fn ordered_reverse<S: serde::Serializer>(
    reverse: &PersistentMap<WalletAddress, PersistentSet<WalletAddress>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    struct Sources<'a>(&'a PersistentSet<WalletAddress>);
    impl Serialize for Sources<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.ordered_sources().serialize(serializer)
        }
    }
    serializer.collect_map(
        reverse
            .iter()
            .map(|(wallet, sources)| (wallet, Sources(sources))),
    )
}

impl DelegationEngine {
    /// Whether two immutable candidates share the same complete backing state.
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn new(max_depth: usize) -> Self {
        Self(std::sync::Arc::new(DelegationState {
            delegations: PersistentMap::new(),
            reverse_delegations: PersistentMap::new(),
            scoped_delegations: PersistentMap::new(),
            max_depth,
            key_ballots: PersistentMap::new(),
        }))
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub fn project_key_ballot(
        &mut self,
        owner: WalletAddress,
        vote: Option<burst_transactions::governance::GovernanceVote>,
    ) {
        if self.key_ballots.get(&owner) != Some(&vote) {
            self.key_ballots.insert(owner, vote);
        }
    }

    pub fn key_ballot(
        &self,
        owner: &WalletAddress,
    ) -> Option<Option<burst_transactions::governance::GovernanceVote>> {
        self.key_ballots.get(owner).copied()
    }

    /// Set or update a global delegation.
    pub fn delegate(
        &mut self,
        from: &WalletAddress,
        to: &WalletAddress,
    ) -> Result<(), GovernanceError> {
        if from == to {
            return Err(GovernanceError::SelfDelegation);
        }
        if self.delegations.get(from) == Some(to) {
            return Ok(());
        }
        let state = std::sync::Arc::make_mut(&mut self.0);
        if let Some(old_to) = state.delegations.get(from) {
            if let Some(set) = state.reverse_delegations.get_mut(old_to) {
                set.remove(from);
                if set.is_empty() {
                    state.reverse_delegations.remove(old_to);
                }
            }
        }
        state.delegations.insert(from.clone(), to.clone());
        state
            .reverse_delegations
            .entry(to.clone())
            .or_default()
            .insert(from.clone());
        Ok(())
    }

    /// Remove a global delegation.
    pub fn undelegate(&mut self, from: &WalletAddress) {
        if !self.delegations.contains_key(from) {
            return;
        }
        if let Some(old_to) = self.delegations.remove(from) {
            if let Some(set) = self.reverse_delegations.get_mut(&old_to) {
                set.remove(from);
                if set.is_empty() {
                    self.reverse_delegations.remove(&old_to);
                }
            }
        }
    }

    /// Set or update a scoped delegation.
    pub fn delegate_scoped(
        &mut self,
        from: &WalletAddress,
        to: &WalletAddress,
        scope: DelegationScope,
    ) -> Result<(), GovernanceError> {
        if scope == DelegationScope::Global {
            return self.delegate(from, to);
        }
        if from == to {
            return Err(GovernanceError::SelfDelegation);
        }
        if self.scoped_delegations.get(&(from.clone(), scope.clone())) != Some(to) {
            self.scoped_delegations
                .insert((from.clone(), scope), to.clone());
        }
        Ok(())
    }

    /// Remove this scope's edge. Resolution may then use a broader scope;
    /// this is not a key-grant revocation tombstone.
    pub fn undelegate_scoped(&mut self, from: &WalletAddress, scope: DelegationScope) {
        if scope == DelegationScope::Global {
            self.undelegate(from);
            return;
        }
        let key = (from.clone(), scope);
        if self.scoped_delegations.contains_key(&key) {
            self.scoped_delegations.remove(&key);
        }
    }

    /// Follow global edges without checking eligibility. Return the terminal,
    /// or None on a cycle or when max_depth account visits cannot reach it.
    pub fn resolve(&self, from: &WalletAddress) -> Option<WalletAddress> {
        let mut current = from.clone();
        let mut visited = HashSet::new();
        for _ in 0..self.max_depth {
            if !visited.insert(current.clone()) {
                return None;
            }
            match self.delegations.get(&current) {
                Some(next) => current = next.clone(),
                None => return Some(current), // End of chain
            }
        }
        None // Exceeded max depth
    }

    /// Resolve proposal > category > global edges without eligibility checks.
    /// This helper has no parameter context; ballot counting uses
    /// `resolve_for_ballot` or its memoized equivalent.
    pub fn resolve_for_context(
        &self,
        from: &WalletAddress,
        proposal_hash: Option<&TxHash>,
        category: Option<&str>,
    ) -> Option<WalletAddress> {
        self.resolve_for_ballot(from, proposal_hash, None, category, |_| true)
    }

    /// Resolve with proposal > parameter > category > global precedence at
    /// every hop. A revoked or otherwise ineligible intermediate cannot pass
    /// another person's voting authority onward.
    pub fn resolve_for_ballot(
        &self,
        from: &WalletAddress,
        proposal_hash: Option<&TxHash>,
        parameter: Option<&crate::GovernableParam>,
        category: Option<&str>,
        is_eligible: impl Fn(&WalletAddress) -> bool,
    ) -> Option<WalletAddress> {
        let mut current = from.clone();
        let mut visited = HashSet::new();
        for _ in 0..self.max_depth {
            if !is_eligible(&current) || !visited.insert(current.clone()) {
                return None;
            }
            let next = proposal_hash
                .and_then(|h| {
                    self.scoped_delegations
                        .get(&(current.clone(), DelegationScope::Proposal(*h)))
                })
                .or_else(|| {
                    parameter.and_then(|p| {
                        self.scoped_delegations
                            .get(&(current.clone(), DelegationScope::Parameter(p.clone())))
                    })
                })
                .or_else(|| {
                    category.and_then(|c| {
                        self.scoped_delegations
                            .get(&(current.clone(), DelegationScope::Category(c.to_string())))
                    })
                })
                .or_else(|| self.delegations.get(&current));

            match next {
                Some(next_addr) => current = next_addr.clone(),
                None => return Some(current), // End of chain
            }
        }
        None // Exceeded max depth
    }

    /// Structural global-graph count: one plus ancestors that resolve here.
    /// The base one is unconditional, even if this account delegates onward or
    /// is ineligible. Actual ballots also depend on scope, eligibility and votes.
    pub fn voting_power(&self, address: &WalletAddress) -> u32 {
        let mut power = 1u32;
        let mut candidates = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(address.clone());
        while let Some(current) = queue.pop_front() {
            if let Some(delegators) = self.reverse_delegations.get(&current) {
                for delegator in delegators {
                    if candidates.insert(delegator.clone()) {
                        queue.push_back(delegator.clone());
                    }
                }
            }
        }
        for candidate in &candidates {
            if let Some(resolved) = self.resolve(candidate) {
                if &resolved == address {
                    power += 1;
                }
            }
        }
        power
    }

    /// Structural count for proposal/category/global scopes, with an unconditional
    /// base one. Scans all graph owners; it does not check parameter scope,
    /// eligibility, direct votes or key ballots. Do not use it as a ballot tally.
    pub fn voting_power_for_context(
        &self,
        address: &WalletAddress,
        proposal_hash: Option<&TxHash>,
        category: Option<&str>,
    ) -> u32 {
        let candidates: HashSet<_> = self
            .delegations
            .keys()
            .chain(self.scoped_delegations.keys().map(|(owner, _)| owner))
            .collect();
        1 + candidates
            .into_iter()
            .filter(|owner| {
                *owner != address
                    && self
                        .resolve_for_context(owner, proposal_hash, category)
                        .as_ref()
                        == Some(address)
            })
            .count() as u32
    }

    /// Get the direct delegate for a wallet (None if not delegated).
    pub fn get_delegate(&self, delegator: &WalletAddress) -> Option<&WalletAddress> {
        self.delegations.get(delegator)
    }

    /// Get all wallets that directly delegated to a given delegate (global only).
    pub fn get_delegators(&self, delegate: &WalletAddress) -> Vec<&WalletAddress> {
        self.reverse_delegations
            .get(delegate)
            .map(|s| s.iter().collect())
            .unwrap_or_default()
    }

    /// Get all global delegations.
    pub fn get_delegations(&self) -> Vec<(WalletAddress, WalletAddress)> {
        self.delegations
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Meta-store key used for persisting the delegation engine state.
const DELEGATION_ENGINE_META_KEY: &str = "delegation_engine_state";

/// Serializable snapshot of the delegation engine's in-memory graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationSnapshot {
    pub delegations: HashMap<WalletAddress, WalletAddress>,
    pub scoped_delegations: HashMap<(WalletAddress, DelegationScope), WalletAddress>,
    pub max_depth: usize,
}

impl DelegationEngine {
    /// Compare the persisted forward graph without materializing another snapshot.
    /// Reverse indexes and projected key ballots are not part of this format.
    pub fn matches_snapshot(&self, snapshot: &DelegationSnapshot) -> bool {
        self.max_depth == snapshot.max_depth
            && self.delegations.len() == snapshot.delegations.len()
            && self.scoped_delegations.len() == snapshot.scoped_delegations.len()
            && snapshot
                .delegations
                .iter()
                .all(|(from, to)| self.delegations.get(from) == Some(to))
            && snapshot
                .scoped_delegations
                .iter()
                .all(|(key, to)| self.scoped_delegations.get(key) == Some(to))
    }

    /// Serialize the delegation graph to bytes for LMDB persistence.
    pub fn save_state(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct SnapshotRef<'a> {
            delegations: &'a PersistentMap<WalletAddress, WalletAddress>,
            scoped_delegations: &'a PersistentMap<(WalletAddress, DelegationScope), WalletAddress>,
            max_depth: usize,
        }
        let snapshot = SnapshotRef {
            delegations: &self.delegations,
            scoped_delegations: &self.scoped_delegations,
            max_depth: self.max_depth,
        };
        bincode::serialize(&snapshot).unwrap_or_default()
    }

    /// Decode the forward graph and rebuild its global reverse index. Key-ballot
    /// projections are cleared. This does not authenticate snapshot provenance
    /// or establish voter eligibility.
    pub fn load_state(data: &[u8]) -> Result<Self, bincode::Error> {
        use bincode::Options;
        let snapshot: DelegationSnapshot = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(data.len() as u64)
            .reject_trailing_bytes()
            .deserialize(data)?;
        let mut reverse = PersistentMap::<WalletAddress, PersistentSet<WalletAddress>>::new();
        for (from, to) in &snapshot.delegations {
            reverse.entry(to.clone()).or_default().insert(from.clone());
        }
        Ok(Self(std::sync::Arc::new(DelegationState {
            delegations: snapshot.delegations.into_iter().collect(),
            reverse_delegations: reverse,
            scoped_delegations: snapshot.scoped_delegations.into_iter().collect(),
            max_depth: snapshot.max_depth,
            key_ballots: PersistentMap::new(),
        })))
    }

    /// The meta-store key used for delegation engine persistence.
    pub fn meta_key() -> &'static str {
        DELEGATION_ENGINE_META_KEY
    }
}

impl Default for DelegationEngine {
    fn default() -> Self {
        Self::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_delegation_clone_detaches_and_preserves_wire_layout() {
        #[derive(Serialize, Deserialize)]
        struct LegacyState {
            delegations: HashMap<WalletAddress, WalletAddress>,
            reverse_delegations: HashMap<WalletAddress, HashSet<WalletAddress>>,
            scoped_delegations: HashMap<(WalletAddress, DelegationScope), WalletAddress>,
            max_depth: usize,
            key_ballots:
                HashMap<WalletAddress, Option<burst_transactions::governance::GovernanceVote>>,
        }
        let mut original = DelegationEngine::default();
        original.delegate(&wallet("a"), &wallet("b")).unwrap();
        original
            .delegate_scoped(
                &wallet("c"),
                &wallet("d"),
                DelegationScope::Category("economic".into()),
            )
            .unwrap();
        original.project_key_ballot(wallet("a"), None);
        for index in (0..129).rev() {
            original
                .delegate(
                    &wallet(&format!("source_{index}")),
                    &wallet(&format!("target_{}", index % 4)),
                )
                .unwrap();
        }
        #[derive(Serialize)]
        struct Reverse<'a>(
            #[serde(serialize_with = "ordered_reverse")]
            &'a PersistentMap<WalletAddress, PersistentSet<WalletAddress>>,
        );
        let legacy_reverse: HashMap<_, _> = original
            .reverse_delegations
            .iter()
            .map(|(target, sources)| {
                let mut sources: Vec<_> = sources.iter().collect();
                sources.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                (target, sources)
            })
            .collect();
        assert_eq!(
            serde_json::to_value(Reverse(&original.reverse_delegations)).unwrap(),
            serde_json::to_value(&legacy_reverse).unwrap()
        );
        assert_eq!(
            bincode::serialized_size(&Reverse(&original.reverse_delegations)).unwrap(),
            bincode::serialized_size(&legacy_reverse).unwrap()
        );
        let mut candidate = original.clone();
        assert!(std::sync::Arc::ptr_eq(&original.0, &candidate.0));
        let encoded = bincode::serialize(&original).unwrap();
        assert_eq!(
            encoded,
            bincode::serialize(&original.snapshot_view()).unwrap()
        );
        let legacy: LegacyState = bincode::deserialize(&encoded).unwrap();
        assert_eq!(legacy.delegations.get(&wallet("a")), Some(&wallet("b")));
        assert!(legacy.reverse_delegations[&wallet("b")].contains(&wallet("a")));
        assert_eq!(legacy.scoped_delegations.len(), 1);
        assert_eq!(legacy.key_ballots.get(&wallet("a")), Some(&None));
        assert_eq!(bincode::serialize(original.0.as_ref()).unwrap(), encoded);
        let restored: DelegationEngine =
            bincode::deserialize(&bincode::serialize(&legacy).unwrap()).unwrap();
        assert_eq!(restored.get_delegate(&wallet("a")), Some(&wallet("b")));
        assert_eq!(restored.key_ballot(&wallet("a")), Some(None));
        assert_eq!(
            restored.resolve_for_context(&wallet("c"), None, Some("economic")),
            Some(wallet("d"))
        );
        candidate.delegate(&wallet("c"), &wallet("d")).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&original.0, &candidate.0));
        assert_eq!(original.get_delegate(&wallet("c")), None);
        assert_eq!(candidate.get_delegate(&wallet("c")), Some(&wallet("d")));
    }

    #[test]
    fn persistent_delegation_candidates_share_unrelated_maps_and_dense_reverse_sets() {
        let mut original = DelegationEngine::default();
        let hub = wallet("hub");
        let other_hub = wallet("other_hub");
        for n in 0..4096 {
            let owner = wallet(&format!("owner_{n}"));
            original.delegate(&owner, &hub).unwrap();
            original
                .delegate_scoped(
                    &owner,
                    &other_hub,
                    DelegationScope::Category("economic".into()),
                )
                .unwrap();
        }
        original.delegate(&wallet("unrelated"), &other_hub).unwrap();
        let mut projected = original.clone();
        projected.project_key_ballot(wallet("owner_0"), None);
        assert!(projected.delegations.ptr_eq(&original.delegations));
        assert!(projected
            .scoped_delegations
            .ptr_eq(&original.scoped_delegations));
        assert!(projected
            .reverse_delegations
            .ptr_eq(&original.reverse_delegations));
        assert_eq!(original.key_ballot(&wallet("owner_0")), None);
        assert_eq!(projected.key_ballot(&wallet("owner_0")), Some(None));
        let mut candidate = original.clone();
        candidate
            .delegate(&wallet("owner_0"), &wallet("new_hub"))
            .unwrap();
        assert!(candidate
            .scoped_delegations
            .ptr_eq(&original.scoped_delegations));
        assert!(candidate.reverse_delegations[&other_hub]
            .ptr_eq(&original.reverse_delegations[&other_hub]));
        assert_eq!(candidate.get_delegators(&hub).len(), 4095);
        assert_eq!(original.get_delegators(&hub).len(), 4096);
        candidate.undelegate(&wallet("owner_0"));
        assert!(candidate.get_delegators(&wallet("new_hub")).is_empty());
        assert_eq!(original.get_delegate(&wallet("owner_0")), Some(&hub));
        let snapshot: DelegationSnapshot = bincode::deserialize(&candidate.save_state()).unwrap();
        assert_eq!(snapshot.delegations.len(), 4096);
        assert_eq!(snapshot.scoped_delegations.len(), 4096);
        let restored =
            DelegationEngine::load_state(&bincode::serialize(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.get_delegators(&hub).len(), 4095);
        assert_eq!(
            restored.resolve_for_context(&wallet("owner_0"), None, Some("economic")),
            Some(other_hub)
        );
        let mut unchanged = original.clone();
        unchanged.delegate(&wallet("owner_0"), &hub).unwrap();
        unchanged.undelegate(&wallet("absent"));
        unchanged
            .delegate_scoped(
                &wallet("owner_0"),
                &wallet("other_hub"),
                DelegationScope::Category("economic".into()),
            )
            .unwrap();
        unchanged.undelegate_scoped(
            &wallet("absent"),
            DelegationScope::Category("economic".into()),
        );
        assert!(unchanged.shares_storage_with(&original));
        let mut same_projection = projected.clone();
        same_projection.project_key_ballot(wallet("owner_0"), None);
        assert!(same_projection.shares_storage_with(&projected));
    }

    #[test]
    fn persisted_delegation_roundtrip_and_corruption() {
        let mut engine = DelegationEngine::new(10);
        let from = wallet("from");
        let to = wallet("to");
        engine.delegate(&from, &to).unwrap();
        let restored = DelegationEngine::load_state(&engine.save_state()).unwrap();
        assert_eq!(restored.get_delegate(&from), Some(&to));
        assert_eq!(restored.get_delegators(&to), vec![&from]);
        assert!(DelegationEngine::load_state(b"corrupt snapshot").is_err());
        assert!(DelegationEngine::load_state(&[]).is_err());
    }

    fn wallet(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>75}", name))
    }

    fn tx_hash(seed: u8) -> TxHash {
        TxHash::new([seed; 32])
    }

    #[test]
    fn test_simple_delegation() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        engine.delegate(&a, &b).unwrap();

        assert_eq!(engine.resolve(&a), Some(b.clone()));
        assert_eq!(engine.voting_power(&b), 2); // own + A
    }

    #[test]
    fn test_transitive_chain_a_b_c() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        engine.delegate(&a, &b).unwrap();
        engine.delegate(&b, &c).unwrap();

        assert_eq!(engine.resolve(&a), Some(c.clone()));
        assert_eq!(engine.resolve(&b), Some(c.clone()));
        assert_eq!(engine.voting_power(&c), 3); // own + A + B
        assert_eq!(engine.voting_power(&b), 1); // B delegated onward, no one resolves to B
    }

    #[test]
    fn test_cycle_detection() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        engine.delegate(&a, &b).unwrap();
        engine.delegate(&b, &c).unwrap();
        engine.delegate(&c, &a).unwrap();

        assert_eq!(engine.resolve(&a), None);
        assert_eq!(engine.resolve(&b), None);
        assert_eq!(engine.resolve(&c), None);
    }

    #[test]
    fn test_max_depth_exceeded() {
        let mut engine = DelegationEngine::new(5);
        let wallets: Vec<WalletAddress> = (0..7).map(|i| wallet(&format!("w{}", i))).collect();

        for i in 0..6 {
            engine.delegate(&wallets[i], &wallets[i + 1]).unwrap();
        }

        // Chain length is 6 hops, max_depth is 5 → exceeds limit
        assert_eq!(engine.resolve(&wallets[0]), None);
        // Shorter chains still work
        assert_eq!(engine.resolve(&wallets[3]), Some(wallets[6].clone()));
    }

    #[test]
    fn test_self_delegation_rejected() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        assert!(engine.delegate(&a, &a).is_err());
    }

    #[test]
    fn test_undelegate() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        engine.delegate(&a, &b).unwrap();
        assert_eq!(engine.resolve(&a), Some(b.clone()));

        engine.undelegate(&a);
        assert_eq!(engine.resolve(&a), Some(a.clone()));
        assert_eq!(engine.voting_power(&b), 1); // Back to just own vote
    }

    #[test]
    fn test_update_delegation() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        engine.delegate(&a, &b).unwrap();
        assert_eq!(engine.resolve(&a), Some(b.clone()));

        engine.delegate(&a, &c).unwrap();
        assert_eq!(engine.resolve(&a), Some(c.clone()));
    }

    #[test]
    fn test_no_delegation_resolves_to_self() {
        let engine = DelegationEngine::new(10);
        let a = wallet("a");
        assert_eq!(engine.resolve(&a), Some(a.clone()));
    }

    #[test]
    fn test_voting_power_no_delegations() {
        let engine = DelegationEngine::new(10);
        let a = wallet("a");
        assert_eq!(engine.voting_power(&a), 1);
    }

    #[test]
    fn test_voting_power_fan_in() {
        let mut engine = DelegationEngine::new(10);
        let delegate = wallet("delegate");
        for i in 0..5 {
            let d = wallet(&format!("d{}", i));
            engine.delegate(&d, &delegate).unwrap();
        }
        assert_eq!(engine.voting_power(&delegate), 6); // own + 5 delegators
    }

    #[test]
    fn scoped_only_transitive_ancestors_are_counted_once() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        let proposal = tx_hash(1);
        engine
            .delegate_scoped(&a, &b, DelegationScope::Proposal(proposal))
            .unwrap();
        engine
            .delegate_scoped(&b, &c, DelegationScope::Proposal(proposal))
            .unwrap();
        assert_eq!(
            engine.voting_power_for_context(&c, Some(&proposal), None),
            3
        );
        assert_eq!(
            engine.voting_power_for_context(&c, Some(&tx_hash(2)), None),
            1
        );
        engine
            .delegate_scoped(&a, &c, DelegationScope::Global)
            .unwrap();
        assert_eq!(
            engine.voting_power_for_context(&c, Some(&proposal), None),
            3
        );
        engine.undelegate_scoped(&a, DelegationScope::Global);
        assert!(engine.get_delegate(&a).is_none());
    }

    #[test]
    fn test_proposal_scoped_overrides_global() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let global_delegate = wallet("global");
        let proposal_delegate = wallet("proposal");
        let hash = tx_hash(1);

        engine.delegate(&a, &global_delegate).unwrap();
        engine
            .delegate_scoped(&a, &proposal_delegate, DelegationScope::Proposal(hash))
            .unwrap();

        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), None),
            Some(proposal_delegate.clone())
        );
        // Global still works for other proposals
        let other_hash = tx_hash(2);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&other_hash), None),
            Some(global_delegate.clone())
        );
    }

    #[test]
    fn test_category_scoped_overrides_global() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let global_delegate = wallet("global");
        let category_delegate = wallet("category");

        engine.delegate(&a, &global_delegate).unwrap();
        engine
            .delegate_scoped(
                &a,
                &category_delegate,
                DelegationScope::Category("economic".to_string()),
            )
            .unwrap();

        assert_eq!(
            engine.resolve_for_context(&a, None, Some("economic")),
            Some(category_delegate.clone())
        );
        assert_eq!(
            engine.resolve_for_context(&a, None, Some("governance")),
            Some(global_delegate.clone())
        );
    }

    #[test]
    fn test_proposal_scope_beats_category() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let global_delegate = wallet("global");
        let category_delegate = wallet("category");
        let proposal_delegate = wallet("proposal");
        let hash = tx_hash(1);

        engine.delegate(&a, &global_delegate).unwrap();
        engine
            .delegate_scoped(
                &a,
                &category_delegate,
                DelegationScope::Category("economic".to_string()),
            )
            .unwrap();
        engine
            .delegate_scoped(&a, &proposal_delegate, DelegationScope::Proposal(hash))
            .unwrap();

        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), Some("economic")),
            Some(proposal_delegate.clone())
        );
    }

    #[test]
    fn test_scoped_falls_back_to_global() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let global_delegate = wallet("global");
        engine.delegate(&a, &global_delegate).unwrap();

        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), Some("economic")),
            Some(global_delegate.clone())
        );
    }

    #[test]
    fn test_undelegate_scoped() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let hash = tx_hash(1);

        engine
            .delegate_scoped(&a, &b, DelegationScope::Proposal(hash))
            .unwrap();
        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), None),
            Some(b.clone())
        );

        let hash = tx_hash(1);
        engine.undelegate_scoped(&a, DelegationScope::Proposal(hash));
        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), None),
            Some(a.clone())
        );
    }

    #[test]
    fn test_scoped_self_delegation_rejected() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let hash = tx_hash(1);
        assert!(engine
            .delegate_scoped(&a, &a, DelegationScope::Proposal(hash))
            .is_err());
    }

    #[test]
    fn test_scoped_transitive_chain() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        let hash = tx_hash(1);

        // A delegates to B (proposal-scoped), B delegates to C (global)
        engine
            .delegate_scoped(&a, &b, DelegationScope::Proposal(hash))
            .unwrap();
        engine.delegate(&b, &c).unwrap();

        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), None),
            Some(c.clone())
        );
    }

    #[test]
    fn test_voting_power_for_context() {
        let mut engine = DelegationEngine::new(10);
        let a = wallet("a");
        let b = wallet("b");
        let c = wallet("c");
        let hash = tx_hash(1);

        // Global: A→B
        engine.delegate(&a, &b).unwrap();
        // For this proposal: A→C (overrides global)
        engine
            .delegate_scoped(&a, &c, DelegationScope::Proposal(hash))
            .unwrap();

        // In global context, B has power 2 (own + A), C has power 1
        assert_eq!(engine.voting_power(&b), 2);
        assert_eq!(engine.voting_power(&c), 1);

        // In proposal context, C has power 2 (own + A), B has power 1
        let hash = tx_hash(1);
        assert_eq!(engine.voting_power_for_context(&c, Some(&hash), None), 2);
        assert_eq!(engine.voting_power_for_context(&b, Some(&hash), None), 1);
    }

    #[test]
    fn test_default_max_depth() {
        let engine = DelegationEngine::default();
        assert_eq!(engine.max_depth, 10);
    }
}
