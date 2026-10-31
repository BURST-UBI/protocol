//! Borrow roots from a fixed engine without encoding or collecting its graph.
use super::{DelegationEngine, DelegationScope};
use crate::record_references::Reference;

impl DelegationEngine {
    /// All forward edges remain roots regardless of current eligibility, depth
    /// resolution or scope precedence. Reverse indexes add no independent roots.
    /// The caller retains this iterator to resume; restarting it rescans the graph.
    /// This bounds additional traversal state, not the engine's cold restore size.
    pub fn retention_references(&self) -> impl Iterator<Item = Reference<'_>> + '_ {
        self.delegations.iter().flat_map(|(from, to)| {
            [Reference::Account(from), Reference::Account(to)]
        }).chain(self.scoped_delegations.iter().flat_map(|((from, scope), to)| {
            [Some(Reference::Account(from)), Some(Reference::Account(to)),
                match scope { DelegationScope::Proposal(hash) => Some(Reference::Proposal(*hash)), _ => None }]
                .into_iter().flatten()
        })).chain(self.key_ballots.keys().map(Reference::Account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{TxHash, WalletAddress};

    #[test]
    fn borrowed_delegation_roots_cover_scopes_and_resume_without_resolution() {
        let mut engine = DelegationEngine::new(1);
        let owner = WalletAddress::new("brst_owner");
        let delegate = WalletAddress::new("brst_delegate");
        let final_owner = WalletAddress::new("brst_terminal");
        engine.delegate(&owner, &delegate).unwrap();
        engine.delegate(&delegate, &final_owner).unwrap();
        // Over-depth and shadowed edges still carry retained obligations.
        assert!(engine.resolve(&owner).is_none());
        let proposal = TxHash::new([7;32]);
        for scope in [DelegationScope::Proposal(proposal),
            DelegationScope::Category("economic".into()),
            DelegationScope::Parameter(crate::GovernableParam::BrnRate)] {
            engine.delegate_scoped(&owner, &final_owner, scope).unwrap();
        }
        let revoked = WalletAddress::new("brst_revoked");
        engine.project_key_ballot(revoked.clone(), None);
        engine.project_key_ballot(owner.clone(), Some(burst_transactions::governance::GovernanceVote::Yea));
        let before = engine.save_state();
        let mut candidate = engine.clone();
        let mut roots = engine.retention_references();
        let first = roots.next().unwrap();
        match first { Reference::Account(address) => {
            assert!(engine.delegations.keys().any(|key| std::ptr::eq(address,key)));
        }, _ => panic!("first root must borrow an owner") }
        candidate.undelegate(&owner);
        let mut seen = vec![format!("{first:?}")];
        for page in [2,1,5,100] {
            seen.extend(roots.by_ref().take(page).map(|r| format!("{r:?}")));
        }
        assert!(roots.next().is_none());
        assert_eq!(seen.len(),13); // 2 global edges + 3 scopes + proposal + 2 ballot owners
        assert_eq!(seen.iter().filter(|r| **r == format!("{:?}",Reference::Proposal(proposal))).count(),1);
        assert!(seen.contains(&format!("{:?}",Reference::Account(&revoked))));
        assert_eq!(seen, engine.retention_references().map(|r| format!("{r:?}")).collect::<Vec<_>>());
        assert_eq!(before, engine.save_state());
        assert_eq!(candidate.retention_references().count(),11);
        // A persisted forward graph has no ephemeral key-ballot projection.
        let restored = DelegationEngine::load_state(&before).unwrap();
        assert_eq!(restored.retention_references().count(),11);
        assert_eq!(DelegationEngine::default().retention_references().count(),0);
    }
}
