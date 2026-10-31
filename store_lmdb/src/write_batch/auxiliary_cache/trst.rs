use super::*;
use burst_trst::{
    engine::{
        holding_rows, RestoredTrstChanges, RestoredTrstInvalidation, RestoredTrstState,
        UnindexedWallets, WalletRecord, WalletRecordReader,
    },
    merger_graph::GraphRecord,
    MergerGraph, SharedSet, TrstEngine, WalletPortfolio,
};

mod graph_delta;

const HOLDINGS_FORMAT: &str = "trst_state_format";
const GRAPH_FORMAT: &str = "trst_graph_format";
const GRAPH_SNAPSHOT: &str = "merger_graph";

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

// Engine validators use String errors; keep storage and cancellation variants
// intact when polling crosses that API boundary.
fn with_engine_poll<T>(
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    read: impl FnOnce(&dyn Fn(&'static str) -> Result<(), String>) -> Result<T, String>,
) -> Result<T, StoreError> {
    let failure = std::cell::RefCell::new(None);
    let result = read(&|stage| {
        poll(stage).map_err(|error| {
            let message = error.to_string();
            *failure.borrow_mut() = Some(error);
            message
        })
    });
    match failure.into_inner() {
        Some(error) => Err(error),
        None => result.map_err(invalid),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HoldingsFormat {
    Aggregate,
    Positional,
    Stable,
}

#[derive(Clone)]
pub(super) struct TrstCache {
    state: RestoredTrstState,
    format: HoldingsFormat,
    dirty: SharedSet<WalletAddress>,
    dirty_owners: SharedSet<WalletAddress>,
    dirty_origins: SharedSet<TxHash>,
    dirty_invalidations: SharedSet<TxHash>,
}

impl TrstCache {
    pub(super) fn clean_snapshot(&self, expiry_secs: u64) -> Option<TrstEngine> {
        (self.format == HoldingsFormat::Stable
            && self.state.engine().expiry_secs == expiry_secs
            && self.dirty.is_empty()
            && self.dirty_owners.is_empty()
            && self.dirty_origins.is_empty()
            && self.dirty_invalidations.is_empty())
        .then(|| self.state.engine().clone())
    }
}

pub(crate) struct TrstRewind {
    cache: TrstCache,
    graph: Vec<(Option<GraphRecord>, Option<GraphRecord>)>,
}

impl AuxiliaryCache {
    pub(crate) fn clear_trst(&mut self, table: UndoTable) {
        if table == UndoTable::Meta {
            self.trst = None;
        }
    }

    pub(crate) fn update_trst(&mut self, key: &[u8], before: Option<&[u8]>, after: Option<&[u8]>) {
        let Some(cache) = self.trst.as_mut() else {
            return;
        };
        if key == HOLDINGS_FORMAT.as_bytes()
            || key == GRAPH_FORMAT.as_bytes()
            || key == TrstEngine::meta_key().as_bytes()
            || key == GRAPH_SNAPSHOT.as_bytes()
            || key.starts_with(GraphRecord::STORAGE_PREFIX.as_bytes())
        {
            self.trst = None;
        } else if key.starts_with(holding_rows::PREFIX.as_bytes()) {
            let records = (|| {
                if cache.format != HoldingsFormat::Stable {
                    return None;
                }
                let key = std::str::from_utf8(key).ok()?;
                before
                    .into_iter()
                    .chain(after)
                    .map(|bytes| holding_rows::Row::from_storage_entry(key, bytes).ok())
                    .collect::<Option<Vec<_>>>()
            })();
            let Some(records) = records else {
                self.trst = None;
                return;
            };
            for record in records {
                let holder = match record {
                    holding_rows::Row::Header { holder, .. }
                    | holding_rows::Row::Holding { holder, .. } => holder,
                };
                cache.dirty.insert(holder);
            }
        } else if key.starts_with(WalletRecord::STORAGE_PREFIX.as_bytes()) {
            let records = (|| {
                if cache.format == HoldingsFormat::Aggregate {
                    return None;
                }
                let key = std::str::from_utf8(key).ok()?;
                before
                    .into_iter()
                    .chain(after)
                    .map(|bytes| WalletRecord::from_storage_entry(key, bytes).ok())
                    .collect::<Option<Vec<_>>>()
            })();
            let Some(records) = records else {
                self.trst = None;
                return;
            };
            for record in records {
                match record {
                    WalletRecord::Portfolio { holder, .. }
                    | WalletRecord::Holding { holder, .. } => {
                        if cache.format != HoldingsFormat::Positional {
                            self.trst = None;
                            return;
                        }
                        cache.dirty.insert(holder);
                    }
                    WalletRecord::OriginOwner(owner) => {
                        cache.dirty_owners.insert(owner);
                    }
                    WalletRecord::Origin { origin, .. } => {
                        cache.dirty_origins.insert(origin);
                    }
                    WalletRecord::Invalidation { origin, .. }
                    | WalletRecord::Cause { origin, .. } => {
                        cache.dirty_invalidations.insert(origin);
                    }
                }
            }
        }
    }
}

impl LmdbWriteTransaction<'_> {
    /// Called only after every journal image matched this writer. Simulate cache
    /// invalidations separately; raw mutations keep their normal invalidation.
    pub(crate) fn prepare_trst_rewind(
        &self,
        changes: &[crate::block_undo::UndoChange],
    ) -> Result<Option<TrstRewind>, StoreError> {
        let auxiliary = self.auxiliary_cache.borrow();
        let Some(cache) = auxiliary.trst.as_ref() else {
            return Ok(None);
        };
        if cache.format != HoldingsFormat::Stable
            || !cache.dirty.is_empty()
            || !cache.dirty_owners.is_empty()
            || !cache.dirty_origins.is_empty()
            || !cache.dirty_invalidations.is_empty()
        {
            return Ok(None);
        }
        let mut pending = AuxiliaryCache {
            trst: Some(cache.clone()),
            ..Default::default()
        };
        let mut graph = Vec::new();
        for change in changes
            .iter()
            .filter(|change| change.table == UndoTable::Meta)
        {
            if change
                .key
                .starts_with(GraphRecord::STORAGE_PREFIX.as_bytes())
            {
                if change.before == change.after {
                    continue;
                }
                let key = std::str::from_utf8(&change.key).map_err(invalid)?;
                let decode =
                    |bytes: &[u8]| GraphRecord::from_storage_entry(key, bytes).map_err(invalid);
                graph.push((
                    change.after.as_deref().map(decode).transpose()?,
                    change.before.as_deref().map(decode).transpose()?,
                ));
            } else {
                pending.update_trst(
                    &change.key,
                    change.after.as_deref(),
                    change.before.as_deref(),
                );
            }
        }
        Ok(pending
            .trst
            .filter(|_| !graph.is_empty())
            .map(|cache| TrstRewind { cache, graph }))
    }

    /// Publish only after the entire raw rewind succeeds. Unsupported graph edits
    /// leave the invalidated cache absent and use ordinary full restoration.
    pub(crate) fn finish_trst_rewind(
        &mut self,
        pending: Option<TrstRewind>,
    ) -> Result<(), StoreError> {
        let Some(pending) = pending else {
            return Ok(());
        };
        let changes = self.trst_cache_changes(&pending.cache)?;
        if let Some(state) = pending
            .cache
            .state
            .retained_graph_rewind(&pending.graph, changes)
            .map_err(invalid)?
        {
            self.validate_pending_bases(&state.engine().merger_graph)?;
            self.auxiliary_cache.get_mut().trst = Some(TrstCache {
                state,
                format: HoldingsFormat::Stable,
                dirty: SharedSet::default(),
                dirty_owners: SharedSet::default(),
                dirty_origins: SharedSet::default(),
                dirty_invalidations: SharedSet::default(),
            });
        }
        Ok(())
    }

    fn trst_format(&self) -> Result<HoldingsFormat, StoreError> {
        self.read_view().trst_format()
    }

    /// Compatibility guard for callers that still write positional records.
    /// Stable readers do not authorize old writers to mutate the new format.
    pub fn uses_trst_records(&self) -> Result<bool, StoreError> {
        match self.trst_format()? {
            HoldingsFormat::Aggregate => Ok(false),
            HoldingsFormat::Positional => Ok(true),
            HoldingsFormat::Stable => Err(invalid("stable TRST writes are not integrated")),
        }
    }
    /// Validated physical format query for representation-aware callers.
    pub fn uses_stable_trst_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_stable_trst_records()
    }
    pub fn uses_graph_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_graph_records()
    }

    /// Holdings only; callers attaching a graph must validate the combined state.
    pub fn read_trst_holdings(&self, expiry_secs: u64) -> Result<TrstEngine, StoreError> {
        self.read_view().read_trst_holdings(expiry_secs)
    }

    /// Persist changed stable holdings and canonical ownership/invalidation rows.
    /// The caller must abort on error, including a conflict after earlier writes.
    /// This never sets a format marker or migrates provisional undo records.
    pub fn persist_stable_trst_delta(
        &mut self,
        before: &TrstEngine,
        after: &TrstEngine,
    ) -> Result<(), StoreError> {
        if self.trst_format()? != HoldingsFormat::Stable {
            return Err(invalid("stable TRST persistence requires migrated storage"));
        }
        #[cfg(test)]
        let proof_start = std::time::Instant::now();
        let successor = {
            let auxiliary = self.auxiliary_cache.borrow();
            match auxiliary.trst.as_ref().filter(|cache| {
                cache.format == HoldingsFormat::Stable
                    && cache.dirty.is_empty()
                    && cache.dirty_owners.is_empty()
                    && cache.dirty_origins.is_empty()
                    && cache.dirty_invalidations.is_empty()
            }) {
                Some(cache) => cache
                    .state
                    .retained_holdings_successor(before, after)
                    .map_err(invalid)?,
                None => None,
            }
        };
        #[cfg(test)]
        HOLDING_PROBE.with(|slot| {
            if let Some(parts) = slot.borrow_mut().as_mut() {
                parts[0] += proof_start.elapsed().as_nanos();
            }
        });
        if successor.is_some() {
            // This synchronous CAS sequence already has a validated successor.
            // Discard the old cut instead of decoding every row just to dirty it.
            // Any early return leaves no reusable cache; publish only on success.
            self.auxiliary_cache.get_mut().trst = None;
        }
        after.visit_portfolio_changes(before, |owner, old, new| {
            holding_rows::visit_delta(owner, old, new, invalid, |old, new| {
                #[cfg(test)]
                let encode_start = std::time::Instant::now();
                let old = old
                    .map(|row| row.storage_entry())
                    .transpose()
                    .map_err(invalid)?;
                let new = new
                    .map(|row| row.storage_entry())
                    .transpose()
                    .map_err(invalid)?;
                #[cfg(test)]
                HOLDING_PROBE.with(|slot| {
                    if let Some(parts) = slot.borrow_mut().as_mut() {
                        parts[1] += encode_start.elapsed().as_nanos();
                        parts[4] += 1;
                    }
                });
                self.apply_stable_trst_change(old, new)
            })
        })?;
        after.visit_wallet_metadata_delta(before, |old, new| {
            self.apply_stable_trst_change(
                old.map(|row| row.storage_entry())
                    .transpose()
                    .map_err(invalid)?,
                new.map(|row| row.storage_entry())
                    .transpose()
                    .map_err(invalid)?,
            )
        })?;
        if let Some(state) = successor {
            // The candidate was tied to the clean pre-write cut. Only this
            // synchronous delta's checked writes ran before publication.
            self.auxiliary_cache.get_mut().trst = Some(TrstCache {
                state,
                format: HoldingsFormat::Stable,
                dirty: SharedSet::default(),
                dirty_owners: SharedSet::default(),
                dirty_origins: SharedSet::default(),
                dirty_invalidations: SharedSet::default(),
            });
        }
        Ok(())
    }

    /// Stage graph and holdings changes, checking each changed row against `before`.
    /// Cache reuse requires a validated successor; otherwise a later read restores
    /// the full state. Abort the transaction on error: earlier rows may be staged.
    pub fn persist_stable_trst_transition(
        &mut self,
        before: &TrstEngine,
        after: &TrstEngine,
    ) -> Result<(), StoreError> {
        if self.trst_format()? != HoldingsFormat::Stable || !self.uses_graph_records()? {
            return Err(invalid(
                "combined TRST transition requires stable holdings and graph rows",
            ));
        }
        if before.merger_graph.shares_storage_with(&after.merger_graph) {
            return self.persist_stable_trst_delta(before, after);
        }
        #[cfg(test)]
        let stage_start = std::time::Instant::now();
        let successor = {
            let auxiliary = self.auxiliary_cache.borrow();
            match auxiliary.trst.as_ref().filter(|cache| {
                cache.format == HoldingsFormat::Stable
                    && cache.dirty.is_empty()
                    && cache.dirty_owners.is_empty()
                    && cache.dirty_origins.is_empty()
                    && cache.dirty_invalidations.is_empty()
            }) {
                Some(cache) => match cache
                    .state
                    .retained_append_successor(before, after)
                    .map_err(invalid)?
                {
                    Some(next) => Some(next),
                    None => cache
                        .state
                        .retained_revocation_successor(before, after)
                        .map_err(invalid)?,
                },
                None => None,
            }
        };
        #[cfg(test)]
        let proof_ns = stage_start.elapsed().as_nanos();
        #[cfg(test)]
        let stage_start = std::time::Instant::now();
        self.persist_graph_record_delta(&before.merger_graph, &after.merger_graph)?;
        #[cfg(test)]
        let graph_ns = stage_start.elapsed().as_nanos();
        #[cfg(test)]
        let stage_start = std::time::Instant::now();
        self.persist_stable_trst_delta(before, after)?;
        #[cfg(test)]
        TRANSITION_PROBE.with(|slot| {
            if let Some(samples) = slot.borrow_mut().as_mut() {
                samples.push([proof_ns, graph_ns, stage_start.elapsed().as_nanos()]);
            }
        });
        if let Some(state) = successor {
            self.auxiliary_cache.get_mut().trst = Some(TrstCache {
                state,
                format: HoldingsFormat::Stable,
                dirty: SharedSet::default(),
                dirty_owners: SharedSet::default(),
                dirty_origins: SharedSet::default(),
                dirty_invalidations: SharedSet::default(),
            });
        }
        Ok(())
    }

    fn apply_stable_trst_change(
        &mut self,
        old: Option<(String, Vec<u8>)>,
        new: Option<(String, Vec<u8>)>,
    ) -> Result<(), StoreError> {
        let key = new
            .as_ref()
            .or(old.as_ref())
            .ok_or_else(|| invalid("empty stable TRST delta"))?
            .0
            .as_str();
        if old
            .as_ref()
            .zip(new.as_ref())
            .is_some_and(|(a, b)| a.0 != b.0)
        {
            return Err(invalid("stable TRST delta changes its primary key"));
        }
        #[cfg(test)]
        let cas_start = std::time::Instant::now();
        if self.get_meta(key)?.as_deref() != old.as_ref().map(|(_, bytes)| bytes.as_slice()) {
            return Err(invalid("stable TRST delta conflicts with persisted state"));
        }
        #[cfg(test)]
        HOLDING_PROBE.with(|slot| {
            if let Some(parts) = slot.borrow_mut().as_mut() {
                parts[2] += cas_start.elapsed().as_nanos();
            }
        });
        #[cfg(test)]
        let write_start = std::time::Instant::now();
        let result = if let Some((_, bytes)) = &new {
            self.put_meta(key, bytes)
        } else {
            self.delete_meta(key)
        };
        #[cfg(test)]
        HOLDING_PROBE.with(|slot| {
            if let Some(parts) = slot.borrow_mut().as_mut() {
                parts[3] += write_start.elapsed().as_nanos();
            }
        });
        result
    }

    /// Replace stable holdings and ownership/invalidation metadata, leaving the graph.
    /// Collect all existing keys and visit every supplied portfolio and metadata row.
    /// Only changed bytes are written. Use the delta path for payments and abort
    /// the transaction on error, which may occur after earlier writes.
    pub fn replace_stable_trst_holdings(&mut self, engine: &TrstEngine) -> Result<(), StoreError> {
        if self.trst_format()? != HoldingsFormat::Stable {
            return Err(invalid("stable TRST replacement requires migrated storage"));
        }
        let mut old_keys = std::collections::BTreeSet::new();
        for prefix in [holding_rows::PREFIX, WalletRecord::STORAGE_PREFIX] {
            for row in self.checkpoint_metadata_entries(prefix.as_bytes())? {
                let (key, _) = row?;
                old_keys.insert(std::str::from_utf8(key).map_err(invalid)?.to_owned());
            }
        }
        let mut write = |key: String, bytes: Vec<u8>| {
            old_keys.remove(&key);
            if self.get_meta(&key)?.as_deref() != Some(bytes.as_slice()) {
                self.put_meta(&key, &bytes)?;
            }
            Ok::<_, StoreError>(())
        };
        for (owner, portfolio) in &engine.wallets {
            holding_rows::visit_delta(owner, None, Some(portfolio), invalid, |_, row| {
                let (key, bytes) = row
                    .ok_or_else(|| invalid("missing stable replacement row"))?
                    .storage_entry()
                    .map_err(invalid)?;
                write(key, bytes)
            })?;
        }
        engine.visit_wallet_metadata_delta(
            &TrstEngine::with_expiry(engine.expiry_secs),
            |old, new| {
                if old.is_some() {
                    return Err(invalid("replacement emitted an old metadata row"));
                }
                let (key, bytes) = new
                    .ok_or_else(|| invalid("missing replacement metadata row"))?
                    .storage_entry()
                    .map_err(invalid)?;
                write(key, bytes)
            },
        )?;
        for key in old_keys {
            self.delete_meta(&key)?;
        }
        Ok(())
    }

    fn read_stable_trst_portfolio(
        &self,
        owner: &WalletAddress,
        expiry_secs: u64,
    ) -> Result<Option<WalletPortfolio>, StoreError> {
        self.read_view()
            .read_stable_trst_portfolio(owner, expiry_secs)
    }

    pub fn read_merger_graph(&self) -> Result<MergerGraph, StoreError> {
        self.read_view().read_merger_graph()
    }

    fn read_trst_portfolio(
        &self,
        owner: &WalletAddress,
        expiry_secs: u64,
    ) -> Result<Option<WalletPortfolio>, StoreError> {
        if self.trst_format()? == HoldingsFormat::Stable {
            return self.read_stable_trst_portfolio(owner, expiry_secs);
        }
        let (key, prefix) = WalletRecord::portfolio_storage_keys(owner);
        let mut reader = WalletRecordReader::new(expiry_secs);
        if let Some(bytes) = self.undo_value_ref(UndoTable::Meta, key.as_bytes())? {
            reader
                .push(WalletRecord::from_storage_entry(&key, bytes).map_err(invalid)?)
                .map_err(invalid)?;
        }
        for row in self.checkpoint_metadata_entries(prefix.as_bytes())? {
            let (key, bytes) = row?;
            reader
                .push(
                    WalletRecord::from_storage_entry(
                        std::str::from_utf8(key).map_err(invalid)?,
                        bytes,
                    )
                    .map_err(invalid)?,
                )
                .map_err(invalid)?;
        }
        let mut engine = reader.finish().map_err(invalid)?;
        Ok(engine
            .wallets
            .remove(owner)
            .map(|portfolio| (*portfolio).clone()))
    }

    fn trst_cache_changes(&self, cache: &TrstCache) -> Result<RestoredTrstChanges, StoreError> {
        let expiry_secs = cache.state.engine().expiry_secs;
        let mut changes = RestoredTrstChanges::default();
        for owner in cache.dirty_owners.iter() {
            let key = WalletRecord::origin_owner_storage_key(owner);
            let present =
                if let Some(bytes) = self.undo_value_ref(UndoTable::Meta, key.as_bytes())? {
                    match WalletRecord::from_storage_entry(&key, bytes).map_err(invalid)? {
                        WalletRecord::OriginOwner(found) if &found == owner => true,
                        _ => return Err(invalid("TRST owner header key mismatch")),
                    }
                } else {
                    false
                };
            changes.owner_headers.insert(owner.clone(), present);
        }
        for origin in cache.dirty_origins.iter() {
            let key = WalletRecord::origin_storage_key(origin);
            let owner = if let Some(bytes) = self.undo_value_ref(UndoTable::Meta, key.as_bytes())? {
                match WalletRecord::from_storage_entry(&key, bytes).map_err(invalid)? {
                    WalletRecord::Origin {
                        owner,
                        origin: found,
                    } if &found == origin => Some(owner),
                    _ => return Err(invalid("TRST origin key mismatch")),
                }
            } else {
                None
            };
            changes.origins.insert(*origin, owner);
        }
        for origin in cache.dirty_invalidations.iter() {
            let (key, prefix) = WalletRecord::invalidation_storage_keys(origin);
            let header = self
                .undo_value_ref(UndoTable::Meta, key.as_bytes())?
                .map(|bytes| WalletRecord::from_storage_entry(&key, bytes).map_err(invalid))
                .transpose()?;
            let mut causes = std::collections::HashSet::new();
            for row in self.checkpoint_metadata_entries(prefix.as_bytes())? {
                let (key, bytes) = row?;
                match WalletRecord::from_storage_entry(
                    std::str::from_utf8(key).map_err(invalid)?,
                    bytes,
                )
                .map_err(invalid)?
                {
                    WalletRecord::Cause {
                        origin: found,
                        cause,
                    } if found == *origin && causes.insert(cause) => {}
                    _ => return Err(invalid("invalid scoped outcome cause")),
                }
            }
            let value = match header {
                Some(WalletRecord::Invalidation {
                    origin: found,
                    wallet_revoked,
                }) if found == *origin => Some(RestoredTrstInvalidation {
                    wallet_revoked,
                    causes,
                }),
                None if causes.is_empty() => None,
                _ => return Err(invalid("outcome causes lack their matching header")),
            };
            changes.invalidations.insert(*origin, value);
        }
        for owner in cache.dirty.iter() {
            changes
                .portfolios
                .insert(owner.clone(), self.read_trst_portfolio(owner, expiry_secs)?);
        }
        Ok(changes)
    }

    /// Rebase a payment candidate onto this writer's validated current state.
    /// The caller must supply every portfolio the payment read. `None` requires
    /// serial validation; `Some` is checked TRST state, not ledger authorization.
    /// This refreshes the current-state cache but writes no records and does not
    /// install the candidate in that cache.
    pub fn rebase_trst_payment(
        &self,
        before: &TrstEngine,
        after: &TrstEngine,
        read_wallets: &[WalletAddress],
        expiry_secs: u64,
    ) -> Result<Option<TrstEngine>, StoreError> {
        self.read_trst_engine(expiry_secs)?;
        let auxiliary = self.auxiliary_cache.borrow();
        let cache = auxiliary
            .trst
            .as_ref()
            .ok_or_else(|| invalid("TRST rebase cache missing"))?;
        cache
            .state
            .rebase_payment(before, after, read_wallets)
            .map(|state| state.map(RestoredTrstState::into_engine))
            .map_err(invalid)
    }

    /// Read combined holdings and graph state, including this writer's changes.
    /// Refresh changed portfolios, ownership and invalidations in the cached state.
    /// Raw graph or format writes and expiry-period changes discard that cache;
    /// a cache miss restores and validates the full state without a size limit.
    pub fn read_trst_engine(&self, expiry_secs: u64) -> Result<TrstEngine, StoreError> {
        let mut auxiliary = self.auxiliary_cache.borrow_mut();
        if auxiliary
            .trst
            .as_ref()
            .is_some_and(|cache| cache.state.engine().expiry_secs != expiry_secs)
        {
            auxiliary.trst = None;
        }
        if let Some(cache) = auxiliary.trst.as_mut() {
            if !cache.dirty.is_empty()
                || !cache.dirty_owners.is_empty()
                || !cache.dirty_origins.is_empty()
                || !cache.dirty_invalidations.is_empty()
            {
                let changes = self.trst_cache_changes(cache)?;
                cache.state.replace_records(changes).map_err(invalid)?;
                cache.dirty = SharedSet::default();
                cache.dirty_owners = SharedSet::default();
                cache.dirty_origins = SharedSet::default();
                cache.dirty_invalidations = SharedSet::default();
            }
            return Ok(cache.state.engine().clone());
        }
        let format = self.trst_format()?;
        let wallets = self
            .read_view()
            .read_trst_wallets_for_restoration(expiry_secs, &|_| Ok(()))?;
        let graph = self.read_merger_graph()?;
        let mut state = wallets
            .restore_with_poll(graph, &|_| Ok(()))
            .map_err(invalid)?;
        if format == HoldingsFormat::Stable {
            state.retain_portfolio_indexes().map_err(invalid)?;
        }
        let engine = state.engine().clone();
        auxiliary.trst = Some(TrstCache {
            state,
            format,
            dirty: SharedSet::default(),
            dirty_owners: SharedSet::default(),
            dirty_origins: SharedSet::default(),
            dirty_invalidations: SharedSet::default(),
        });
        Ok(engine)
    }
}

#[cfg(test)]
thread_local! {
    // Successor proof, holding encoding, CAS reads, raw writes, holding rows.
    // CAS/write totals also include changed ownership metadata rows.
    static HOLDING_PROBE: std::cell::RefCell<Option<[u128; 5]>> = const {
        std::cell::RefCell::new(None)
    };
    static TRANSITION_PROBE: std::cell::RefCell<Option<Vec<[u128; 3]>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
mod tests;

impl crate::read_view::LedgerReadView<'_> {
    fn economic_row_format(
        &self,
        marker: &str,
        aggregate: &str,
        prefix: &str,
    ) -> Result<bool, StoreError> {
        match self.get_meta(marker)?.as_deref() {
            None => {
                if let Some(row) = self.checkpoint_metadata_entries(prefix.as_bytes())?.next() {
                    row?;
                    return Err(invalid("economic rows exist without a format marker"));
                }
                Ok(false)
            }
            Some([1]) => {
                if self
                    .undo_value_ref(UndoTable::Meta, aggregate.as_bytes())?
                    .is_some()
                {
                    return Err(invalid("mixed aggregate and row economic state"));
                }
                Ok(true)
            }
            Some(_) => Err(invalid("unsupported economic storage format")),
        }
    }

    fn trst_format(&self) -> Result<HoldingsFormat, StoreError> {
        let marker = self.get_meta(HOLDINGS_FORMAT)?;
        if marker.as_deref() == Some(&[2]) {
            if self
                .undo_value_ref(UndoTable::Meta, TrstEngine::meta_key().as_bytes())?
                .is_some()
            {
                return Err(invalid("mixed aggregate and stable TRST state"));
            }
            for kind in ["0/", "1/"] {
                let prefix = format!("{}{kind}", WalletRecord::STORAGE_PREFIX);
                if let Some(row) = self.checkpoint_metadata_entries(prefix.as_bytes())?.next() {
                    row?;
                    return Err(invalid("mixed positional and stable TRST holdings"));
                };
            }
            return Ok(HoldingsFormat::Stable);
        }
        if let Some(row) = self
            .checkpoint_metadata_entries(holding_rows::PREFIX.as_bytes())?
            .next()
        {
            row?;
            return Err(invalid("stable TRST holdings require their format marker"));
        }
        self.economic_row_format(
            HOLDINGS_FORMAT,
            TrstEngine::meta_key(),
            WalletRecord::STORAGE_PREFIX,
        )
        .map(|rows| {
            if rows {
                HoldingsFormat::Positional
            } else {
                HoldingsFormat::Aggregate
            }
        })
    }

    pub fn uses_graph_records(&self) -> Result<bool, StoreError> {
        self.economic_row_format(GRAPH_FORMAT, GRAPH_SNAPSHOT, GraphRecord::STORAGE_PREFIX)
    }

    pub fn read_trst_holdings(&self, expiry_secs: u64) -> Result<TrstEngine, StoreError> {
        self.read_trst_holdings_with_poll(expiry_secs, &|_| Ok(()))
    }

    fn read_trst_holdings_with_poll(
        &self,
        expiry_secs: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<TrstEngine, StoreError> {
        let wallets = self.read_trst_wallets_for_restoration(expiry_secs, poll)?;
        with_engine_poll(poll, |poll| wallets.into_engine_with_poll(poll))
    }

    fn read_trst_wallets_for_restoration(
        &self,
        expiry_secs: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<UnindexedWallets, StoreError> {
        poll("storage:trst-holdings")?;
        let format = self.trst_format()?;
        if format == HoldingsFormat::Stable {
            return self.read_stable_trst_holdings(expiry_secs, poll);
        }
        if format == HoldingsFormat::Aggregate {
            let bytes = self
                .undo_value_ref(UndoTable::Meta, TrstEngine::meta_key().as_bytes())?
                .ok_or_else(|| invalid("missing persisted TRST holdings"))?;
            return TrstEngine::load_wallets_for_restoration(bytes, expiry_secs).map_err(invalid);
        }
        let mut reader = WalletRecordReader::new(expiry_secs);
        for row in self.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())? {
            poll("storage:trst-record")?;
            let (key, bytes) = row?;
            reader
                .push(
                    WalletRecord::from_storage_entry(
                        std::str::from_utf8(key).map_err(invalid)?,
                        bytes,
                    )
                    .map_err(invalid)?,
                )
                .map_err(invalid)?;
        }
        poll("storage:trst-holdings-finish")?;
        with_engine_poll(poll, |poll| reader.finish_for_restoration_with_poll(poll))
    }

    fn read_stable_trst_portfolio(
        &self,
        owner: &WalletAddress,
        expiry_secs: u64,
    ) -> Result<Option<WalletPortfolio>, StoreError> {
        self.read_stable_trst_portfolio_with_poll(owner, expiry_secs, &|_| Ok(()))
    }

    fn read_stable_trst_portfolio_with_poll(
        &self,
        owner: &WalletAddress,
        expiry_secs: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<Option<WalletPortfolio>, StoreError> {
        poll("storage:trst-portfolio")?;
        let (key, prefix) = holding_rows::portfolio_keys(owner);
        let rows = self.checkpoint_metadata_entries(prefix.as_bytes())?;
        let Some(bytes) = self.undo_value_ref(UndoTable::Meta, key.as_bytes())? else {
            if let Some(row) = rows.into_iter().next() {
                row?;
                return Err(invalid("stable TRST holding has no portfolio header"));
            }
            return Ok(None);
        };
        let header = holding_rows::Row::from_storage_entry(&key, bytes).map_err(invalid)?;
        // The holding decoder uses String errors. Preserve the original storage
        // or cancellation error when crossing that iterator boundary.
        let mut failure = None;
        let restored = with_engine_poll(poll, |engine_poll| {
            holding_rows::restore_with_poll(
                owner,
                header,
                rows.map(|row| {
                    let result = (|| {
                        poll("storage:trst-holding")?;
                        let (key, bytes) = row?;
                        holding_rows::Row::from_storage_entry(
                            std::str::from_utf8(key).map_err(invalid)?,
                            bytes,
                        )
                        .map_err(invalid)
                    })();
                    result.map_err(|error| {
                        let message = error.to_string();
                        failure = Some(error);
                        message
                    })
                }),
                expiry_secs,
                engine_poll,
            )
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let portfolio = restored?;
        poll("storage:trst-portfolio-finished")?;
        Ok(Some(portfolio))
    }

    fn read_stable_trst_holdings(
        &self,
        expiry_secs: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<UnindexedWallets, StoreError> {
        let mut reader = WalletRecordReader::new(expiry_secs);
        let mut restored_rows = 0usize;
        let header_prefix = format!("{}0/", holding_rows::PREFIX);
        for row in self.checkpoint_metadata_entries(header_prefix.as_bytes())? {
            poll("storage:trst-header")?;
            let (key, bytes) = row?;
            let header = holding_rows::Row::from_storage_entry(
                std::str::from_utf8(key).map_err(invalid)?,
                bytes,
            )
            .map_err(invalid)?;
            let holding_rows::Row::Header { holder, .. } = header else {
                return Err(invalid("stable TRST header namespace contains a holding"));
            };
            let portfolio = self
                .read_stable_trst_portfolio_with_poll(&holder, expiry_secs, poll)?
                .ok_or_else(|| {
                    invalid("stable TRST portfolio disappeared from pinned transaction")
                })?;
            restored_rows = restored_rows
                .checked_add(1)
                .and_then(|n| n.checked_add(portfolio.tokens.len()))
                .ok_or_else(|| invalid("stable TRST row count overflow"))?;
            reader
                .push(WalletRecord::Portfolio {
                    holder: holder.clone(),
                    cached_transferable: portfolio.cached_transferable,
                    earliest_expiry: portfolio.earliest_expiry,
                })
                .map_err(invalid)?;
            for (position, token) in portfolio.into_canonical().tokens.into_iter().enumerate() {
                poll("storage:trst-restored-holding")?;
                reader
                    .push(WalletRecord::Holding {
                        holder: holder.clone(),
                        position: position as u64,
                        token: Box::new(token),
                    })
                    .map_err(invalid)?;
            }
        }
        // Compare the stored row count with the rows reconstructed above to catch
        // orphan holdings and unknown keys without collecting another copy.
        let mut stored_rows = 0usize;
        for row in self.checkpoint_metadata_entries(holding_rows::PREFIX.as_bytes())? {
            poll("storage:trst-inventory")?;
            row?;
            stored_rows = stored_rows
                .checked_add(1)
                .ok_or_else(|| invalid("stable TRST row count overflow"))?;
        }
        if restored_rows != stored_rows {
            return Err(invalid("unclaimed stable TRST rows"));
        }
        for row in self.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())? {
            poll("storage:trst-record")?;
            let (key, bytes) = row?;
            let record =
                WalletRecord::from_storage_entry(std::str::from_utf8(key).map_err(invalid)?, bytes)
                    .map_err(invalid)?;
            if matches!(
                record,
                WalletRecord::Portfolio { .. } | WalletRecord::Holding { .. }
            ) {
                return Err(invalid("mixed positional and stable TRST holdings"));
            }
            reader.push(record).map_err(invalid)?;
        }
        poll("storage:trst-holdings-finish")?;
        with_engine_poll(poll, |poll| reader.finish_for_restoration_with_poll(poll))
    }

    pub fn read_merger_graph(&self) -> Result<MergerGraph, StoreError> {
        self.read_merger_graph_with_poll(&|_| Ok(()))
    }

    fn read_merger_graph_with_poll(
        &self,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<MergerGraph, StoreError> {
        poll("storage:trst-graph")?;
        if !self.uses_graph_records()? {
            let bytes = self
                .undo_value_ref(UndoTable::Meta, GRAPH_SNAPSHOT.as_bytes())?
                .ok_or_else(|| invalid("missing persisted merger graph"))?;
            return MergerGraph::from_bytes(bytes).map_err(invalid);
        }
        let mut reader = burst_trst::merger_graph::GraphRecordReader::default();
        for row in self.checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())? {
            poll("storage:trst-graph-record")?;
            let (key, bytes) = row?;
            reader
                .push(
                    GraphRecord::from_storage_entry(
                        std::str::from_utf8(key).map_err(invalid)?,
                        bytes,
                    )
                    .map_err(invalid)?,
                )
                .map_err(invalid)?;
        }
        poll("storage:trst-graph-finish")?;
        with_engine_poll(poll, |poll| reader.finish_with_poll(poll))
    }
}

impl crate::read_view::LedgerReadView<'_> {
    /// Full captured-view reconstruction. Ordinary writers retain their incremental cache.
    pub fn read_trst_engine(&self, expiry_secs: u64) -> Result<TrstEngine, StoreError> {
        self.restore_trst_engine(expiry_secs, true, &|_| Ok(()))
    }

    /// Fully validate the captured state without preparing portfolio indexes for
    /// future writes. Comparison and clock normalization can use the raw holdings,
    /// avoiding the stable format's additional O(t log t) cold index construction.
    pub fn read_trst_engine_for_comparison(
        &self,
        expiry_secs: u64,
    ) -> Result<TrstEngine, StoreError> {
        self.read_trst_engine_for_comparison_with_poll(expiry_secs, &|_| Ok(()))
    }

    /// Poll storage traversal and private validation/index construction. Legacy
    /// aggregate decoding and destruction remain synchronous; no hard time bound.
    pub fn read_trst_engine_for_comparison_with_poll(
        &self,
        expiry_secs: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<TrstEngine, StoreError> {
        self.restore_trst_engine(expiry_secs, false, poll)
    }

    fn restore_trst_engine(
        &self,
        expiry_secs: u64,
        retain_indexes: bool,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<TrstEngine, StoreError> {
        let format = self.trst_format()?;
        let wallets = self.read_trst_wallets_for_restoration(expiry_secs, poll)?;
        let graph = self.read_merger_graph_with_poll(poll)?;
        poll("storage:trst-validation")?;
        let engine = if retain_indexes {
            let mut state = with_engine_poll(poll, |poll| wallets.restore_with_poll(graph, poll))?;
            if format == HoldingsFormat::Stable {
                state.retain_portfolio_indexes().map_err(invalid)?;
            }
            state.into_engine()
        } else {
            with_engine_poll(poll, |poll| {
                wallets.restore_for_comparison_with_poll(graph, poll)
            })?
        };
        poll("storage:trst-restored")?;
        Ok(engine)
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn uses_stable_trst_records(&self) -> Result<bool, StoreError> {
        Ok(self.trst_format()? == HoldingsFormat::Stable)
    }
}
