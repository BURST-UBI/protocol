//! Vote delegation — entrust voting power to a representative.
//!
//! Supports:
//! - **Transitive delegation** (A→B→C means A's vote goes to C)
//! - **Cycle detection** and max-depth limits
//! - **Scoped delegation** (per-proposal, per-category, and global)
//!
//! This is governance delegation (one-person-one-vote), distinct from
//! consensus delegation (balance-weighted for ORV).

use crate::error::GovernanceError;
use burst_types::{TxHash, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Scope for a delegation — determines which proposals it applies to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DelegationScope {
    /// Applies to all proposals.
    Global,
    /// Applies only to a specific proposal.
    Proposal(TxHash),
    /// Applies to a category of parameters (e.g., "economic", "verification").
    Category(String),
}

/// A scoped delegation record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopedDelegation {
    pub from: WalletAddress,
    pub to: WalletAddress,
    pub scope: DelegationScope,
}

/// Manages vote delegation for governance, including transitive and scoped delegation.
pub struct DelegationEngine {
    /// Global delegations: delegator → delegate.
    delegations: HashMap<WalletAddress, WalletAddress>,
    /// Reverse index: delegate → set of direct delegators (global only).
    reverse_delegations: HashMap<WalletAddress, HashSet<WalletAddress>>,
    /// Scoped delegations: (delegator, scope) → delegate.
    scoped_delegations: HashMap<(WalletAddress, DelegationScope), WalletAddress>,
    /// Maximum transitive chain depth (to prevent abuse).
    max_depth: usize,
}

impl DelegationEngine {
    pub fn new(max_depth: usize) -> Self {
        Self {
            delegations: HashMap::new(),
            reverse_delegations: HashMap::new(),
            scoped_delegations: HashMap::new(),
            max_depth,
        }
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
        if let Some(old_to) = self.delegations.get(from) {
            if let Some(set) = self.reverse_delegations.get_mut(old_to) {
                set.remove(from);
                if set.is_empty() {
                    self.reverse_delegations.remove(old_to);
                }
            }
        }
        self.delegations.insert(from.clone(), to.clone());
        self.reverse_delegations
            .entry(to.clone())
            .or_default()
            .insert(from.clone());
        Ok(())
    }

    /// Remove a global delegation.
    pub fn undelegate(&mut self, from: &WalletAddress) {
        if let Some(old_to) = self.delegations.remove(from) {
            if let Some(set) = self.reverse_delegations.get_mut(&old_to) {
                set.remove(from);
                if set.is_empty() {
                    self.reverse_delegations.remove(&old_to);
                }
            }
        }
    }

    /// Set or update a scoped delegation (per-proposal or per-category).
    pub fn delegate_scoped(
        &mut self,
        from: &WalletAddress,
        to: &WalletAddress,
        scope: DelegationScope,
    ) -> Result<(), GovernanceError> {
        if from == to {
            return Err(GovernanceError::SelfDelegation);
        }
        self.scoped_delegations
            .insert((from.clone(), scope), to.clone());
        Ok(())
    }

    /// Remove a scoped delegation.
    pub fn undelegate_scoped(&mut self, from: &WalletAddress, scope: DelegationScope) {
        self.scoped_delegations.remove(&(from.clone(), scope));
    }

    /// Resolve the final delegate for a given address following the global chain.
    /// Returns None if there is a cycle or the chain exceeds max_depth.
    pub fn resolve(&self, from: &WalletAddress) -> Option<WalletAddress> {
        let mut current = from.clone();
        let mut visited = HashSet::new();
        for _ in 0..self.max_depth {
            if !visited.insert(current.clone()) {
                return None; // Cycle detected
            }
            match self.delegations.get(&current) {
                Some(next) => current = next.clone(),
                None => return Some(current), // End of chain
            }
        }
        None // Exceeded max depth
    }

    /// Resolve the final delegate for a given address with scoped context.
    ///
    /// At each step in the chain, checks delegates in priority order:
    /// 1. Proposal-specific delegation
    /// 2. Category delegation
    /// 3. Global delegation
    ///
    /// Returns None if there is a cycle or the chain exceeds max_depth.
    pub fn resolve_for_context(
        &self,
        from: &WalletAddress,
        proposal_hash: Option<&TxHash>,
        category: Option<&str>,
    ) -> Option<WalletAddress> {
        let mut current = from.clone();
        let mut visited = HashSet::new();
        for _ in 0..self.max_depth {
            if !visited.insert(current.clone()) {
                return None; // Cycle detected
            }
            let next = proposal_hash
                .and_then(|h| {
                    self.scoped_delegations
                        .get(&(current.clone(), DelegationScope::Proposal(*h)))
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

    /// Get the total voting power for an address (own vote + all delegated votes).
    ///
    /// Collects candidate delegators via reverse-index BFS, then verifies
    /// each resolves to `address` (accounts for transitive chains that
    /// pass through intermediate nodes).
    pub fn voting_power(&self, address: &WalletAddress) -> u32 {
        let mut power = 1u32;
        let mut candidates = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(address.clone());
        while let Some(current) = queue.pop_front() {
            if let Some(delegators) = self.reverse_delegations.get(&current) {
                for d in delegators {
                    if candidates.insert(d.clone()) {
                        queue.push_back(d.clone());
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

    /// Get the total voting power for an address in a scoped context.
    pub fn voting_power_for_context(
        &self,
        address: &WalletAddress,
        proposal_hash: Option<&TxHash>,
        category: Option<&str>,
    ) -> u32 {
        let mut power = 1u32; // Own vote
        let mut all_delegators = HashSet::new();
        for delegator in self.delegations.keys() {
            all_delegators.insert(delegator.clone());
        }
        for (delegator, _) in self.scoped_delegations.keys() {
            all_delegators.insert(delegator.clone());
        }
        for delegator in &all_delegators {
            if let Some(resolved) = self.resolve_for_context(delegator, proposal_hash, category) {
                if &resolved == address {
                    power += 1;
                }
            }
        }
        power
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegationSnapshot {
    pub delegations: HashMap<WalletAddress, WalletAddress>,
    pub scoped_delegations: HashMap<(WalletAddress, DelegationScope), WalletAddress>,
    pub max_depth: usize,
}

impl DelegationEngine {
    /// Serialize the delegation graph to bytes for LMDB persistence.
    pub fn save_state(&self) -> Vec<u8> {
        let snapshot = DelegationSnapshot {
            delegations: self.delegations.clone(),
            scoped_delegations: self.scoped_delegations.clone(),
            max_depth: self.max_depth,
        };
        bincode::serialize(&snapshot).unwrap_or_default()
    }

    /// Restore the delegation graph from serialized bytes.
    pub fn load_state(data: &[u8]) -> Self {
        match bincode::deserialize::<DelegationSnapshot>(data) {
            Ok(snapshot) => {
                let mut reverse = HashMap::<WalletAddress, HashSet<WalletAddress>>::new();
                for (from, to) in &snapshot.delegations {
                    reverse.entry(to.clone()).or_default().insert(from.clone());
                }
                Self {
                    delegations: snapshot.delegations,
                    reverse_delegations: reverse,
                    scoped_delegations: snapshot.scoped_delegations,
                    max_depth: snapshot.max_depth,
                }
            }
            Err(_) => Self::default(),
        }
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

    fn wallet(name: &str) -> WalletAddress {
        WalletAddress::new(format!(
            "brst_{:0>75}",
            name
        ))
    }

    fn tx_hash(seed: u8) -> TxHash {
        TxHash::new([seed; 32])
    }

    // ── Transitive Delegation (6.1) ──────────────────────────────────────

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
        let wallets: Vec<WalletAddress> = (0..7)
            .map(|i| wallet(&format!("w{}", i)))
            .collect();

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

    // ── Scoped Delegation (6.2) ──────────────────────────────────────────

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
        let global_del = wallet("global");
        let cat_del = wallet("category");
        let prop_del = wallet("proposal");
        let hash = tx_hash(1);

        engine.delegate(&a, &global_del).unwrap();
        engine
            .delegate_scoped(
                &a,
                &cat_del,
                DelegationScope::Category("economic".to_string()),
            )
            .unwrap();
        engine
            .delegate_scoped(&a, &prop_del, DelegationScope::Proposal(hash))
            .unwrap();

        let hash = tx_hash(1);
        assert_eq!(
            engine.resolve_for_context(&a, Some(&hash), Some("economic")),
            Some(prop_del.clone())
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
