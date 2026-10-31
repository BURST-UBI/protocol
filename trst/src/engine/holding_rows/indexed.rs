//! Persistent holding indexes for one wallet and expiry period.
//! Structural checks cover owner, token IDs, ordering, state tags and cached
//! amounts. Ledger authentication and graph-provenance checks belong to callers.

use super::{holding, validate, Row, TrstToken, WalletPortfolio};
use crate::engine::{ConsumedProvenance, RevocationEvent, RestorationEvent};
use crate::{MergerGraph, Shared, SharedMap};
use burst_types::{Timestamp, TrstBasis, TrstState, TxHash, WalletAddress};
use std::collections::HashMap;
use imbl::{OrdMap, OrdSet};
use std::ops::Bound::{Excluded, Unbounded};

type Order = (Timestamp, u128);
mod counters;

#[derive(Clone, PartialEq, Eq)]
struct Holding {
    order: Order,
    token: Shared<TrstToken>,
    next: Option<TxHash>,
}

/// Updates stage a candidate; errors leave the original portfolio intact.
/// Point changes copy affected tree paths. Exporting t holdings in canonical
/// order performs t token lookups, taking O(t log t) work and O(t) output space.
#[derive(Clone)]
pub struct IndexedPortfolio {
    holder: WalletAddress,
    expiry_secs: u64,
    rows: SharedMap<TxHash, Holding>,
    order: OrdMap<Order, TxHash>,
    active_times: OrdSet<(Timestamp, TxHash)>,
    origins: OrdMap<TxHash, OrdSet<Order>>,
    revoked_origins: OrdMap<TxHash, OrdSet<Order>>,
    // Non-revoked holdings partitioned by merge and definite single-origin basis.
    split_members: OrdMap<(TxHash, Option<TxHash>), OrdSet<Order>>,
    origin_balances: OrdMap<TxHash, u128>,
    balance: u128,
    counters: counters::Counters,
    next_order: u128,
}

impl IndexedPortfolio {
    pub(crate) fn cached_balance(&self) -> u128 { self.balance }

    pub(crate) fn cached_expiry(&self) -> Option<Timestamp> { self.earliest_expiry() }

    pub(crate) fn expiry_period(&self) -> u64 { self.expiry_secs }

    pub(crate) fn counter_window(&self, now: Timestamp) -> Option<((u128, u128), Option<Timestamp>)> {
        self.counters.period_window(now.as_secs(), self.expiry_secs).map(|(totals, next)| (totals, next.map(Timestamp::new)))
    }

    /// Read the candidate clock without changing either root. Only due active
    /// entries need adjustment; inactive and unrelated non-due holdings are skipped.
    /// Each due entry requires a token lookup. Already-expired stored states are
    /// not reactivated by querying an earlier timestamp.
    pub(crate) fn candidate_balances(&self, origin: &TxHash, now: Timestamp) -> (u128, u128) {
        let mut balance = self.balance;
        let mut origin_balance = self.origin_active_balance(origin);
        for (_, id) in self.active_times.iter().take_while(|(at, _)| at.has_expired(self.expiry_secs, now)) {
            let token = self.get(id).expect("indexed active expiry");
            balance -= token.amount;
            if token.origin == *origin { origin_balance -= token.amount; }
        }
        (balance, origin_balance)
    }

    /// Active timestamps are ordered before adding the governed period, so
    /// saturated deadlines retain the canonical effective-time / ID ordering.
    pub(crate) fn select_expiry_merge_group(&self, now: Timestamp, expiry_secs: u64) -> Vec<TrstToken> {
        if now.as_secs() == u64::MAX { return Vec::new(); }
        let lower = match now.as_secs().checked_sub(expiry_secs) {
            Some(last_expired) => Excluded((Timestamp::new(last_expired), TxHash::new([u8::MAX; 32]))),
            None => Unbounded,
        };
        let mut selected = Vec::new();
        let mut cutoff = None;
        for (at, id) in self.active_times.range((lower, Unbounded)).take(crate::engine::MAX_MERGE_SOURCES) {
            let end = *cutoff.get_or_insert_with(|| at.as_secs().saturating_add(expiry_secs / 10));
            if at.as_secs() > end { break; }
            selected.push(self.get(id).expect("indexed active holding").clone());
        }
        selected
    }

    pub(crate) fn matches_context(&self, holder: &WalletAddress, expiry_secs: u64) -> bool {
        self.holder == *holder && self.expiry_secs == expiry_secs
    }

    pub(crate) fn can_diff_with(&self, before: &Self, holder: &WalletAddress) -> bool {
        self.holder == *holder && before.holder == *holder && self.expiry_secs == before.expiry_secs
    }

    pub(crate) fn same_canonical_state(&self, other: &Self) -> bool {
        // Holder/period are local validation context, not canonical fields.
        if self.balance != other.balance || self.earliest_expiry() != other.earliest_expiry()
            || self.rows.len() != other.rows.len()
            || self.order.get_min().map(|(_, id)| id) != other.order.get_min().map(|(_, id)| id) {
            return false;
        }
        self.rows.changed_keys(&other.rows).all(|id| match (self.rows.get(id), other.rows.get(id)) {
            (Some(a), Some(b)) => a.token == b.token && a.next == b.next,
            _ => false,
        })
    }

    /// Cold O(t log t) conversion, retaining input order among equal timestamps.
    /// Check the stored deadline against the supplied period and build indexes.
    /// No clock-based reclassification or graph-provenance validation is performed.
    pub fn from_portfolio(
        holder: WalletAddress,
        portfolio: &WalletPortfolio,
        expiry_secs: u64,
    ) -> Result<Self, String> {
        validate(&holder, portfolio)?;
        let mut result = Self {
            holder, expiry_secs, rows: SharedMap::default(), order: OrdMap::new(),
            active_times: OrdSet::new(), balance: 0,
            counters: counters::Counters::default(),
            origins: OrdMap::new(), revoked_origins: OrdMap::new(),
            origin_balances: OrdMap::new(), split_members: OrdMap::new(),
            next_order: portfolio.tokens.len() as u128,
        };
        for (n, token) in portfolio.tokens.iter().enumerate() {
            let order = (token.origin_timestamp, n as u128);
            result.order.insert(order, token.id);
            result.rows.insert(token.id, Holding {
                order, token: token.clone().into(),
                next: portfolio.tokens.get(n + 1).map(|next| next.id),
            });
            result.add_membership(order, token);
            result.add_token_totals(token)?;
        }
        if result.earliest_expiry() != portfolio.earliest_expiry {
            return Err("TRST holding expiry cache differs from governed period".into());
        }
        Ok(result)
    }

    pub fn get(&self, id: &TxHash) -> Option<&TrstToken> {
        self.rows.get(id).map(|row| &*row.token)
    }

    pub(crate) fn iter_tokens(&self) -> impl Iterator<Item = &TrstToken> {
        self.rows.values().map(|row| &*row.token)
    }

    /// Non-revoked holdings of one origin in canonical portfolio order.
    /// Expired holdings remain members, matching the engine's holder index.
    pub fn origin_tokens<'a>(&'a self, origin: &TxHash) -> impl Iterator<Item = &'a TrstToken> {
        self.origins.get(origin).into_iter().flat_map(|set| set.iter())
            .map(|key| self.get(&self.order[key]).expect("indexed origin holding"))
    }

    fn split_key(token: &TrstToken) -> (TxHash, Option<TxHash>) {
        let single = match token.basis {
            // Keep malformed zero bases in the general bucket so validation
            // still rejects them, even when revoking a different origin.
            TrstBasis::Single(origin) if origin != TxHash::ZERO => Some(origin),
            _ => None,
        };
        (token.origin, single)
    }

    fn split_tokens(&self, merge: TxHash, revoked: TxHash) -> impl Iterator<Item = &TrstToken> {
        let mut general = self.split_members.get(&(merge, None)).into_iter()
            .flat_map(|members| members.iter()).peekable();
        let mut single = self.split_members.get(&(merge, Some(revoked))).into_iter()
            .flat_map(|members| members.iter()).peekable();
        // Both disjoint sets use the original portfolio order, including ties.
        std::iter::from_fn(move || {
            let order = match (general.peek(), single.peek()) {
                (Some(a), Some(b)) if a <= b => general.next(),
                (Some(_), Some(_)) => single.next(),
                (Some(_), None) => general.next(),
                (None, Some(_)) => single.next(),
                (None, None) => None,
            }?;
            Some(self.get(&self.order[order]).expect("indexed split holding"))
        })
    }

    /// Revoked chunks tagged with this burn origin, including merged holdings.
    pub fn revoked_tokens<'a>(&'a self, origin: &TxHash) -> impl Iterator<Item = &'a TrstToken> {
        self.revoked_origins.get(origin).into_iter().flat_map(|set| set.iter())
            .map(|key| self.get(&self.order[key]).expect("indexed revoked holding"))
    }

    /// Active-state aggregate at this cut. Call expiry normalization first when
    /// the agreed clock requires it; this method does not reinterpret local time.
    pub fn origin_active_balance(&self, origin: &TxHash) -> u128 {
        self.origin_balances.get(origin).copied().unwrap_or(0)
    }

    /// Consume active holdings of one origin in canonical order, retaining
    /// separate composition bases exactly as the engine's debit operation does.
    /// Admission must check the requested balance and normalize expiry first.
    /// Like the engine, an oversized request consumes only the available amount.
    /// Returned portions are grouped and sorted by composition basis, not by
    /// consumption order. This does not construct or authorize a send block.
    pub fn with_debited_origin(
        &self,
        origin: &TxHash,
        amount: u128,
    ) -> Result<(Self, Vec<ConsumedProvenance>), String> {
        let mut result = self.clone();
        if amount == 0 { return Ok((result, Vec::new())); }
        let mut remaining = amount;
        let mut portions: HashMap<TrstBasis, ConsumedProvenance> = HashMap::new();
        for token in self.origin_tokens(origin) {
            if remaining == 0 { break; }
            if token.state != TrstState::Active { continue; }
            let take = token.amount.min(remaining);
            match portions.get_mut(&token.basis) {
                Some(portion) => {
                    portion.amount = portion.amount.checked_add(take)
                        .ok_or("TRST consumed provenance overflow")?;
                }
                None => {
                    portions.insert(token.basis, ConsumedProvenance {
                        basis: token.basis, amount: take, origin: token.origin,
                        origin_wallet: token.origin_wallet.clone(),
                        origin_timestamp: token.origin_timestamp,
                        effective_origin_timestamp: token.effective_origin_timestamp,
                    });
                }
            }
            remaining -= take;
            if take == token.amount {
                result.remove_owned(&token.id)?;
            } else {
                let mut token = token.clone();
                token.amount -= take;
                result.replace_owned(token)?;
            }
        }
        let mut portions: Vec<_> = portions.into_values().collect();
        portions.sort_by_key(|portion| portion.basis.key());
        Ok((result, portions))
    }

    /// Apply an authorized whole-origin revocation to this portfolio only.
    /// Graph revocation, holder indexes and pending transfers remain the caller's
    /// atomic transition; merged proportional cuts require their separate path.
    pub fn with_revoked_origin(&self, origin: &TxHash) -> Result<(Self, Vec<RevocationEvent>), String> {
        let mut result = self.clone();
        let mut events = Vec::new();
        for token in self.origin_tokens(origin) {
            if !matches!(token.state, TrstState::Active | TrstState::Expired) { continue; }
            let mut revoked = token.clone();
            revoked.state = TrstState::Revoked;
            revoked.revoked_origin = Some(*origin);
            result.replace_owned(revoked)?;
            events.push(RevocationEvent {
                holder: self.holder.clone(), token_id: token.id, revoked_origin: *origin,
                revoked_amount: token.amount, total_amount: token.amount,
            });
        }
        Ok((result, events))
    }

    /// Restore exactly the chunks tagged with an authorized burn origin.
    /// Preserve composition bases and use the current governed expiry period.
    /// The caller must first resolve graph/outcome invalidation authorization.
    pub fn with_restored_origin(
        &self, origin: &TxHash, now: Timestamp,
    ) -> Result<(Self, Vec<RestorationEvent>), String> {
        let mut result = self.clone();
        let mut events = Vec::new();
        for token in self.revoked_tokens(origin) {
            let mut restored = token.clone();
            restored.revoked_origin = None;
            restored.state = if token.is_expired(now, self.expiry_secs) {
                TrstState::Expired
            } else { TrstState::Active };
            result.replace_owned(restored)?;
            events.push(RestorationEvent {
                holder: self.holder.clone(), token_id: token.id, amount: token.amount,
            });
        }
        Ok((result, events))
    }

    /// Apply one caller-authorized merged-origin taint using the existing graph
    /// rounding/basis functions. Return both new roots only after every split
    /// succeeds; callers must publish graph, holdings and pending effects together.
    pub fn with_split_revocation(
        &self,
        graph: &MergerGraph,
        merge: &TxHash,
        revoked_origin: TxHash,
        tainted: u128,
    ) -> Result<(Self, MergerGraph, Vec<RevocationEvent>), String> {
        self.with_split_revocation_cached(graph, merge, revoked_origin, tainted, &mut HashMap::new())
    }

    /// Answers are scoped to one revoked origin and immutable revision records.
    /// Reuse across holder splits is valid while the graph only appends revisions.
    pub(crate) fn with_split_revocation_cached(
        &self,
        graph: &MergerGraph,
        merge: &TxHash,
        revoked_origin: TxHash,
        tainted: u128,
        answers: &mut HashMap<TxHash, bool>,
    ) -> Result<(Self, MergerGraph, Vec<RevocationEvent>), String> {
        if !graph.contains_merge(merge) { return Err("TRST split origin is not a merge".into()); }
        let mut result = self.clone();
        let basis_graph = graph;
        let mut graph = graph.clone();
        let mut events = Vec::new();
        for token in self.split_tokens(*merge, revoked_origin) {
            if !matches!(token.state, TrstState::Active | TrstState::Expired) { continue; }
            #[cfg(test)]
            let basis_started = std::time::Instant::now();
            if !graph.valid_basis(merge, token.basis) {
                return Err("invalid TRST split composition basis".into());
            }
            let cut = basis_graph.basis_cut_cached(
                merge, token.basis, token.amount, &revoked_origin, tainted, answers);
            #[cfg(test)]
            crate::engine::SPLIT_PHASES.with(|slot| {
                if let Some(totals) = slot.borrow_mut().as_mut() {
                    totals[0] += basis_started.elapsed().as_nanos();
                }
            });
            if cut == 0 { continue; }
            let mut remainder = token.clone();
            if cut >= token.amount {
                remainder.state = TrstState::Revoked;
                remainder.revoked_origin = Some(revoked_origin);
                remainder.basis = TrstBasis::Single(revoked_origin);
                #[cfg(test)]
                let phase_started = std::time::Instant::now();
                result.replace_owned(remainder)?;
                #[cfg(test)]
                crate::engine::SPLIT_PHASES.with(|slot| {
                    if let Some(totals) = slot.borrow_mut().as_mut() {
                        totals[2] += phase_started.elapsed().as_nanos();
                    }
                });
            } else {
                remainder.amount -= cut;
                #[cfg(test)]
                let phase_started = std::time::Instant::now();
                remainder.basis = graph.reduced_basis(merge, token.basis, revoked_origin, tainted);
                #[cfg(test)]
                crate::engine::SPLIT_PHASES.with(|slot| {
                    if let Some(totals) = slot.borrow_mut().as_mut() {
                        totals[1] += phase_started.elapsed().as_nanos();
                    }
                });
                #[cfg(test)]
                let phase_started = std::time::Instant::now();
                result.replace_owned(remainder)?;
                #[cfg(test)]
                crate::engine::SPLIT_PHASES.with(|slot| {
                    if let Some(totals) = slot.borrow_mut().as_mut() {
                        totals[2] += phase_started.elapsed().as_nanos();
                    }
                });
                #[cfg(test)]
                let phase_started = std::time::Instant::now();
                result.insert_owned(TrstToken {
                    basis: TrstBasis::Single(revoked_origin),
                    id: crate::engine::split_token_id(&token.id, &revoked_origin),
                    amount: cut, origin: *merge, link: token.id, holder: self.holder.clone(),
                    origin_timestamp: token.origin_timestamp,
                    effective_origin_timestamp: token.effective_origin_timestamp,
                    state: TrstState::Revoked, origin_wallet: token.origin_wallet.clone(),
                    revoked_origin: Some(revoked_origin),
                })?;
                #[cfg(test)]
                crate::engine::SPLIT_PHASES.with(|slot| {
                    if let Some(totals) = slot.borrow_mut().as_mut() {
                        totals[3] += phase_started.elapsed().as_nanos();
                    }
                });
            }
            events.push(RevocationEvent {
                holder: self.holder.clone(), token_id: token.id, revoked_origin,
                revoked_amount: cut, total_amount: token.amount,
            });
        }
        Ok((result, graph, events))
    }

    fn add_membership(&mut self, order: Order, token: &TrstToken) {
        if token.revoked_origin.is_none() {
            self.split_members.entry(Self::split_key(token)).or_default().insert(order);
        }
        let (index, origin) = match token.revoked_origin {
            Some(tag) => (&mut self.revoked_origins, tag),
            None => (&mut self.origins, token.origin),
        };
        let mut members = index.get(&origin).cloned().unwrap_or_default();
        members.insert(order);
        index.insert(origin, members);
    }

    fn remove_membership(&mut self, order: Order, token: &TrstToken) {
        if token.revoked_origin.is_none() {
            let key = Self::split_key(token);
            let members = self.split_members.get_mut(&key).expect("indexed split membership");
            members.remove(&order);
            if members.is_empty() { self.split_members.remove(&key); }
        }
        let (index, origin) = match token.revoked_origin {
            Some(tag) => (&mut self.revoked_origins, tag),
            None => (&mut self.origins, token.origin),
        };
        let members = index.get_mut(&origin).expect("indexed origin membership");
        members.remove(&order);
        if members.is_empty() { index.remove(&origin); }
    }

    pub(crate) fn iter_canonical_tokens(&self) -> impl Iterator<Item = &TrstToken> {
        self.order.values().map(|id| &*self.rows[id].token)
    }

    /// Clone holdings into canonical vector order with the current cached balance
    /// and deadline. No expiry transition or graph validation is performed.
    pub fn to_portfolio(&self) -> WalletPortfolio {
        WalletPortfolio::from_parts(
            self.order.values().map(|id| (*self.rows[id].token).clone()).collect(),
            self.balance, self.earliest_expiry())
    }

    fn earliest_expiry(&self) -> Option<Timestamp> {
        self.active_times.get_min().map(|(at, _)| Timestamp::new(at.as_secs().saturating_add(self.expiry_secs)))
    }

    fn header(&self) -> Row {
        Row::Header {
            holder: self.holder.clone(), count: self.rows.len() as u64,
            head: self.order.get_min().map(|(_, id)| *id),
            cached_transferable: self.balance, earliest_expiry: self.earliest_expiry(),
        }
    }

    fn check_token(&self, token: &TrstToken) -> Result<(), String> {
        if token.holder != self.holder { return Err("stable portfolio owner mismatch".into()); }
        if (token.state == TrstState::Revoked) != token.revoked_origin.is_some() {
            return Err("inconsistent TRST revocation state".into());
        }
        Ok(())
    }

    fn remove_token_totals(&mut self, token: &TrstToken) {
        self.counters.remove(token.effective_origin_timestamp.as_secs(),
            token.amount, token.state == TrstState::Revoked);
        self.remove_active_summary(token);
    }

    fn remove_active_summary(&mut self, token: &TrstToken) {
        if token.state == TrstState::Active {
            self.balance -= token.amount;
            let balance = self.origin_active_balance(&token.origin) - token.amount;
            if balance == 0 { self.origin_balances.remove(&token.origin); }
            else { self.origin_balances.insert(token.origin, balance); }
            self.active_times.remove(&(token.effective_origin_timestamp, token.id));
        }
    }

    fn add_token_totals(&mut self, token: &TrstToken) -> Result<(), String> {
        self.add_active_summary(token)?;
        self.counters.insert(token.effective_origin_timestamp.as_secs(),
            token.amount, token.state == TrstState::Revoked);
        Ok(())
    }

    fn add_active_summary(&mut self, token: &TrstToken) -> Result<(), String> {
        if token.state == TrstState::Active {
            let total = self.balance.checked_add(token.amount)
                .ok_or("TRST holding balance overflow")?;
            let balance = self.origin_active_balance(&token.origin).checked_add(token.amount)
                .ok_or("TRST origin balance overflow")?;
            self.balance = total;
            self.origin_balances.insert(token.origin, balance);
            self.active_times.insert((token.effective_origin_timestamp, token.id));
        }
        Ok(())
    }

    /// Private labels are not token identity. Exhaustion relabels in the same
    /// order, costing O(t log t) rather than rejecting an economic operation.
    fn relabel(&mut self) {
        let previous = self.order.clone();
        self.order = OrdMap::new();
        self.origins = OrdMap::new();
        self.revoked_origins = OrdMap::new();
        self.split_members = OrdMap::new();
        for (n, (_, id)) in previous.iter().enumerate() {
            let row = self.rows.get_mut(id).expect("indexed order has a row");
            row.order = (row.order.0, n as u128);
            self.order.insert(row.order, *id);
            let order = row.order;
            let token = row.token.clone();
            self.add_membership(order, &token);
        }
        self.next_order = self.rows.len() as u128;
    }

    /// Insert after existing timestamp ties. O(log t) persistent index work,
    /// except for private-label exhaustion recovery. Checks owner, state/tag
    /// consistency, duplicate ID and active-balance overflow; does not authenticate
    /// issuance or verify graph references.
    pub fn with_inserted(&self, token: TrstToken) -> Result<Self, String> {
        let mut result = self.clone();
        result.insert_owned(token)?;
        Ok(result)
    }

    /// Share the original cut once; discard the entire candidate on any error.
    pub(crate) fn with_inserted_batch(
        &self, tokens: impl IntoIterator<Item = TrstToken>,
    ) -> Result<Self, String> {
        let mut result = self.clone();
        for token in tokens {
            result.insert_owned(token)?;
        }
        Ok(result)
    }

    /// A single insert validates before mutation, so it can reuse owned paths.
    /// Multiple inserts still need a private candidate for whole-batch rollback.
    pub(crate) fn insert_batch(&mut self, tokens: &[TrstToken]) -> Result<(), String> {
        match tokens {
            [] => Ok(()),
            [token] => self.insert_owned(token.clone()),
            _ => {
                *self = self.with_inserted_batch(tokens.iter().cloned())?;
                Ok(())
            }
        }
    }

    // All fallible checks precede index changes. Other owned mutation helpers
    // may change their candidate before failing and must remain private.
    fn insert_owned(&mut self, token: TrstToken) -> Result<(), String> {
        self.check_token(&token)?;
        if self.rows.contains_key(&token.id) { return Err("duplicate TRST holding".into()); }
        self.add_token_totals(&token)?;
        if self.next_order == u128::MAX { self.relabel(); }
        let key = (token.origin_timestamp, self.next_order);
        self.next_order += 1;
        let previous = self.order.get_prev(&key).map(|(_, id)| *id);
        let next = self.order.get_next(&key).map(|(_, id)| *id);
        if let Some(id) = previous {
            self.rows.get_mut(&id).expect("indexed predecessor").next = Some(token.id);
        }
        self.order.insert(key, token.id);
        self.add_membership(key, &token);
        self.rows.insert(token.id, Holding { order: key, token: token.into(), next });
        Ok(())
    }

    /// Replace a payload without changing its established order. A caller that
    /// changes the ordering timestamp must explicitly remove and insert instead.
    pub fn with_replaced(&self, token: TrstToken) -> Result<Self, String> {
        let mut result = self.clone();
        result.replace_owned(token)?;
        Ok(result)
    }

    fn replace_owned(&mut self, token: TrstToken) -> Result<(), String> {
        self.check_token(&token)?;
        let old = self.rows.get(&token.id).ok_or("TRST holding not found")?;
        if old.order.0 != token.origin_timestamp {
            return Err("TRST payload replacement changes ordering timestamp".into());
        }
        if *old.token == token { return Ok(()); }
        let old = old.clone();
        self.remove_token_totals(&old.token);
        self.add_token_totals(&token)?;
        let same_membership = old.token.revoked_origin == token.revoked_origin
            && (token.revoked_origin.is_some() || Self::split_key(&old.token) == Self::split_key(&token));
        if !same_membership {
            self.remove_membership(old.order, &old.token);
            self.add_membership(old.order, &token);
        }
        let id = token.id;
        self.rows.get_mut(&id).expect("existing holding").token = token.into();
        Ok(())
    }

    pub fn with_removed(&self, id: &TxHash) -> Result<Self, String> {
        let mut result = self.clone();
        result.remove_owned(id)?;
        Ok(result)
    }

    // The candidate is already private. Reuse uniquely owned tree paths across
    // a batch instead of cloning its roots again for every consumed fragment.
    fn remove_owned(&mut self, id: &TxHash) -> Result<(), String> {
        let old = self.rows.get(id).cloned().ok_or("TRST holding not found")?;
        self.remove_token_totals(&old.token);
        self.remove_membership(old.order, &old.token);
        let previous = self.order.range((Unbounded, Excluded(old.order)))
            .next_back().map(|(_, id)| *id);
        if let Some(previous) = previous {
            self.rows.get_mut(&previous).expect("indexed predecessor").next = old.next;
        }
        self.order.remove(&old.order);
        self.rows.remove(id);
        Ok(())
    }

    /// Expire only due active holdings. Idle calls share all roots; processing k
    /// due holdings costs O(k log t), without scanning unrelated tokens.
    pub fn with_expired_at(&self, now: Timestamp) -> Result<(Self, u128), String> {
        let mut result = self.clone();
        let mut expired = 0u128;
        while let Some(&(at, id)) = result.active_times.get_min() {
            if !at.has_expired(self.expiry_secs, now) { break; }
            let mut token = result.get(&id).expect("indexed active expiry").clone();
            expired = expired.checked_add(token.amount).ok_or("TRST expired balance overflow")?;
            token.state = TrstState::Expired;
            result.replace_owned(token)?;
        }
        Ok((result, expired))
    }

    /// Normalize the agreed period/clock, including resurrection. Examine all
    /// holdings, but retain active summaries for unchanged states. Origin-time
    /// counters share their root across period changes and clock normalization.
    pub fn with_expiry_period(&self, expiry_secs: u64, now: Timestamp) -> Result<Self, String> {
        #[cfg(test)]
        let probe = std::env::var_os("BURST_PERIOD_STAGE_TIMING")
            .map(|_| std::time::Instant::now());
        let mut result = self.clone();
        result.expiry_secs = expiry_secs;
        let state_at = |token: &TrstToken| match token.state {
            TrstState::Active | TrstState::Expired => {
                if token.is_expired(now, expiry_secs) { TrstState::Expired }
                else { TrstState::Active }
            }
            other => other,
        };
        let mut final_balance = 0u128;
        let mut active_count = 0usize;
        let mut changed_count = 0usize;
        let mut resurrect = false;
        for (_, row) in &self.rows {
            let token = &row.token;
            let state = state_at(token);
            changed_count += usize::from(state != token.state);
            if state == TrstState::Active {
                active_count += 1;
                final_balance = final_balance.checked_add(token.amount)
                    .ok_or("TRST holding balance overflow")?;
                resurrect |= token.state == TrstState::Expired;
            }
        }
        #[cfg(test)]
        let classification_ms = probe.map(|start| start.elapsed().as_secs_f64() * 1000.0);
        #[cfg(test)]
        let mutation_start = probe.map(|_| std::time::Instant::now());
        // Rebuild only when inserting the surviving active entries takes fewer
        // summary updates than removing/adding every changed entry.
        let rebuild = active_count < changed_count;
        if rebuild {
            result.active_times = OrdSet::new();
            result.origin_balances = OrdMap::new();
            result.balance = 0;
        }
        if changed_count != 0 {
            for (id, row) in &self.rows {
                let state = state_at(&row.token);
                if state != row.token.state {
                    if !rebuild && row.token.state == TrstState::Active {
                        result.remove_active_summary(&row.token);
                    }
                    let mut token = (*row.token).clone();
                    token.state = state;
                    if rebuild { result.add_active_summary(&token)?; }
                    result.rows.get_mut(id).expect("existing holding").token = token.into();
                } else if rebuild {
                    result.add_active_summary(&row.token)?;
                }
            }
        }
        // Incremental updates remove expiring value before resurrection adds.
        // Mixed restored states can otherwise overflow a temporary sum even
        // though the checked final balance fits.
        if !rebuild && resurrect {
            for (_, row) in &self.rows {
                if row.token.state == TrstState::Expired && state_at(&row.token) == TrstState::Active {
                    let mut active = (*row.token).clone();
                    active.state = TrstState::Active;
                    result.add_active_summary(&active)?;
                }
            }
        }
        debug_assert_eq!(result.balance, final_balance);
        #[cfg(test)]
        if let (Some(classification_ms), Some(start)) = (classification_ms, mutation_start) {
            let mutation_ms = start.elapsed().as_secs_f64() * 1000.0;
            println!("period_normalization_stages {{\"rows\":{},\"changed\":{changed_count},\"active\":{active_count},\"rebuild\":{rebuild},\"classification_ms\":{classification_ms},\"mutation_ms\":{mutation_ms}}}", self.rows.len());
        }
        Ok(result)
    }

    /// Header first, then changed stable rows in token-ID order. Shared tree
    /// branches are skipped. This ordering is deterministic but deliberately
    /// distinct from the vector visitor's portfolio traversal order.
    /// The callback can stop emission without retaining all changed rows; earlier
    /// callback effects are not rolled back. Holder and period must match. The
    /// indexes supply structural validity, not ledger authorization.
    pub fn visit_delta<E>(
        &self,
        before: &Self,
        mut invalid: impl FnMut(String) -> E,
        mut visit: impl FnMut(Option<Row>, Option<Row>) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.holder != before.holder || self.expiry_secs != before.expiry_secs {
            return Err(invalid("indexed delta requires the same holder and governed period".into()));
        }
        let old = before.header();
        let new = self.header();
        if old != new { visit(Some(old), Some(new))?; }
        for id in self.rows.changed_keys(&before.rows) {
            let encode = |row: &Holding| holding(&self.holder, &row.token, row.next);
            let old = before.rows.get(id).map(encode);
            let new = self.rows.get(id).map(encode);
            if old != new { visit(old, new)?; }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
