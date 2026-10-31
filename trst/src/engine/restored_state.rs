//! Validated restoration state with checked ownership and portfolio updates.
//! This is a local consistency proof, never ledger authentication or finality.
use super::*;

mod prepared;

#[derive(Clone)]
pub struct RestoredTrstState {
    engine: TrstEngine,
    owners: SharedMap<TxHash, Shared<WalletAddress>>,
    revocation_counts: SharedMap<TxHash, usize>,
    revision_index: crate::merger_graph::RevisionIndex,
    // The engine's origin_holders index deliberately excludes revoked tokens.
    // Track their original composition IDs too, so graph removal cannot orphan
    // a revoked token whose tag is an unrelated (but valid) revoked burn.
    revoked_token_origins: SharedMap<TxHash, usize>,
    token_revisions: SharedMap<TxHash, usize>,
    retain_indexes: bool,
}

/// Revocation causes for one origin, restored alongside its holdings.
#[derive(Clone)]
pub struct RestoredTrstInvalidation {
    pub wallet_revoked: bool,
    pub causes: HashSet<BlockHash>,
}

/// Records from one storage snapshot. `replace_records` validates ownership,
/// invalidations and portfolios together against the graph and expiry period.
#[derive(Default)]
pub struct RestoredTrstChanges {
    pub owner_headers: std::collections::BTreeMap<WalletAddress, bool>,
    pub origins: std::collections::BTreeMap<TxHash, Option<WalletAddress>>,
    pub portfolios: std::collections::BTreeMap<WalletAddress, Option<WalletPortfolio>>,
    pub invalidations: std::collections::BTreeMap<TxHash, Option<RestoredTrstInvalidation>>,
}

impl RestoredTrstState {
    /// Validate the complete engine before accepting incremental holding changes.
    /// Owning the engine prevents callers from changing its validation context.
    pub fn from_engine(engine: TrstEngine) -> Result<Self, String> {
        Self::from_engine_with_poll(engine, &|_| Ok(()))
    }

    /// Validate and construct private indexes, returning no state if polling fails.
    /// This does not bound allocation, individual map operations or destruction.
    pub fn from_engine_with_poll(
        mut engine: TrstEngine,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::validate_engine_with_poll(&mut engine, poll)?;
        let mut owners = SharedMap::default();
        for (owner, origins) in &engine.wallet_origins {
            poll("trst:restored-owner")?;
            let owner = Shared::from(owner.clone());
            for origin in origins {
                poll("trst:restored-origin")?;
                owners.insert(*origin, owner.clone());
            }
        }
        let revocation_counts = engine.merger_graph.revocation_counts_with_poll(poll)?;
        let revision_index =
            crate::merger_graph::RevisionIndex::new_with_poll(&engine.merger_graph, poll)?;
        let (revoked_token_origins, token_revisions) =
            Self::build_token_counts_with_poll(&engine, poll)?;
        Ok(Self {
            engine,
            owners,
            revocation_counts,
            revision_index,
            revoked_token_origins,
            token_revisions,
            retain_indexes: false,
        })
    }

    fn validate_engine_with_poll(
        engine: &mut TrstEngine,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(), String> {
        engine.merger_graph.validate_persisted_with_poll(poll)?;
        engine.validate_bases_with_poll(poll)?;
        for (owner, portfolio) in &engine.wallets {
            poll("trst:restored-portfolio")?;
            Self::validate_portfolio_with_poll(engine, owner, portfolio, poll)?;
        }
        for invalidation in engine.outcome_invalidations.values() {
            poll("trst:restored-outcome")?;
            if invalidation.causes.is_empty() {
                return Err("invalid outcome revocation causes".into());
            }
            for cause in &invalidation.causes {
                poll("trst:restored-cause")?;
                if cause.is_zero() {
                    return Err("invalid outcome revocation causes".into());
                }
            }
        }
        engine.rebuild_indexes_with_poll(poll)?;
        Ok(())
    }

    /// Full validation for a read-only consumer. Keep all checked counts, using
    /// temporary hash maps instead of retaining mutation indexes that it discards.
    pub(super) fn validate_for_comparison_with_poll(
        mut engine: TrstEngine,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<TrstEngine, String> {
        Self::validate_engine_with_poll(&mut engine, poll)?;
        for origins in engine.wallet_origins.values() {
            poll("trst:restored-owner")?;
            for _ in origins {
                poll("trst:restored-origin")?;
            }
        }
        drop(
            engine
                .merger_graph
                .build_revocation_counts_with_poll::<HashMap<TxHash, usize>>(poll)?,
        );
        drop(
            crate::merger_graph::RevisionIndex::build_counts_with_poll::<HashMap<TxHash, usize>>(
                &engine.merger_graph,
                poll,
            )?,
        );
        drop(Self::build_token_counts_with_poll::<HashMap<TxHash, usize>>(&engine, poll)?);
        Ok(engine)
    }

    fn build_token_counts_with_poll<C: crate::dependency_counts::Counts>(
        engine: &TrstEngine,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(C, C), String> {
        let mut revoked_token_origins = C::default();
        let mut token_revisions = C::default();
        for portfolio in engine.wallets.values() {
            poll("trst:dependency-wallet")?;
            for token in &portfolio.tokens {
                poll("trst:dependency-token")?;
                Self::count_token_dependencies(
                    &mut revoked_token_origins,
                    &mut token_revisions,
                    token,
                    true,
                )?;
            }
        }
        Ok((revoked_token_origins, token_revisions))
    }

    pub fn engine(&self) -> &TrstEngine {
        &self.engine
    }

    pub fn into_engine(self) -> TrstEngine {
        self.engine
    }

    /// Build persistent indexes for storage writers that update individual rows.
    /// Conversion costs O(t log t) for a vector portfolio containing t holdings.
    pub fn retain_portfolio_indexes(&mut self) -> Result<(), String> {
        let mut wallets = self.engine.wallets.clone();
        for (holder, portfolio) in &self.engine.wallets {
            let indexed = portfolio.validated_index(holder, self.engine.expiry_secs)?;
            wallets.insert(
                holder.clone(),
                WalletPortfolio::from_indexed(indexed).into(),
            );
        }
        self.engine.wallets = wallets;
        self.retain_indexes = true;
        Ok(())
    }

    /// Build a successor from this validated snapshot and changed holdings. None means
    /// the caller must use full restoration; Err means changed data is invalid.
    /// Public derived indexes in `after` are deliberately never adopted.
    pub fn retained_holdings_successor(
        &self,
        before: &TrstEngine,
        after: &TrstEngine,
    ) -> Result<Option<Self>, String> {
        let base = &self.engine;
        if !base.shares_wallet_snapshot_with(before)
            || base.expiry_secs != before.expiry_secs
            || base.expiry_secs != after.expiry_secs
            || !base.merger_graph.shares_storage_with(&before.merger_graph)
            || !base.merger_graph.shares_storage_with(&after.merger_graph)
            || !base
                .wallet_origins
                .shares_storage_with(&after.wallet_origins)
            || !base
                .outcome_invalidations
                .shares_storage_with(&before.outcome_invalidations)
        {
            return Ok(None);
        }
        // Unchanged records inherit the validated cut. Changed causes remain
        // consistency data; outcome authorization belongs to the ledger caller.
        for origin in after
            .outcome_invalidations
            .changed_keys(&base.outcome_invalidations)
        {
            if let Some(invalidation) = after.outcome_invalidations.get(origin) {
                if invalidation.causes.is_empty()
                    || invalidation.causes.iter().any(BlockHash::is_zero)
                {
                    return Err("invalid outcome revocation causes".into());
                }
                if !after.merger_graph.is_origin_revoked(origin) {
                    return Err("outcome-invalidated origin is active in merger graph".into());
                }
            }
        }
        let mut candidate = self.clone();
        candidate.engine.outcome_invalidations = after.outcome_invalidations.clone();
        let mut supported = true;
        after.visit_portfolio_changes(before, |holder, old, new| {
            let new_portfolio = new;
            let empty = || {
                holding_rows::indexed::IndexedPortfolio::from_portfolio(
                    holder.clone(),
                    &WalletPortfolio::default(),
                    base.expiry_secs,
                )
            };
            let old_root = match old {
                Some(portfolio) => match portfolio.indexed() {
                    Some(root) => root.clone(),
                    None => {
                        supported = false;
                        return Ok::<_, String>(());
                    }
                },
                None => empty()?,
            };
            let new = match new {
                Some(portfolio) => match portfolio.indexed() {
                    Some(root) => root.clone(),
                    // A newly created portfolio has no untouched holdings to
                    // scan: checking all its incoming data is required work.
                    None if old.is_none() => portfolio.validated_index(holder, base.expiry_secs)?,
                    None => {
                        supported = false;
                        return Ok(());
                    }
                },
                None => empty()?,
            };
            if !new.can_diff_with(&old_root, holder) {
                supported = false;
                return Ok(());
            }
            let mut origins = HashSet::new();
            let mut tags = HashSet::new();
            new.visit_delta(
                &old_root,
                |e| e,
                |old, new| {
                    if let Some(holding_rows::Row::Holding { token, .. }) = &new {
                        Self::validate_token(base, holder, token)?;
                    }
                    if let Some(holding_rows::Row::Holding { token, .. }) = &old {
                        Self::count_token_dependencies(
                            &mut candidate.revoked_token_origins,
                            &mut candidate.token_revisions,
                            token,
                            false,
                        )?;
                    }
                    if let Some(holding_rows::Row::Holding { token, .. }) = &new {
                        Self::count_token_dependencies(
                            &mut candidate.revoked_token_origins,
                            &mut candidate.token_revisions,
                            token,
                            true,
                        )?;
                    }
                    for row in old.into_iter().chain(new) {
                        if let holding_rows::Row::Holding { token, .. } = row {
                            if let Some(tag) = token.revoked_origin {
                                tags.insert(tag);
                            } else {
                                origins.insert(token.origin);
                            }
                        }
                    }
                    Ok(())
                },
            )?;
            for (keys, index, revoked) in [
                (origins, &mut candidate.engine.origin_holders, false),
                (tags, &mut candidate.engine.revoked_holders, true),
            ] {
                for origin in keys {
                    let present = if revoked {
                        new.revoked_tokens(&origin).next().is_some()
                    } else {
                        new.origin_tokens(&origin).next().is_some()
                    };
                    if present {
                        index.entry(origin).or_default().insert(holder.clone());
                    } else if let Some(holders) = index.get_mut(&origin) {
                        holders.remove(holder);
                        if holders.is_empty() {
                            index.remove(&origin);
                        }
                    }
                }
            }
            match new_portfolio {
                Some(portfolio) if portfolio.indexed().is_some() => {
                    candidate
                        .engine
                        .wallets
                        .insert(holder.clone(), after.wallets[holder].clone());
                }
                Some(_) => {
                    candidate
                        .engine
                        .wallets
                        .insert(holder.clone(), WalletPortfolio::from_indexed(new).into());
                }
                None => {
                    candidate.engine.wallets.remove(holder);
                }
            }
            Ok(())
        })?;
        Ok(supported.then_some(candidate))
    }

    /// Validate appended merges and changed holdings against the current snapshot.
    /// Storage must persist both before publishing the returned state.
    pub fn retained_append_successor(
        &self,
        before: &TrstEngine,
        after: &TrstEngine,
    ) -> Result<Option<Self>, String> {
        if !self
            .engine
            .merger_graph
            .shares_storage_with(&before.merger_graph)
        {
            return Ok(None);
        }
        let Some(added) = after
            .merger_graph
            .validated_append_ids(&self.engine.merger_graph)?
        else {
            return Ok(None);
        };
        for id in &added {
            if self.owners.contains_key(id) {
                return Err("TRST origin is registered as both burn and merge".into());
            }
            for source in &after
                .merger_graph
                .get_merge(id)
                .expect("validated added node")
                .source_origins
            {
                if !after.merger_graph.contains_merge(&source.origin)
                    && !self.owners.contains_key(&source.origin)
                {
                    return Err("merger graph source lacks burn ownership".into());
                }
            }
        }
        // Appends cannot alter an existing origin's ancestry or revocation state.
        // Reuse the holdings proof with the now-validated graph as its context.
        let mut context = self.clone();
        context.revocation_counts = after
            .merger_graph
            .add_revocation_counts(&self.revocation_counts, added)?;
        context.revision_index = self
            .revision_index
            .updated(&before.merger_graph, &after.merger_graph)?;
        context.engine.merger_graph = after.merger_graph.clone();
        let mut scoped_before = before.clone();
        scoped_before.merger_graph = after.merger_graph.clone();
        context.retained_holdings_successor(&scoped_before, after)
    }

    /// Retain unaffected holdings across graph revocation metadata changes.
    /// Fixed topology, checked metadata deltas and private contribution counts
    /// preserve the validated graph without scanning untouched nodes.
    pub fn retained_revocation_successor(
        &self,
        before: &TrstEngine,
        after: &TrstEngine,
    ) -> Result<Option<Self>, String> {
        if !self
            .engine
            .merger_graph
            .shares_storage_with(&before.merger_graph)
            || !self.engine.shares_wallet_snapshot_with(before)
            || !self
                .engine
                .wallet_origins
                .shares_storage_with(&after.wallet_origins)
            || !self
                .engine
                .outcome_invalidations
                .shares_storage_with(&before.outcome_invalidations)
            || self.engine.expiry_secs != before.expiry_secs
            || self.engine.expiry_secs != after.expiry_secs
        {
            return Ok(None);
        }
        let Some(revocation_counts) = after
            .merger_graph
            .validated_revocation_transition(&before.merger_graph, &self.revocation_counts)?
        else {
            return Ok(None);
        };
        let mut removed = Vec::new();
        for origin in after
            .merger_graph
            .revoked_origins()
            .changed_keys(before.merger_graph.revoked_origins())
        {
            if after.merger_graph.is_origin_revoked(origin) {
                if !self.owners.contains_key(origin) {
                    return Err("revoked TRST origin lacks burn ownership".into());
                }
            } else {
                if after.outcome_invalidations.contains_key(origin) {
                    return Err("outcome-invalidated origin is active in merger graph".into());
                }
                removed.push(*origin);
            }
        }
        let mut context = self.clone();
        context.revocation_counts = revocation_counts;
        context.revision_index = self
            .revision_index
            .updated(&before.merger_graph, &after.merger_graph)?;
        context.engine.merger_graph = after.merger_graph.clone();
        let mut scoped_before = before.clone();
        scoped_before.merger_graph = after.merger_graph.clone();
        let Some(candidate) = context.retained_holdings_successor(&scoped_before, after)? else {
            return Ok(None);
        };
        for origin in removed {
            if candidate
                .engine
                .revoked_holders
                .get(&origin)
                .is_some_and(|holders| !holders.is_empty())
            {
                return Err("TRST revoked holding lacks its revoked burn origin".into());
            }
        }
        Ok(Some(candidate))
    }

    /// Checked inverse of complete appended graph rows plus final holding and
    /// ownership records from the same writer. The caller must first compare
    /// exact journal bytes with storage, and publish only after all writes pass.
    /// Unsupported revision edits retain the cold restoration fallback.
    pub fn retained_graph_removal(
        &self,
        records: &[crate::merger_graph::GraphRecord],
        changes: RestoredTrstChanges,
    ) -> Result<Option<Self>, String> {
        let Some((graph, removed)) = self
            .engine
            .merger_graph
            .remove_appended_records(records, &self.revision_index)?
        else {
            return Ok(None);
        };
        let mut candidate = self.clone();
        for id in &removed {
            for origin in self
                .engine
                .merger_graph
                .get_merge(id)
                .expect("checked removed node")
                .revoked_contribs
                .keys()
            {
                let count = candidate
                    .revocation_counts
                    .get(origin)
                    .copied()
                    .unwrap_or(0)
                    .checked_sub(1)
                    .ok_or("revocation contribution count underflow")?;
                if count == 0 {
                    candidate.revocation_counts.remove(origin);
                } else {
                    candidate.revocation_counts.insert(*origin, count);
                }
            }
        }
        candidate.revision_index = self
            .revision_index
            .updated(&self.engine.merger_graph, &graph)?;
        candidate.engine.merger_graph = graph;
        candidate.replace_records(changes)?;
        for id in removed {
            if candidate
                .engine
                .origin_holders
                .get(&id)
                .is_some_and(|holders| !holders.is_empty())
                || candidate.revoked_token_origins.contains_key(&id)
            {
                return Err("graph row undo would orphan a TRST holding".into());
            }
        }
        Ok(Some(candidate))
    }

    /// Inverse metadata or append rows from a matched journal. A graph-only
    /// proof is insufficient: all changed holdings/outcomes and surviving
    /// remainder references are checked before returning a candidate.
    pub fn retained_graph_rewind(
        &self,
        graph_changes: &[(
            Option<crate::merger_graph::GraphRecord>,
            Option<crate::merger_graph::GraphRecord>,
        )],
        changes: RestoredTrstChanges,
    ) -> Result<Option<Self>, String> {
        let Some(rewound) = self.engine.merger_graph.rewind_revocation_records(
            graph_changes,
            &self.revocation_counts,
            &self.revision_index,
        )?
        else {
            let removed = graph_changes
                .iter()
                .map(|(old, new)| new.is_none().then(|| old.clone()).flatten())
                .collect::<Option<Vec<_>>>();
            return match removed {
                Some(rows) => self.retained_graph_removal(&rows, changes),
                None => Ok(None),
            };
        };
        let mut candidate = self.clone();
        candidate.engine.merger_graph = rewound.graph;
        candidate.revocation_counts = rewound.contributions;
        candidate.revision_index = rewound.revisions;
        candidate.replace_records(changes)?;
        for revision in rewound.removed_revisions {
            if candidate.token_revisions.contains_key(&revision) {
                return Err("graph undo would orphan a held composition revision".into());
            }
        }
        for origin in candidate
            .engine
            .merger_graph
            .revoked_origins()
            .changed_keys(self.engine.merger_graph.revoked_origins())
        {
            if candidate.engine.merger_graph.is_origin_revoked(origin) {
                if !candidate.owners.contains_key(origin) {
                    return Err("revoked TRST origin lacks burn ownership".into());
                }
            } else if candidate.engine.outcome_invalidations.contains_key(origin)
                || candidate
                    .engine
                    .revoked_holders
                    .get(origin)
                    .is_some_and(|holders| !holders.is_empty())
            {
                return Err("graph undo leaves an active origin with revoked dependencies".into());
            }
        }
        Ok(Some(candidate))
    }

    fn count_token_dependencies<C: crate::dependency_counts::Counts>(
        revoked: &mut C,
        revisions: &mut C,
        token: &TrstToken,
        add: bool,
    ) -> Result<(), String> {
        let adjust = |counts: &mut C, key, add| {
            crate::dependency_counts::adjust(
                counts,
                key,
                add,
                "token dependency count overflow or underflow",
            )
        };
        if token.revoked_origin.is_some() {
            adjust(revoked, token.origin, add)?;
        }
        if let TrstBasis::Remainder(revision) = token.basis {
            adjust(revisions, revision, add)?;
        }
        Ok(())
    }

    /// Apply final registry and holding records atomically. The reverse owner
    /// index prevents a new registration from requiring a global owner scan.
    /// Removing an origin checks graph references and all remaining holders.
    pub fn replace_records(&mut self, changes: RestoredTrstChanges) -> Result<(), String> {
        let mut candidate = self.clone();
        for (owner, present) in &changes.owner_headers {
            if *present && !candidate.engine.wallet_origins.contains_key(owner) {
                candidate
                    .engine
                    .wallet_origins
                    .insert(owner.clone(), SharedSet::default());
            }
        }
        for (origin, owner) in &changes.origins {
            let old = candidate.owners.get(origin).cloned();
            if old.as_ref().map(|owner| &**owner) == owner.as_ref() {
                continue;
            }
            if let Some(owner) = owner {
                if changes.owner_headers.get(owner) == Some(&false)
                    || !candidate.engine.wallet_origins.contains_key(owner)
                {
                    return Err("TRST origin has no owner record".into());
                }
                if candidate.engine.merger_graph.contains_merge(origin) {
                    return Err("TRST origin is registered as both burn and merge".into());
                }
            }
            if let Some(old) = old {
                candidate
                    .engine
                    .wallet_origins
                    .get_mut(&old)
                    .expect("validated owner")
                    .remove(origin);
                candidate.owners.remove(origin);
            }
            if let Some(owner) = owner {
                candidate
                    .engine
                    .wallet_origins
                    .get_mut(owner)
                    .expect("checked owner")
                    .insert(*origin);
                candidate
                    .owners
                    .insert(*origin, Shared::from(owner.clone()));
            }
        }
        for (owner, present) in &changes.owner_headers {
            if !present {
                if candidate
                    .engine
                    .wallet_origins
                    .get(owner)
                    .is_some_and(|origins| !origins.is_empty())
                {
                    return Err("TRST owner removal leaves registered origins".into());
                }
                candidate.engine.wallet_origins.remove(owner);
            }
        }
        for (origin, invalidation) in changes.invalidations {
            if let Some(invalidation) = invalidation {
                if invalidation.causes.is_empty()
                    || invalidation.causes.iter().any(BlockHash::is_zero)
                    || !candidate.engine.merger_graph.is_origin_revoked(&origin)
                    || !candidate.owners.contains_key(&origin)
                {
                    return Err("invalid outcome revocation record".into());
                }
                candidate.engine.outcome_invalidations.insert(
                    origin,
                    OutcomeInvalidation {
                        wallet_revoked: invalidation.wallet_revoked,
                        causes: invalidation.causes,
                    },
                );
            } else {
                candidate.engine.outcome_invalidations.remove(&origin);
            }
        }
        for (owner, portfolio) in changes.portfolios {
            candidate.replace_portfolio(&owner, portfolio)?;
        }
        let mut affected = std::collections::BTreeSet::new();
        for origin in changes.origins.keys() {
            if self.owners.contains_key(origin)
                && !candidate.owners.contains_key(origin)
                && candidate.engine.merger_graph.references_burn_origin(origin)
            {
                return Err("TRST origin removal leaves a graph dependency".into());
            }
            if let Some(holders) = candidate.engine.origin_holders.get(origin) {
                affected.extend(holders.iter().cloned());
            }
            if let Some(holders) = candidate.engine.revoked_holders.get(origin) {
                affected.extend(holders.iter().cloned());
            }
        }
        for holder in affected {
            if let Some(portfolio) = candidate.engine.wallets.get(&holder) {
                Self::validate_portfolio(&candidate.engine, &holder, portfolio)?;
            }
        }
        *self = candidate;
        Ok(())
    }

    fn validate_portfolio(
        engine: &TrstEngine,
        owner: &WalletAddress,
        portfolio: &WalletPortfolio,
    ) -> Result<(), String> {
        Self::validate_portfolio_with_poll(engine, owner, portfolio, &|_| Ok(()))
    }

    fn validate_portfolio_with_poll(
        engine: &TrstEngine,
        owner: &WalletAddress,
        portfolio: &WalletPortfolio,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(), String> {
        portfolio.validate_snapshot_with_poll(poll)?;
        let mut expiry = None;
        for token in &portfolio.tokens {
            poll("trst:restored-token")?;
            Self::validate_token(engine, owner, token)?;
            if token.state == TrstState::Active {
                let at = token.earliest_expiry(engine.expiry_secs);
                expiry = Some(expiry.map_or(at, |old: Timestamp| old.min(at)));
            }
        }
        if expiry != portfolio.earliest_expiry {
            return Err("TRST holding expiry cache differs from governed period".into());
        }
        Ok(())
    }

    fn validate_token(
        engine: &TrstEngine,
        owner: &WalletAddress,
        token: &TrstToken,
    ) -> Result<(), String> {
        if &token.holder != owner {
            return Err("TRST holding owner differs from its portfolio".into());
        }
        if !engine.merger_graph.contains_merge(&token.origin)
            && !engine
                .wallet_origins
                .get(&token.origin_wallet)
                .is_some_and(|origins| origins.contains(&token.origin))
        {
            return Err("TRST holding differs from its burn ownership".into());
        }
        // The fixed graph was fully checked against the fixed registry at
        // construction, so every revoked graph origin has a burn owner.
        if token
            .revoked_origin
            .is_some_and(|origin| !engine.merger_graph.is_origin_revoked(&origin))
        {
            return Err("TRST revoked holding lacks its revoked burn origin".into());
        }
        if !engine.merger_graph.valid_basis(&token.origin, token.basis) {
            return Err("TRST holding references an invalid composition basis".into());
        }
        Ok(())
    }

    /// Replace a complete portfolio from the same storage snapshot. The graph,
    /// origin registry and expiry period must still match this validated state.
    /// Checks and index updates visit the old and new holdings; this is not an
    /// individual-row update. Invalid portfolio data leaves the state unchanged.
    pub fn replace_portfolio(
        &mut self,
        owner: &WalletAddress,
        portfolio: Option<WalletPortfolio>,
    ) -> Result<(), String> {
        let portfolio = if self.retain_indexes {
            portfolio
                .map(|portfolio| {
                    portfolio
                        .validated_index(owner, self.engine.expiry_secs)
                        .map(WalletPortfolio::from_indexed)
                })
                .transpose()?
        } else {
            portfolio
        };
        if let Some(portfolio) = &portfolio {
            Self::validate_portfolio(&self.engine, owner, portfolio)?;
        }
        if self.engine.wallets.get(owner).map(|old| &**old) == portfolio.as_ref() {
            return Ok(());
        }
        let mut origins = HashSet::new();
        let mut tags = HashSet::new();
        if let Some(old) = self.engine.wallets.get(owner) {
            for token in &old.tokens {
                Self::count_token_dependencies(
                    &mut self.revoked_token_origins,
                    &mut self.token_revisions,
                    token,
                    false,
                )?;
                if let Some(tag) = token.revoked_origin {
                    tags.insert(tag);
                } else {
                    origins.insert(token.origin);
                }
            }
        }
        self.engine.wallets.remove(owner);
        self.engine.deindex_wallet_origins(owner, &origins, &tags);
        if let Some(portfolio) = portfolio {
            for token in &portfolio.tokens {
                Self::count_token_dependencies(
                    &mut self.revoked_token_origins,
                    &mut self.token_revisions,
                    token,
                    true,
                )?;
                match token.revoked_origin {
                    Some(tag) => {
                        self.engine
                            .revoked_holders
                            .entry(tag)
                            .or_default()
                            .insert(owner.clone());
                    }
                    None => {
                        self.engine
                            .origin_holders
                            .entry(token.origin)
                            .or_default()
                            .insert(owner.clone());
                    }
                }
            }
            self.engine.wallets.insert(owner.clone(), portfolio.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
