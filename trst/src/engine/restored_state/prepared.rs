use super::*;

impl RestoredTrstState {
    /// Return a candidate rebased onto this validated state without modifying it.
    /// The caller must declare every portfolio read by the payment; changed
    /// portfolios are also checked for write conflicts. Ownership removals,
    /// revocation, revision and expiry changes prevent this rebase. Independent
    /// ownership and graph appends can be combined.
    ///
    /// Some contains checked TRST state, not an authorized ledger transition.
    /// None means the indexed path cannot handle the candidate; the caller must
    /// validate it through the serial path. Errors leave this state unchanged.
    pub fn rebase_payment(
        &self,
        before: &TrstEngine,
        after: &TrstEngine,
        read_wallets: &[WalletAddress],
    ) -> Result<Option<Self>, String> {
        let current = &self.engine;
        for context in [current, after] {
            if context.expiry_secs != before.expiry_secs
                || !context.outcome_invalidations.shares_storage_with(&before.outcome_invalidations)
            {
                return Err("prepared TRST global context changed".into());
            }
        }
        let unchanged = |holder: &WalletAddress| match
            (current.wallets.get(holder), before.wallets.get(holder)) {
                (None, None) => true,
                (Some(a), Some(b)) => a.shares_storage_with(b),
                _ => false,
            };
        for holder in read_wallets {
            if !unchanged(holder) {
                return Err("prepared TRST portfolio dependency changed".into());
            }
        }
        // Registry growth cannot change an existing burn's owner. Validate both
        // branches structurally, then patch only candidate additions into proof.
        append_origins(before, current)?;
        let changes = append_origins(before, after)?;
        for origin in changes.origins.keys() {
            if self.owners.contains_key(origin) {
                return Err("prepared TRST burn ID already exists".into());
            }
        }
        let mut context = self.clone();
        context.replace_records(changes)?;
        let owned = context.engine();
        let mut rebased = owned.clone();
        rebased.merger_graph = current.merger_graph.rebase_appends(&before.merger_graph, &after.merger_graph)?
            .ok_or("prepared TRST graph dependency changed")?;
        after.visit_portfolio_changes(before, |holder, _, new| {
            if !unchanged(holder) {
                return Err("prepared TRST write conflicts with current portfolio".into());
            }
            match new {
                Some(_) => { rebased.wallets.insert(holder.clone(), after.wallets[holder].clone()); }
                None => { rebased.wallets.remove(holder); }
            }
            Ok::<(), String>(())
        })?;
        // Reuse the checked delta path to derive holder/revision indexes from
        // changed tokens. Never adopt the candidate's public derived indexes.
        if current.merger_graph.shares_storage_with(&rebased.merger_graph) {
            context.retained_holdings_successor(owned, &rebased)
        } else {
            context.retained_append_successor(owned, &rebased)
        }
    }
}

fn append_origins(before: &TrstEngine, after: &TrstEngine) -> Result<RestoredTrstChanges, String> {
    let mut changes = RestoredTrstChanges::default();
    let empty = SharedSet::default();
    for owner in after.wallet_origins.changed_keys(&before.wallet_origins) {
        let old = before.wallet_origins.get(owner).unwrap_or(&empty);
        let new = after.wallet_origins.get(owner)
            .ok_or("prepared TRST removed an owner")?;
        if !before.wallet_origins.contains_key(owner) {
            changes.owner_headers.insert(owner.clone(), true);
        }
        for origin in new.changed_keys(old) {
            if old.contains(origin) {
                return Err("prepared TRST removed or reassigned a burn".into());
            }
            if changes.origins.insert(*origin, Some(owner.clone())).is_some() {
                return Err("prepared TRST duplicate burn ownership".into());
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_burns_preserve_independent_ownership_and_reject_collisions() {
        let mut proof = RestoredTrstState::from_engine(super::super::tests::fixture()).unwrap();
        proof.retain_portfolio_indexes().unwrap();
        let base = proof.engine().clone();
        let a = WalletAddress::new("brst_owner_1");
        let b = WalletAddress::new("brst_new_owner");
        let mint = |engine: &mut TrstEngine, owner: &WalletAddress, id: u8| {
            // Register a burn origin without adding a holding.
            engine.mint(TxHash::new([id; 32]), owner.clone(), 7,
                owner.clone(), Timestamp::new(101)).unwrap();
        };
        let mut first = base.clone();
        mint(&mut first, &a, 40);
        let current = RestoredTrstState::from_engine(first.clone()).unwrap();
        let mut second = base.clone();
        mint(&mut second, &b, 41);
        let rebased = current.rebase_payment(&base, &second, &[b.clone()]).unwrap().unwrap();
        let mut expected = first.clone();
        mint(&mut expected, &b, 41);
        assert_eq!(rebased.engine().save_wallets(), expected.save_wallets());
        assert_eq!(rebased.owners, RestoredTrstState::from_engine(expected).unwrap().owners);
        assert!(rebased.engine.wallets.shares_storage_with(&current.engine.wallets));
        assert_eq!(current.engine().save_wallets(), first.save_wallets());
        assert!(rebased.rebase_payment(&base, &second, &[b.clone()]).is_err());
        let mut duplicate = second.clone();
        duplicate.wallet_origins.entry(a.clone()).or_default().insert(TxHash::new([41; 32]));
        assert!(current.rebase_payment(&base, &duplicate, &[]).is_err());
        let mut reassigned = base.clone();
        reassigned.wallet_origins.get_mut(&a).unwrap().remove(&TxHash::new([1; 32]));
        reassigned.wallet_origins.entry(b.clone()).or_default().insert(TxHash::new([1; 32]));
        assert!(proof.rebase_payment(&base, &reassigned, &[]).is_err());
        let mut moved_pending = first.clone();
        moved_pending.wallet_origins.get_mut(&a).unwrap().remove(&TxHash::new([40; 32]));
        moved_pending.wallet_origins.entry(b.clone()).or_default().insert(TxHash::new([40; 32]));
        assert!(RestoredTrstState::from_engine(moved_pending).unwrap()
            .rebase_payment(&first, &first, &[]).is_err());
        let mut existing_collision = base.clone();
        existing_collision.wallet_origins.entry(b.clone()).or_default().insert(TxHash::new([1; 32]));
        assert!(proof.rebase_payment(&base, &existing_collision, &[]).is_err());
        let mut merge_collision = base.clone();
        merge_collision.merger_graph.record_merge(crate::merger_graph::MergeNode {
            merge_tx: TxHash::new([41; 32]), source_origins: vec![
                crate::merger_graph::MergeSource { origin: TxHash::new([1; 32]), amount: 1 }],
            total_amount: 1, revoked_contribs: std::collections::HashMap::new(),
        });
        let merged = RestoredTrstState::from_engine(merge_collision).unwrap();
        assert!(merged.rebase_payment(&base, &second, &[]).is_err());
    }

    #[test]
    fn prepared_merges_preserve_other_accounts_and_checked_indexes() {
        let mut engine = super::super::tests::fixture();
        let a = WalletAddress::new("brst_owner_1");
        let b = WalletAddress::new("brst_owner_2");
        for (owner, id) in [(&a, 4), (&b, 5)] {
            let token = engine.mint(TxHash::new([id; 32]), owner.clone(), 30,
                owner.clone(), Timestamp::new(100)).unwrap();
            engine.track_token(token);
        }
        let mut proof = RestoredTrstState::from_engine(engine).unwrap();
        proof.retain_portfolio_indexes().unwrap();
        let base = proof.engine().clone();
        let merge = |state: &mut TrstEngine, holder: &WalletAddress, ids: [u8; 2], id: u8| {
            let ids = ids.map(|id| TxHash::new([id; 32]));
            let inputs = state.wallets[holder].select_merge_inputs(&ids).unwrap();
            let token = state.merge(&inputs, holder.clone(), TxHash::new([id; 32]), Timestamp::new(101)).unwrap();
            state.bulk_untrack(holder, &ids.into_iter().collect());
            state.track_token(token);
        };
        let mut first = base.clone();
        merge(&mut first, &a, [1, 4], 10);
        let current = proof.retained_append_successor(&base, &first).unwrap().unwrap();
        let mut second = base.clone();
        merge(&mut second, &b, [2, 5], 11);
        let rebased = current.rebase_payment(&base, &second, &[b.clone()]).unwrap().unwrap();
        let mut expected = first.clone();
        merge(&mut expected, &b, [2, 5], 11);
        let full = RestoredTrstState::from_engine(expected).unwrap();
        assert_eq!(rebased.engine().save_wallets(), full.engine().save_wallets());
        assert_eq!(rebased.engine().merger_graph.to_bytes(), full.engine().merger_graph.to_bytes());
        assert_eq!(rebased.engine.origin_holders, full.engine.origin_holders);
        assert_eq!(rebased.engine.revoked_holders, full.engine.revoked_holders);
        assert_eq!(rebased.revision_index, full.revision_index);
        assert_eq!(rebased.token_revisions, full.token_revisions);
        assert!(rebased.engine.wallets[&a].shares_storage_with(&current.engine.wallets[&a]));
        assert!(current.rebase_payment(&base, &second, &[a.clone()]).is_err());
        let mut collision = base.clone();
        // A valid graph structure cannot reuse a registered burn ID as a merge.
        collision.merger_graph.record_merge(crate::merger_graph::MergeNode {
            merge_tx: TxHash::new([3; 32]), source_origins: vec![
                crate::merger_graph::MergeSource { origin: TxHash::new([1; 32]), amount: 1 }],
            total_amount: 1, revoked_contribs: std::collections::HashMap::new(),
        });
        assert!(proof.rebase_payment(&base, &collision, &[]).is_err());
    }

    #[test]
    fn rebase_preserves_independent_debits_indexes_and_rejects_dependencies() {
        let mut proof = RestoredTrstState::from_engine(super::super::tests::fixture()).unwrap();
        proof.retain_portfolio_indexes().unwrap();
        let base = proof.engine().clone();
        let a = WalletAddress::new("brst_owner_1");
        let b = WalletAddress::new("brst_owner_2");
        let mut first = base.clone();
        first.debit_wallet_with_provenance(&a, &TxHash::new([1; 32]), 7);
        let current = proof.retained_holdings_successor(&base, &first).unwrap().unwrap();
        let mut second = base.clone();
        second.debit_wallet_with_provenance(&b, &TxHash::new([2; 32]), 11);
        let result = current.rebase_payment(&base, &second, &[b.clone()]).unwrap().unwrap();
        assert_eq!(result.engine().transferable_balance_snapshot(&a), Some(43));
        assert_eq!(result.engine().transferable_balance_snapshot(&b), Some(39));
        assert_eq!(current.engine().transferable_balance_snapshot(&b), Some(50));
        assert!(result.engine().wallets[&a].shares_storage_with(&current.engine().wallets[&a]));
        let restored = RestoredTrstState::from_engine(result.engine().clone()).unwrap();
        assert_eq!(result.engine.origin_holders, restored.engine.origin_holders);
        assert_eq!(result.engine.revoked_holders, restored.engine.revoked_holders);
        assert_eq!(result.token_revisions, restored.token_revisions);
        assert!(current.rebase_payment(&base, &second, &[a.clone()]).is_err());
        second.wallets.remove(&a);
        assert!(current.rebase_payment(&base, &second, &[]).is_err());
        assert!(current.engine().wallets.contains_key(&a));
        let mut changed = base.clone();
        changed.set_expiry_period(base.expiry_secs + 1, Timestamp::new(100)).unwrap();
        assert!(proof.rebase_payment(&base, &changed, &[]).is_err());
        let changed = RestoredTrstState::from_engine(changed).unwrap();
        assert!(changed.rebase_payment(&base, &base, &[]).is_err());
        let mut changed = base.clone();
        changed.revoke_by_originator(&a);
        assert!(proof.rebase_payment(&base, &changed, &[]).is_err());
        let changed = RestoredTrstState::from_engine(changed).unwrap();
        assert!(changed.rebase_payment(&base, &base, &[]).is_err());
        let raw = super::super::tests::fixture();
        let raw_proof = RestoredTrstState::from_engine(raw.clone()).unwrap();
        let mut raw_after = raw.clone();
        raw_after.debit_wallet_with_provenance(&b, &TxHash::new([2;32]), 1);
        assert!(raw_proof.rebase_payment(&raw, &raw_after, &[b]).unwrap().is_none());
    }
}
