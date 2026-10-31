use super::{holding_rows::indexed::IndexedPortfolio, TrstToken};
use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{ops::{Deref, DerefMut}, sync::OnceLock};

/// Serialized portfolio fields, also exposed through mutable vector access.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalPortfolio {
    pub tokens: Vec<TrstToken>,
    pub cached_transferable: u128,
    pub earliest_expiry: Option<Timestamp>,
}

/// A portfolio backed by either a token vector or persistent holding indexes.
/// Reading vector fields materializes and caches t holdings using O(t log t)
/// lookup work and O(t) output space.
/// Mutable vector access discards the index; indexed operations avoid this copy.
pub struct WalletPortfolio {
    canonical: OnceLock<CanonicalPortfolio>,
    indexed: Option<IndexedPortfolio>,
}

impl WalletPortfolio {
    pub fn from_parts(
        tokens: Vec<TrstToken>, cached_transferable: u128, earliest_expiry: Option<Timestamp>,
    ) -> Self {
        CanonicalPortfolio { tokens, cached_transferable, earliest_expiry }.into()
    }

    pub fn into_canonical(self) -> CanonicalPortfolio {
        self.canonical.into_inner().unwrap_or_else(||
            self.indexed.expect("portfolio representation").to_portfolio().into_canonical())
    }

    /// Borrow every retained holding, including expired and revoked tokens,
    /// without materializing the canonical vector. Iteration order is unspecified.
    pub fn iter_tokens(&self) -> impl Iterator<Item = &TrstToken> {
        self.canonical.get().filter(|_| self.indexed.is_none()).into_iter()
            .flat_map(|portfolio| portfolio.tokens.iter())
            .chain(self.indexed.iter().flat_map(IndexedPortfolio::iter_tokens))
    }

    /// Borrow canonical token order without materializing an indexed portfolio.
    pub(super) fn iter_canonical_tokens(&self) -> impl Iterator<Item = &TrstToken> {
        self.canonical.get().filter(|_| self.indexed.is_none()).into_iter()
            .flat_map(|canonical| {
                let CanonicalPortfolio { tokens, cached_transferable: _, earliest_expiry: _ } = canonical;
                tokens.iter()
            })
            .chain(self.indexed.iter().flat_map(IndexedPortfolio::iter_canonical_tokens))
    }

    pub(super) fn from_indexed(indexed: IndexedPortfolio) -> Self {
        Self { canonical: OnceLock::new(), indexed: Some(indexed) }
    }

    pub(super) fn indexed(&self) -> Option<&IndexedPortfolio> { self.indexed.as_ref() }

    pub(super) fn cached_balance(&self) -> u128 {
        self.indexed.as_ref().map_or_else(
            || self.canonical.get().expect("raw portfolio").cached_transferable,
            IndexedPortfolio::cached_balance)
    }

    pub(super) fn cached_expiry(&self) -> Option<Timestamp> {
        self.indexed.as_ref().map_or_else(
            || self.canonical.get().expect("raw portfolio").earliest_expiry,
            IndexedPortfolio::cached_expiry)
    }

    /// Insert the whole batch into an indexed portfolio, or leave it unchanged.
    /// False asks the caller to use the vector path, which also accepts duplicate IDs.
    pub(super) fn insert_indexed(
        &mut self, holder: &WalletAddress, expiry_secs: u64, tokens: &[TrstToken],
    ) -> bool {
        if self.indexed.is_none() { return false; }
        self.insert_received(holder, expiry_secs, tokens)
    }

    pub(super) fn insert_received(
        &mut self, holder: &WalletAddress, expiry_secs: u64, tokens: &[TrstToken],
    ) -> bool {
        // Convert an empty vector without scanning old holdings. Publish the
        // index only after the whole incoming batch passes validation.
        let mut initial = if self.indexed.is_none()
            && self.canonical.get().is_some_and(|cut| cut.tokens.is_empty())
        {
            IndexedPortfolio::from_portfolio(holder.clone(), self, expiry_secs).ok()
        } else { None };
        let Some(root) = self.indexed.as_mut().or(initial.as_mut())
            .filter(|root| root.matches_context(holder, expiry_secs)) else { return false };
        if root.insert_batch(tokens).is_err() { return false; }
        if let Some(root) = initial { self.indexed = Some(root); }
        self.canonical.take();
        true
    }

    pub(super) fn validated_index(
        &self, holder: &WalletAddress, expiry_secs: u64,
    ) -> Result<IndexedPortfolio, String> {
        if let Some(indexed) = &self.indexed {
            if indexed.matches_context(holder, expiry_secs) { return Ok(indexed.clone()); }
        }
        IndexedPortfolio::from_portfolio(holder.clone(), self, expiry_secs)
    }
}

impl From<CanonicalPortfolio> for WalletPortfolio {
    fn from(canonical: CanonicalPortfolio) -> Self {
        Self { canonical: OnceLock::from(canonical), indexed: None }
    }
}
impl Default for WalletPortfolio {
    fn default() -> Self { CanonicalPortfolio::default().into() }
}
impl Clone for WalletPortfolio {
    fn clone(&self) -> Self {
        match &self.indexed {
            Some(indexed) => Self::from_indexed(indexed.clone()),
            None => self.deref().clone().into(),
        }
    }
}
impl Deref for WalletPortfolio {
    type Target = CanonicalPortfolio;
    fn deref(&self) -> &Self::Target {
        self.canonical.get_or_init(||
            self.indexed.as_ref().expect("portfolio representation").to_portfolio().into_canonical())
    }
}
impl DerefMut for WalletPortfolio {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let _ = <Self as Deref>::deref(self);
        self.indexed = None;
        self.canonical.get_mut().expect("materialized portfolio")
    }
}
impl PartialEq for WalletPortfolio {
    fn eq(&self, other: &Self) -> bool {
        match (&self.indexed, &other.indexed) {
            (Some(a), Some(b)) => a.same_canonical_state(b),
            _ => self.deref() == other.deref(),
        }
    }
}
impl Eq for WalletPortfolio {}
impl std::fmt::Debug for WalletPortfolio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.deref().fmt(f) }
}
impl Serialize for WalletPortfolio {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for WalletPortfolio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CanonicalPortfolio::deserialize(deserializer).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::holding_rows, TrstEngine};
    use burst_types::TxHash;

    fn fixture() -> (WalletAddress, TrstEngine) {
        let holder = WalletAddress::new("brst_retained");
        let mut engine = TrstEngine::with_expiry(1000);
        for n in 1..=8 {
            let mut token = engine.mint(TxHash::new([n; 32]), holder.clone(), 10,
                holder.clone(), Timestamp::new(100 + n as u64)).unwrap();
            token.origin = TxHash::new([1; 32]);
            engine.track_token(token);
        }
        (holder, engine)
    }

    #[test]
    fn new_holder_reverse_deliveries_keep_indexed_order_and_prior_cuts() {
        for now in [0, 2000] {
            let holder = WalletAddress::new("brst_new_holder");
            let mut engine = TrstEngine::with_expiry(1000);
            let revoked = std::collections::BTreeMap::new();
            let mut expected = Vec::new();
            for n in 0..128u8 {
                let token = engine.mint(TxHash::new([n + 1; 32]), holder.clone(), 3,
                    holder.clone(), Timestamp::new(10 + u64::from(128 - n) / 2)).unwrap();
                let old = engine.get_portfolio(&holder).cloned();
                if n % 4 == 0 {
                    if let Some(portfolio) = engine.get_portfolio(&holder) {
                        let _ = bincode::serialize(portfolio).unwrap();
                        assert!(portfolio.canonical.get().is_some());
                    }
                }
                engine.receive_settled_token(token.clone(), &revoked, Timestamp::new(now)).unwrap();
                let mut token = token;
                if now == 2000 { token.state = burst_types::TrstState::Expired; }
                expected.push(token);
                expected.sort_by_key(|token| token.origin_timestamp);
                let portfolio = engine.get_portfolio(&holder).unwrap();
                assert!(portfolio.indexed.is_some() && portfolio.canonical.get().is_none());
                assert_eq!(portfolio.iter_canonical_tokens().cloned().collect::<Vec<_>>(), expected);
                assert_eq!(portfolio.cached_balance(), if now == 0 { 3 * (u128::from(n) + 1) } else { 0 });
                if let Some(old) = old {
                    assert_eq!(old.iter_tokens().count(), usize::from(n));
                    assert!(old.canonical.get().is_none());
                }
            }
            let expected = CanonicalPortfolio {
                earliest_expiry: if now == 0 {
                    Some(expected[0].earliest_expiry(1000))
                } else { None },
                tokens: expected, cached_transferable: if now == 0 { 384 } else { 0 },
            };
            assert_eq!(bincode::serialize(engine.get_portfolio(&holder).unwrap()).unwrap(),
                bincode::serialize(&expected).unwrap());
        }
    }

    #[test]
    fn empty_index_conversion_failure_keeps_canonical_fallback_untouched() {
        let (holder, engine) = fixture();
        assert!(engine.get_portfolio(&holder).unwrap().indexed.is_none());
        let token = engine.get_portfolio(&holder).unwrap().tokens[0].clone();
        let empty = WalletPortfolio::default();
        let mut candidate = empty.clone();
        assert!(!candidate.insert_received(&holder, 1000, &[token.clone(), token.clone()]));
        assert!(candidate.indexed.is_none());
        assert_eq!(candidate, empty);
        let mut wrong_holder = token.clone();
        wrong_holder.holder = WalletAddress::new("brst_other");
        assert!(!candidate.insert_received(&holder, 1000, &[token.clone(), wrong_holder]));
        assert!(candidate.indexed.is_none());
        assert_eq!(candidate, empty);
        candidate.cached_transferable = 1;
        let corrupt = candidate.clone();
        assert!(!candidate.insert_received(&holder, 1000, &[token]));
        assert_eq!(candidate, corrupt);
    }

    #[test]
    fn borrowed_holdings_visit_all_states_without_materializing_canonical_vectors() {
        let (holder, engine) = fixture();
        let raw = engine.get_portfolio(&holder).unwrap().clone().into_canonical();
        let raw = WalletPortfolio::from(raw);
        let root = raw.validated_index(&holder, 1000).unwrap();
        let (revoked, _) = root.with_revoked_origin(&TxHash::new([1; 32])).unwrap();
        let (expired, _) = root.with_expired_at(Timestamp::new(2000)).unwrap();
        let encoded = |portfolio: &WalletPortfolio| {
            portfolio.iter_tokens().map(|token| (token.id, bincode::serialize(token).unwrap()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(encoded(&raw).len(), 8);
        for root in [root, revoked, expired] {
            let expected = root.to_portfolio();
            let retained = WalletPortfolio::from_indexed(root);
            assert!(retained.canonical.get().is_none());
            assert_eq!(encoded(&retained), encoded(&expected));
            assert!(retained.iter_canonical_tokens().eq(expected.iter_canonical_tokens()));
            assert_eq!(retained.iter_tokens().count(), 8);
            for token in retained.iter_tokens() {
                assert!(std::ptr::eq(token, retained.indexed().unwrap().get(&token.id).unwrap()));
            }
            assert!(retained.canonical.get().is_none());
            let mut left = TrstEngine::with_expiry(1000);
            left.wallets.insert(holder.clone(), expected.into());
            let mut right = TrstEngine::with_expiry(1000);
            right.wallets.insert(holder.clone(), retained.into());
            assert!(left.wallet_snapshot_eq_at_with_poll(&right, 500, Timestamp::new(1000), &|_| Ok(())).unwrap());
            assert!(right.wallets[&holder].canonical.get().is_none());
        }
    }

    #[test]
    fn canonical_bytes_and_legacy_mutation_invalidation_are_preserved() {
        let (holder, engine) = fixture();
        let original = engine.get_portfolio(&holder).unwrap();
        let canonical = original.clone().into_canonical();
        let bytes = bincode::serialize(&canonical).unwrap();
        let root = original.validated_index(&holder, 1000).unwrap();
        let retained = WalletPortfolio::from_indexed(root);
        assert!(retained.canonical.get().is_none());
        assert_eq!(bincode::serialize(&retained).unwrap(), bytes);
        let mut candidate = retained.clone();
        assert!(candidate.canonical.get().is_none());
        candidate.tokens[0].amount += 1;
        candidate.cached_transferable += 1;
        assert!(candidate.indexed.is_none());
        assert!(retained.indexed.is_some());
        assert_eq!(retained.tokens[0].amount, 10);
        let restored: WalletPortfolio = bincode::deserialize(&bytes).unwrap();
        assert!(restored.indexed.is_none());
        assert_eq!(restored, retained);
    }

    #[test]
    fn repeated_engine_debits_and_stable_deltas_do_not_materialize_the_vec() {
        let (holder, mut engine) = fixture();
        let mut reference = engine.clone();
        let origin = TxHash::new([1; 32]);
        engine.debit_wallet_with_provenance(&holder, &origin, 1);
        reference.debit_unindexed_portfolio(&holder, &origin, 1);
        let before = engine.clone();
        let old = before.get_portfolio(&holder).unwrap();
        assert!(old.indexed.is_some() && old.canonical.get().is_none());
        engine.debit_wallet_with_provenance(&holder, &origin, 2);
        reference.debit_unindexed_portfolio(&holder, &origin, 2);
        let new = engine.get_portfolio(&holder).unwrap();
        assert!(new.indexed.is_some() && new.canonical.get().is_none());
        assert_ne!(old, new);
        let mut count = 0;
        holding_rows::visit_delta(&holder, Some(old), Some(new), |e| e, |_, _| {
            count += 1; Ok(())
        }).unwrap();
        assert_eq!(count, 2);
        assert!(old.canonical.get().is_none() && new.canonical.get().is_none());
        assert_eq!(engine.save_wallets(), reference.save_wallets());
    }

    #[test]
    fn raw_corruption_cannot_reuse_a_retained_validation_proof() {
        let (holder, mut engine) = fixture();
        engine.debit_wallet(&holder, &TxHash::new([1; 32]), 1);
        let old = engine.get_portfolio(&holder).unwrap().clone();
        let mut new = old.clone();
        let duplicate = new.tokens[0].id;
        new.tokens[1].id = duplicate;
        assert!(new.indexed.is_none());
        assert!(new.validated_index(&holder, 1000).is_err());
        let mut callbacks = 0;
        assert!(holding_rows::visit_delta(&holder, Some(&old), Some(&new), |e| e, |_, _| {
            callbacks += 1; Ok(())
        }).is_err());
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn indexed_balance_admission_and_expiry_avoid_legacy_materialization() {
        let (holder, mut retained) = fixture();
        let mut raw = retained.clone();
        let origin = TxHash::new([1; 32]);
        retained.debit_wallet(&holder, &origin, 1);
        raw.debit_unindexed_portfolio(&holder, &origin, 1);
        assert_eq!(retained.transferable_balance_snapshot(&holder), raw.transferable_balance_snapshot(&holder));
        for now in [0, 1100, 1101, 1104, u64::MAX] {
            let now = Timestamp::new(now);
            let baseline = retained.clone();
            for origin in [origin, TxHash::new([99; 32])] {
                assert_eq!(retained.candidate_balances(&holder, &origin, now),
                    raw.candidate_balances(&holder, &origin, now));
            }
            assert!(retained.shares_wallet_snapshot_with(&baseline));
            assert!(retained.get_portfolio(&holder).unwrap().canonical.get().is_none());
            let mut candidate = retained.clone();
            let mut expected = raw.clone();
            assert_eq!(candidate.transferable_balance(&holder, now),
                expected.transferable_balance(&holder, now));
            assert_eq!(candidate.origin_transferable(&holder, &origin, now),
                expected.origin_transferable(&holder, &origin, now));
            let portfolio = candidate.get_portfolio(&holder).unwrap();
            assert!(portfolio.indexed.is_some() && portfolio.canonical.get().is_none());
            // Export a detached portfolio: serializing the shared engine cut would
            // populate retained's OnceLock too when an idle flush shares that cut.
            let exported = candidate.get_portfolio(&holder).unwrap().indexed().unwrap().to_portfolio();
            assert_eq!(bincode::serialize(&exported).unwrap(),
                bincode::serialize(expected.get_portfolio(&holder).unwrap()).unwrap());
            let mut candidate = retained.clone();
            let mut expected = raw.clone();
            assert_eq!(candidate.flush_all_expired_by_wallet(now), expected.flush_all_expired_by_wallet(now));
            assert!(candidate.get_portfolio(&holder).unwrap().canonical.get().is_none());
            // Export a detached portfolio: serializing the shared engine cut would
            // populate retained's OnceLock too when an idle flush shares that cut.
            let exported = candidate.get_portfolio(&holder).unwrap().indexed().unwrap().to_portfolio();
            assert_eq!(bincode::serialize(&exported).unwrap(),
                bincode::serialize(expected.get_portfolio(&holder).unwrap()).unwrap());
        }
    }

    #[test]
    fn incoming_tokens_and_settled_batches_preserve_retained_cuts() {
        use burst_types::TrstState;
        use std::collections::BTreeMap;
        for timestamp in [99, 104, 108, 200] {
            for now in [0, 1104] {
                let (holder, mut retained) = fixture();
                let mut raw = retained.clone();
                retained.debit_wallet(&holder, &TxHash::new([1; 32]), 1);
                raw.debit_unindexed_portfolio(&holder, &TxHash::new([1; 32]), 1);
                let before = retained.clone();
                let mut token = retained.mint(TxHash::new([90; 32]), holder.clone(), 3,
                    holder.clone(), Timestamp::new(timestamp)).unwrap();
                raw.mint(TxHash::new([90; 32]), holder.clone(), 3,
                    holder.clone(), Timestamp::new(timestamp)).unwrap();
                for (n, state) in [TrstState::Active, TrstState::Expired, TrstState::Revoked]
                    .into_iter().enumerate()
                {
                    token.id = TxHash::new([100 + n as u8; 32]);
                    token.state = state;
                    token.revoked_origin = (state == TrstState::Revoked).then_some(token.origin);
                    retained.track_token(token.clone());
                    raw.track_token(token.clone());
                }
                token.state = TrstState::Active;
                token.revoked_origin = None;
                let portions: Vec<_> = (110..113).map(|id| {
                    let mut portion = token.clone();
                    portion.id = TxHash::new([id; 32]);
                    portion
                }).collect();
                let revoked = BTreeMap::new();
                retained.receive_settled_batch(portions.iter().cloned().map(|t| (t, &revoked)),
                    Timestamp::new(now)).unwrap();
                raw.receive_settled_batch(portions.into_iter().map(|t| (t, &revoked)),
                    Timestamp::new(now)).unwrap();
                let old = before.get_portfolio(&holder).unwrap();
                let new = retained.get_portfolio(&holder).unwrap();
                assert!(old.canonical.get().is_none() && new.canonical.get().is_none());
                let mut changed = 0;
                holding_rows::visit_delta(&holder, Some(old), Some(new), |e| e, |_, _| {
                    changed += 1; Ok(())
                }).unwrap();
                assert!(changed > 0);
                assert!(old.canonical.get().is_none() && new.canonical.get().is_none());
                assert_eq!(retained.save_wallets(), raw.save_wallets());
            }
        }
    }

    #[test]
    fn failed_indexed_receive_keeps_legacy_duplicate_behavior_without_partial_insert() {
        let (holder, mut retained) = fixture();
        let mut raw = retained.clone();
        retained.debit_wallet(&holder, &TxHash::new([1; 32]), 1);
        raw.debit_unindexed_portfolio(&holder, &TxHash::new([1; 32]), 1);
        let portfolio = retained.get_portfolio(&holder).unwrap();
        let duplicate = portfolio.indexed().unwrap().get(&TxHash::new([2; 32])).unwrap().clone();
        let mut fresh = duplicate.clone();
        fresh.id = TxHash::new([99; 32]);
        let mut detached = portfolio.clone();
        assert!(!detached.insert_indexed(&holder, 1000, &[fresh.clone(), duplicate.clone()]));
        assert_eq!(&detached, portfolio);
        assert!(detached.canonical.get().is_none());
        let empty = std::collections::BTreeMap::new();
        retained.receive_settled_batch([(fresh.clone(), &empty), (duplicate.clone(), &empty)],
            Timestamp::new(0)).unwrap();
        raw.receive_settled_batch([(fresh, &empty), (duplicate, &empty)], Timestamp::new(0)).unwrap();
        assert!(retained.get_portfolio(&holder).unwrap().indexed().is_none());
        assert_eq!(retained.save_wallets(), raw.save_wallets());
    }
    #[test]
    fn retained_counters_match_raw_clock_rules_without_materialization() {
        use burst_types::TrstState;
        let (holder, engine) = fixture();
        let mut root = engine.get_portfolio(&holder).unwrap().validated_index(&holder, 1000).unwrap();
        for (n, state) in [(2, TrstState::Expired), (3, TrstState::Revoked), (4, TrstState::Pending)] {
            let mut token = root.get(&TxHash::new([n; 32])).unwrap().clone();
            token.state = state;
            token.revoked_origin = (state == TrstState::Revoked).then_some(token.origin);
            root = root.with_replaced(token).unwrap();
        }
        for period in [1000, 0, u64::MAX, 50, 200] {
            root = root.with_expiry_period(period, Timestamp::new(150)).unwrap();
            for step in 0..3 {
                if step == 1 { root = root.with_removed(&TxHash::new([1; 32])).unwrap(); }
                if step == 2 {
                    let mut token = root.get(&TxHash::new([2; 32])).unwrap().clone();
                    token.id = TxHash::new([1; 32]); token.amount = 0;
                    root = root.with_inserted(token).unwrap();
                }
                let mut retained = TrstEngine::with_expiry(period);
                let mut raw = retained.clone();
                retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root.clone()).into());
                raw.wallets.insert(holder.clone(), root.to_portfolio().into());
                for at in [0, 100, 101, 150, 1100, 1101, 1104, u64::MAX, 0] {
                    assert_eq!(retained.token_counter_window(&holder, Timestamp::new(at)),
                        raw.token_counter_window(&holder, Timestamp::new(at)));
                    assert!(retained.get_portfolio(&holder).unwrap().canonical.get().is_none());
                }
            }
        }
    }

    #[test]
    fn retained_counter_overflow_preserves_legacy_error_and_recovers_after_removal() {
        use burst_types::TrstState;
        let (holder, engine) = fixture();
        let mut portfolio = engine.get_portfolio(&holder).unwrap().clone();
        portfolio.tokens.truncate(2);
        for token in &mut portfolio.tokens { token.state = TrstState::Expired; }
        portfolio.tokens[0].amount = u128::MAX;
        portfolio.tokens[1].amount = 1;
        portfolio.cached_transferable = 0;
        portfolio.earliest_expiry = None;
        let root = portfolio.validated_index(&holder, 1000).unwrap();
        let mut retained = TrstEngine::with_expiry(1000);
        let mut raw = retained.clone();
        retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root.clone()).into());
        raw.wallets.insert(holder.clone(), portfolio.into());
        for at in [0, 1101, 1102] {
            assert_eq!(retained.token_counter_window(&holder, Timestamp::new(at)),
                raw.token_counter_window(&holder, Timestamp::new(at)));
        }
        let root = root.with_removed(&TxHash::new([1; 32])).unwrap();
        retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
        assert_eq!(retained.token_counters(&holder, Timestamp::new(1102)), Ok((1, 0)));
        assert!(retained.get_portfolio(&holder).unwrap().canonical.get().is_none());
    }

    #[test]
    fn retained_revocation_and_restoration_match_raw_without_materializing() {
        let holder = WalletAddress::new("brst_holder");
        let owners = [WalletAddress::new("brst_origin_a"), WalletAddress::new("brst_origin_b")];
        let mut raw = TrstEngine::with_expiry(1000);
        for n in 1..=12u8 {
            let token = raw.mint(TxHash::new([n; 32]), holder.clone(), n as u128,
                owners[(n as usize - 1) % 2].clone(), Timestamp::new(100)).unwrap();
            raw.track_token(token);
        }
        let ids = [TxHash::new([1; 32]), TxHash::new([2; 32])];
        let inputs = raw.wallets[&holder].select_merge_inputs(&ids).unwrap();
        let merged = raw.merge(&inputs, holder.clone(), TxHash::new([99; 32]), Timestamp::new(100)).unwrap();
        raw.bulk_untrack(&holder, &ids.into_iter().collect());
        raw.track_token(merged);
        let mut retained = raw.clone();
        let root = retained.wallets[&holder].validated_index(&holder, 1000).unwrap();
        retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
        let original = retained.clone();
        for (owner, revoke, now) in [(0, true, 100), (0, true, 100), (0, false, 100),
            (1, true, 100), (0, true, 100), (1, false, 1200), (0, false, 1200),
            (0, true, 1200), (0, false, 100)] {
            if revoke {
                let (actual_events, actual_pending) = retained.revoke_with_pending_effects(&owners[owner]);
                let (expected_events, expected_pending) = raw.revoke_with_pending_effects(&owners[owner]);
                assert_eq!(format!("{:?}", actual_events), format!("{:?}", expected_events));
                assert_eq!(actual_pending.len(), expected_pending.len());
                for (actual, expected) in actual_pending.iter().zip(&expected_pending) {
                    assert_eq!(actual.origin, expected.origin);
                    assert_eq!(actual.taints, expected.taints);
                }
            } else {
                assert_eq!(format!("{:?}", retained.restore_by_originator(&owners[owner], Timestamp::new(now)).unwrap()),
                    format!("{:?}", raw.restore_by_originator(&owners[owner], Timestamp::new(now)).unwrap()));
            }
            let actual = retained.get_portfolio(&holder).unwrap();
            assert!(actual.indexed.is_some() && actual.canonical.get().is_none());
            assert_eq!(bincode::serialize(&actual.clone()).unwrap(),
                bincode::serialize(raw.get_portfolio(&holder).unwrap()).unwrap());
            assert_eq!(retained.merger_graph.to_bytes(), raw.merger_graph.to_bytes());
            assert_eq!(retained.origin_holders, raw.origin_holders);
            assert_eq!(retained.revoked_holders, raw.revoked_holders);
            for clock in [100, 1100, 1200] {
                assert_eq!(retained.token_counter_window(&holder, Timestamp::new(clock)),
                    raw.token_counter_window(&holder, Timestamp::new(clock)));
            }
            assert!(actual.canonical.get().is_none());
        }
        assert!(original.get_portfolio(&holder).unwrap().canonical.get().is_none());
    }

    #[test]
    fn retained_single_and_bulk_removal_match_raw_without_materializing() {
        use burst_types::TrstState;
        let (holder, mut raw) = fixture();
        {
            let portfolio = raw.wallets.get_mut(&holder).unwrap();
            portfolio.tokens[0].amount = 0;
            portfolio.tokens[1].state = TrstState::Expired;
            portfolio.tokens[3].state = TrstState::Pending;
            for n in [2, 5] {
                portfolio.tokens[n].state = TrstState::Revoked;
                portfolio.tokens[n].revoked_origin = Some(TxHash::new([3; 32]));
            }
            portfolio.recompute_transferable(Timestamp::EPOCH, 1000);
            portfolio.recompute_earliest_expiry(1000);
        }
        raw.rebuild_indexes();
        let mut retained = raw.clone();
        let root = retained.wallets[&holder].validated_index(&holder, 1000).unwrap();
        retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
        let original = retained.clone();
        for (bulk, ids) in [(true, vec![1]), (false, vec![3]),
            (true, vec![2, 4, 6, 99]), (true, vec![5, 7, 8]), (true, vec![])] {
            let ids: std::collections::HashSet<_> = ids.into_iter().map(|n| TxHash::new([n; 32])).collect();
            if bulk {
                retained.bulk_untrack(&holder, &ids);
                raw.bulk_untrack(&holder, &ids);
            } else {
                let id = ids.iter().next().unwrap();
                retained.untrack_token(&holder, id);
                raw.untrack_token(&holder, id);
            }
            let actual = retained.get_portfolio(&holder).unwrap();
            assert!(actual.indexed.is_some() && actual.canonical.get().is_none());
            assert_eq!(bincode::serialize(&actual.clone()).unwrap(),
                bincode::serialize(raw.get_portfolio(&holder).unwrap()).unwrap());
            assert_eq!(retained.origin_holders, raw.origin_holders);
            assert_eq!(retained.revoked_holders, raw.revoked_holders);
            assert_eq!(retained.wallet_origins, original.wallet_origins);
            for now in [0, 1101, 1105, u64::MAX] {
                assert_eq!(retained.token_counter_window(&holder, Timestamp::new(now)),
                    raw.token_counter_window(&holder, Timestamp::new(now)));
            }
            assert!(actual.canonical.get().is_none());
        }
        assert!(retained.origin_holders.is_empty() && retained.revoked_holders.is_empty());
        assert_eq!(original.wallets[&holder].indexed().unwrap().get(&TxHash::new([1; 32])).unwrap().amount, 0);
        assert!(original.get_portfolio(&holder).unwrap().canonical.get().is_none());
    }

    #[test]
    fn bulk_removal_refreshes_deadline_when_only_zero_active_value_is_removed() {
        let (holder, mut engine) = fixture();
        engine.wallets.get_mut(&holder).unwrap().tokens[0].amount = 0;
        engine.wallets.get_mut(&holder).unwrap().cached_transferable = 70;
        engine.bulk_untrack(&holder, &[TxHash::new([1; 32])].into_iter().collect());
        let portfolio = engine.get_portfolio(&holder).unwrap();
        assert_eq!(portfolio.cached_transferable, 70);
        assert_eq!(portfolio.earliest_expiry, Some(Timestamp::new(1102)));
    }

    #[test]
    fn retained_merge_selection_matches_raw_at_caps_ties_and_saturated_deadlines() {
        use burst_types::TrstState;
        for count in [0usize, 1, 255, 256, 257, 600] {
            let (holder, engine) = fixture();
            let base = engine.get_portfolio(&holder).unwrap().tokens[0].clone();
            let tokens: Vec<_> = (0..count).map(|n| {
                let mut token = base.clone();
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(n as u64).to_be_bytes());
                token.id = TxHash::new(id);
                token.origin_timestamp = Timestamp::new(n as u64);
                token.effective_origin_timestamp = Timestamp::new(match n % 7 {
                    0 => u64::MAX, 1 => u64::MAX - 2, 2 => 0,
                    _ => (count - n) as u64 % 31,
                });
                token.state = match n % 11 {
                    0 => TrstState::Revoked, 1 => TrstState::Expired,
                    2 => TrstState::Pending, _ => TrstState::Active,
                };
                token.revoked_origin = (token.state == TrstState::Revoked).then_some(token.origin);
                token
            }).collect();
            for governed in [0, 1, 1000, u64::MAX] {
                let balance = tokens.iter().filter(|t| t.state == TrstState::Active).map(|t| t.amount).sum();
                let mut raw = WalletPortfolio::from_parts(tokens.clone(), balance, None);
                raw.recompute_earliest_expiry(governed);
                let root = raw.validated_index(&holder, governed).unwrap();
                let retained = WalletPortfolio::from_indexed(root);
                for period in [0, 1, 1000, u64::MAX] {
                    for now in [0, 1, 20, 1000, u64::MAX - 1, u64::MAX] {
                        assert_eq!(retained.select_expiry_merge_group(Timestamp::new(now), period),
                            raw.select_expiry_merge_group(Timestamp::new(now), period),
                            "count={count} governed={governed} period={period} now={now}");
                    }
                }
                let mut ids: Vec<_> = tokens.iter().rev().take(3).map(|t| t.id).collect();
                if let Some(first) = ids.first().copied() { ids.push(first); }
                assert_eq!(retained.select_merge_inputs(&ids), raw.select_merge_inputs(&ids));
                ids.push(TxHash::new([255; 32]));
                ids.push(TxHash::new([254; 32]));
                assert_eq!(retained.select_merge_inputs(&ids), raw.select_merge_inputs(&ids));
                assert!(retained.canonical.get().is_none());
            }
        }
    }

    #[test]
    fn retained_merge_selection_tracks_mutation_expiry_and_period_rebuilds() {
        let (holder, mut raw) = fixture();
        let mut retained = raw.clone();
        let root = retained.wallets[&holder].validated_index(&holder, 1000).unwrap();
        retained.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
        for (period, now) in [(1000, 0), (1, 1000), (u64::MAX, 1000), (0, 105), (1000, 0)] {
            raw.set_expiry_period(period, Timestamp::new(now)).unwrap();
            retained.set_expiry_period(period, Timestamp::new(now)).unwrap();
            let token = raw.mint(TxHash::new([90; 32]), holder.clone(), 2, holder.clone(), Timestamp::new(50)).unwrap();
            raw.track_token(token.clone());
            retained.track_token(token);
            for at in [0, 100, 1105, u64::MAX - 1, u64::MAX] {
                assert_eq!(retained.wallets[&holder].select_expiry_merge_group(Timestamp::new(at), period),
                    raw.wallets[&holder].select_expiry_merge_group(Timestamp::new(at), period));
            }
            raw.untrack_token(&holder, &TxHash::new([90; 32]));
            retained.untrack_token(&holder, &TxHash::new([90; 32]));
            assert!(retained.wallets[&holder].canonical.get().is_none());
        }
    }

    #[test]
    fn governed_resurrection_overflow_keeps_all_wallets_and_period_atomic() {
        use burst_types::TrstState;
        let first = WalletAddress::new("brst_a");
        let last = WalletAddress::new("brst_z");
        let mut raw = TrstEngine::with_expiry(10);
        for (n, holder, amount) in [(1, &first, 1), (2, &last, u128::MAX), (3, &last, 1)] {
            let mut token = raw.mint(TxHash::new([n; 32]), holder.clone(), amount,
                holder.clone(), Timestamp::new(100)).unwrap();
            token.state = TrstState::Expired;
            raw.track_token(token);
        }
        // This is a structurally valid historical portfolio, not an overflowing
        // active balance smuggled past snapshot validation.
        let bytes = raw.save_wallets();
        TrstEngine::load_wallets(&bytes, 10).unwrap();
        for indexed in [false, true] {
            let mut engine = raw.clone();
            if indexed {
                for holder in [&first, &last] {
                    let root = engine.wallets[holder].validated_index(holder, 10).unwrap();
                    engine.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
                }
            }
            let before = engine.clone();
            assert_eq!(engine.set_expiry_period(1000, Timestamp::new(150)).unwrap_err(),
                "TRST holding balance overflow");
            assert_eq!(engine.expiry_secs, 10);
            assert!(engine.shares_wallet_snapshot_with(&before));
            assert_eq!(engine.save_wallets(), bytes);
            assert_eq!(engine.wallet_origins, before.wallet_origins);
            // The representable boundary still succeeds; no saturation or lost value.
            engine.untrack_token(&last, &TxHash::new([3; 32]));
            engine.set_expiry_period(1000, Timestamp::new(150)).unwrap();
            assert_eq!(engine.expiry_secs, 1000);
            assert_eq!(engine.transferable_balance_snapshot(&first), Some(1));
            assert_eq!(engine.transferable_balance_snapshot(&last), Some(u128::MAX));
            assert_eq!(engine.wallets[&last].cached_expiry(), Some(Timestamp::new(1100)));
        }
    }

    #[test]
    fn reversal_balance_boundaries_are_atomic_for_raw_and_indexed_portfolios() {
        use burst_types::BlockHash;
        let holder = WalletAddress::new("brst_holder");
        let owner = WalletAddress::new("brst_revoked_owner");
        let honest_owner = WalletAddress::new("brst_honest_owner");
        let origin = TxHash::new([91; 32]);
        let cause = BlockHash::new([7; 32]);
        for indexed in [false, true] {
            for outcome in [false, true] {
                // Exact maximum succeeds; overflow fails; expired restoration
                // does not add to the active balance.
                for (amount, now, fails) in [
                    (u128::MAX - 1, 102, false),
                    (u128::MAX, 102, true),
                    (u128::MAX, 1100, false),
                ] {
                    let mut engine = TrstEngine::with_expiry(1000);
                    let token = engine.mint(origin, holder.clone(), amount,
                        owner.clone(), Timestamp::new(100)).unwrap();
                    engine.track_token(token);
                    let origins = [(owner.clone(), origin)];
                    if outcome {
                        engine.invalidate_outcome_origins(cause, &origins).unwrap();
                    } else {
                        engine.revoke_by_originator(&owner);
                    }
                    let token = engine.mint(TxHash::new([92; 32]), holder.clone(),
                        1, honest_owner.clone(), Timestamp::new(101)).unwrap();
                    engine.track_token(token);
                    if indexed {
                        let root = engine.wallets[&holder].validated_index(&holder, 1000).unwrap();
                        engine.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root).into());
                    } else {
                        let _ = <WalletPortfolio as DerefMut>::deref_mut(engine.wallets.get_mut(&holder).unwrap());
                    }
                    let before = engine.clone();
                    let result = if outcome {
                        engine.restore_outcome_origins(cause, &origins, Timestamp::new(now))
                    } else {
                        engine.restore_by_originator(&owner, Timestamp::new(now))
                    };
                    if fails {
                        assert!(result.is_err(), "indexed={indexed}, outcome={outcome}");
                        assert!(engine.shares_wallet_snapshot_with(&before));
                        assert!(engine.merger_graph.shares_storage_with(&before.merger_graph));
                        assert!(engine.origin_holders.shares_storage_with(&before.origin_holders));
                        assert!(engine.revoked_holders.shares_storage_with(&before.revoked_holders));
                        assert_eq!(engine.save_wallets(), before.save_wallets());
                        // Failure preserved the cause/indexes: a later expired
                        // restoration still finds the same holding and succeeds.
                        let retry = if outcome {
                            engine.restore_outcome_origins(cause, &origins, Timestamp::new(1100))
                        } else {
                            engine.restore_by_originator(&owner, Timestamp::new(1100))
                        }.unwrap();
                        assert_eq!(retry.len(), 1);
                    } else {
                        assert_eq!(result.unwrap().len(), 1);
                        assert_eq!(engine.wallets[&holder].cached_balance(),
                            if now == 102 { u128::MAX } else { 1 });
                    }
                    assert!(!engine.merger_graph.is_origin_revoked(&origin));
                    assert!(holding_rows::delta(&holder, Some(&before.wallets[&holder]),
                        Some(&engine.wallets[&holder])).is_ok());
                    TrstEngine::load_wallets(&engine.save_wallets(), 1000).unwrap();
                }
            }
        }
    }

}
