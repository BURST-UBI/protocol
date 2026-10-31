//! Decoded engines and derived indexes cached by LMDB generation. Writers clone
//! the matching cache, update or invalidate affected entries, then publish after
//! commit. Read snapshots may populate entries only for their pinned generation.
//! Environment read helpers require a view created by that same environment.
use super::*;
use bincode::Options;
use burst_consti::ConstiEngine;
use burst_governance::DelegationEngine;
mod brn;
mod trst;
mod expiry_counters;
mod pending_expiry;
mod pending_provenance;
mod confirmations;
mod confirmed_authority;
mod confirmed_governance;

#[derive(Clone, Default)]
pub(crate) struct AuxiliaryCache {
    pub(super) confirmed_governance: Option<crate::confirmed_governance::Cached>,
    pub(super) confirmed_authority: Option<crate::confirmed_authority::Cached>,
    pub(super) confirmations: Option<burst_utils::SharedSet<BlockHash>>,
    projection_identity: Option<usize>,
    snapshot_base_identity: Option<usize>,
    pub(super) payment_authority_identity: Option<usize>,
    brn: Option<brn::BrnCache>,
    brn_rates: Option<burst_brn::RateHistory>,
    electorate: Option<std::sync::Arc<[WalletAddress]>>,
    expiry_counters: Option<expiry_counters::CounterSelection>,
    pending_expiry: Option<pending_expiry::PendingSelection>,
    pending_provenance: Option<pending_provenance::PendingDependencies>,
    trst: Option<trst::TrstCache>,
    delegation: Option<DelegationEngine>,
    constitution: Option<ConstiEngine>,
    pub(super) verification: Option<burst_verification::VerificationOrchestrator>,
}

impl AuxiliaryCache {
    pub(super) fn clear_electorate(&mut self, table: UndoTable) {
        if table == UndoTable::Accounts { self.electorate = None; self.projection_identity = None; }
    }

    pub(super) fn invalidate_projection(&mut self) {
        self.projection_identity = None;
    }

    pub(super) fn invalidate(&mut self, key: &[u8]) {
        if key.starts_with(b"checkpoint_snapshot/") {
            self.snapshot_base_identity = None;
        }
        if key.starts_with(b"confirmed_governance/") { self.confirmed_governance = None; }
        if key.starts_with(b"confirmed_authority/") { self.confirmed_authority = None; }
        if key == burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY.as_bytes()
            || key.starts_with(burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX.as_bytes())
            || key.starts_with(b"payment_epoch/")
            || key.starts_with(b"confirmed_governance/")
        {
            self.payment_authority_identity = None;
        }
        if key == DelegationEngine::meta_key().as_bytes()
            || key == b"protocol_params"
            || key == b"governance_records_format"
            || key.starts_with(b"governance_state/")
        {
            self.invalidate_projection();
        }
        if key == b"verification_orchestrator"
            || key == b"verification_records_format"
            || key.starts_with(b"verification_state/")
        {
            self.verification = None;
        }
        if key == DelegationEngine::meta_key().as_bytes() {
            self.delegation = None;
        } else if key == b"consti_engine" {
            self.constitution = None;
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.confirmed_governance.is_none()
            && self.confirmed_authority.is_none()
            && self.confirmations.is_none()
            && self.payment_authority_identity.is_none()
            && self.projection_identity.is_none()
            && self.snapshot_base_identity.is_none()
            && self.electorate.is_none()
            && self.pending_expiry.is_none()
            && self.pending_provenance.is_none()
            && self.expiry_counters.is_none()
            && self.brn.is_none()
            && self.brn_rates.is_none()
            && self.trst.is_none()
            && self.verification.is_none()
            && self.delegation.is_none()
            && self.constitution.is_none()
    }
}

// Identities never wrap or reuse values, and retain no input snapshots.
static NEXT_PROJECTION_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

impl LmdbEnvironment {
    /// Speculative reads may share only complete, clean engines for this exact
    /// environment and pinned generation. A miss never restores state, repairs
    /// caches or queues for the LMDB writer; authoritative admission does that.
    pub fn cached_payment_engines(
        &self, view: &LmdbReadTransaction<'_>, expiry_secs: u64, max_rows: usize, max_bytes: usize,
    ) -> Result<Option<(burst_brn::BrnEngine, burst_trst::TrstEngine)>, StoreError> {
        if !view.belongs_to(self) { return Ok(None); }
        let Some(generation) = view.generation else { return Ok(None); };
        // Vote reservations retain this mutex through their durable commit.
        // Speculation must not queue behind that writer indirectly either.
        let slot = match self.auxiliary_cache.try_lock() {
            Ok(slot) => slot,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(StoreError::Backend("payment cache lock poisoned".into()));
            }
        };
        let Some((_, cache)) = slot.as_ref().filter(|(id, _)| *id == generation) else { return Ok(None); };
        let (Some(brn), Some(trst)) = (&cache.brn, &cache.trst) else { return Ok(None); };
        // The complete cached TRST reader checked mixed/unknown rows at this cut.
        // Retain preparation's normalized-graph eligibility without a cold scan.
        if self.meta_db.get(&view.txn, b"trst_graph_format").map_err(LmdbError::from)? != Some(&[1][..]) {
            return Ok(None);
        }
        let Some(brn) = brn.clean_snapshot(max_rows, max_bytes)? else { return Ok(None); };
        let Some(trst) = trst.clean_snapshot(expiry_secs) else { return Ok(None); };
        Ok(Some((brn, trst)))
    }

    /// Local reuse token for display-tally inputs. The view must belong to this
    /// environment. Managed unrelated commits carry the token forward; an
    /// unknown generation or exhausted identity space returns None.
    pub fn read_governance_projection_identity(&self, view: &LmdbReadTransaction<'_>) -> Option<usize> {
        let generation = view.generation?;
        let mut slot = self.auxiliary_cache.lock().unwrap();
        if slot.as_ref().is_some_and(|(id, _)| *id > generation) { return None; }
        if slot.as_ref().is_none_or(|(id, _)| *id != generation) {
            *slot = Some((generation, AuxiliaryCache::default()));
        }
        let cache = &mut slot.as_mut().unwrap().1;
        if cache.projection_identity.is_none() {
            cache.projection_identity = NEXT_PROJECTION_ID.try_update(
                std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1)).ok();
        }
        cache.projection_identity
    }

    /// Local reuse token for payment-authority inputs, not a certificate or a
    /// check that the descriptor exists. The view must belong to this environment.
    /// Managed unrelated commits retain the token; untracked commits require a
    /// new token.
    pub fn read_payment_authority_identity(&self, view: &LmdbReadTransaction<'_>) -> Option<usize> {
        let generation = view.generation?;
        let mut slot = self.auxiliary_cache.lock().unwrap();
        if slot.as_ref().is_some_and(|(id, _)| *id > generation) { return None; }
        if slot.as_ref().is_none_or(|(id, _)| *id != generation) {
            *slot = Some((generation, AuxiliaryCache::default()));
        }
        let cache = &mut slot.as_mut().unwrap().1;
        if cache.payment_authority_identity.is_none() {
            cache.payment_authority_identity = NEXT_PROJECTION_ID.try_update(
                std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1)).ok();
        }
        cache.payment_authority_identity
    }

    /// Identity of all installed snapshot bytes at this pinned view, including
    /// descriptor and pages. It is not authentication. Unknown/foreign views,
    /// lock contention and exhausted identity space cannot establish reuse.
    pub fn read_snapshot_base_identity(&self, view: &LmdbReadTransaction<'_>) -> Option<usize> {
        if !view.belongs_to(self) { return None; }
        let generation = view.generation?;
        let mut slot = self.auxiliary_cache.try_lock().ok()?;
        if slot.as_ref().is_some_and(|(id, _)| *id > generation) { return None; }
        if slot.as_ref().is_none_or(|(id, _)| *id != generation) {
            *slot = Some((generation, AuxiliaryCache::default()));
        }
        let cache = &mut slot.as_mut().unwrap().1;
        if cache.snapshot_base_identity.is_none() {
            cache.snapshot_base_identity = NEXT_PROJECTION_ID.try_update(
                std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1)).ok();
        }
        cache.snapshot_base_identity
    }

    /// One retained electorate per environment, bounded to 8 MiB of address
    /// payload plus inline element storage. Larger sets still work uncached.
    pub fn read_verified_snapshot(
        &self, view: &LmdbReadTransaction<'_>,
    ) -> Result<std::sync::Arc<[WalletAddress]>, StoreError> {
        self.read_verified_snapshot_with_limit(view, 8 << 20)
    }

    fn read_verified_snapshot_with_limit(
        &self, view: &LmdbReadTransaction<'_>, retain_limit: usize,
    ) -> Result<std::sync::Arc<[WalletAddress]>, StoreError> {
        if let Some(generation) = view.generation {
            let cache = self.auxiliary_cache.lock().unwrap();
            if let Some((_, cache)) = cache.as_ref().filter(|(id, _)| *id == generation) {
                if let Some(addresses) = &cache.electorate { return Ok(addresses.clone()); }
            }
        }
        let addresses: std::sync::Arc<[WalletAddress]> =
            crate::verified_index::addresses(&view.txn, self.verified_accounts_db)?.into();
        let retained = addresses.iter().try_fold(0usize, |used, address| {
            used.checked_add(std::mem::size_of::<WalletAddress>())?
                .checked_add(address.as_str().len()).filter(|size| *size <= retain_limit)
        });
        if retained.is_some() {
            self.publish_read_auxiliary(view.generation, |cache| cache.electorate = Some(addresses.clone()));
        }
        Ok(addresses)
    }

    /// Return an immutable delegation image for this pinned database view.
    pub fn read_delegation_snapshot(
        &self, view: &LmdbReadTransaction<'_>,
    ) -> Result<Option<DelegationEngine>, StoreError> {
        if let Some(generation) = view.generation {
            let cache = self.auxiliary_cache.lock().unwrap();
            if let Some((_, cache)) = cache.as_ref().filter(|(id, _)| *id == generation) {
                if let Some(engine) = &cache.delegation { return Ok(Some(engine.clone())); }
            }
        }
        let Some(bytes) = self.meta_db.get(&view.txn, DelegationEngine::meta_key().as_bytes())
            .map_err(LmdbError::from)? else { return Ok(None) };
        let engine = DelegationEngine::load_state(bytes)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        self.publish_read_auxiliary(view.generation, |cache| cache.delegation = Some(engine.clone()));
        Ok(Some(engine))
    }

    pub fn read_constitution_snapshot(
        &self, view: &LmdbReadTransaction<'_>,
    ) -> Result<Option<ConstiEngine>, StoreError> {
        if let Some(generation) = view.generation {
            let cache = self.auxiliary_cache.lock().unwrap();
            if let Some((_, cache)) = cache.as_ref().filter(|(id, _)| *id == generation) {
                if let Some(engine) = &cache.constitution { return Ok(Some(engine.clone())); }
            }
        }
        let Some(bytes) = self.meta_db.get(&view.txn, b"consti_engine")
            .map_err(LmdbError::from)? else { return Ok(None) };
        let engine: ConstiEngine = bincode::DefaultOptions::new().with_fixint_encoding()
            .with_limit(bytes.len() as u64).reject_trailing_bytes().deserialize(bytes)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        self.publish_read_auxiliary(view.generation, |cache| cache.constitution = Some(engine.clone()));
        Ok(Some(engine))
    }

    fn publish_read_auxiliary(&self, generation: Option<usize>, publish: impl FnOnce(&mut AuxiliaryCache)) {
        let Some(generation) = generation else { return };
        let mut slot = self.auxiliary_cache.lock().unwrap();
        if slot.as_ref().is_some_and(|(id, _)| *id > generation) { return; }
        if slot.as_ref().is_none_or(|(id, _)| *id != generation) {
            *slot = Some((generation, AuxiliaryCache::default()));
        }
        // Only this view's tag is published, never the latest environment ID.
        // Merge same-generation fields; an old reader cannot overwrite a newer image.
        publish(&mut slot.as_mut().unwrap().1);
    }
}

impl LmdbWriteTransaction<'_> {
    /// Read an isolated shared candidate from the current writer view.
    pub fn read_delegation_engine(&self) -> Result<Option<DelegationEngine>, StoreError> {
        if let Some(engine) = self.auxiliary_cache.borrow().delegation.as_ref() {
            return Ok(Some(engine.clone()));
        }
        let Some(bytes) =
            self.undo_value_ref(UndoTable::Meta, DelegationEngine::meta_key().as_bytes())?
        else {
            return Ok(None);
        };
        let engine = DelegationEngine::load_state(bytes)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        self.auxiliary_cache.borrow_mut().delegation = Some(engine.clone());
        Ok(Some(engine))
    }

    /// Missing or malformed snapshots are never replaced with a default engine.
    pub fn read_constitution_engine(&self) -> Result<Option<ConstiEngine>, StoreError> {
        if let Some(engine) = self.auxiliary_cache.borrow().constitution.as_ref() {
            return Ok(Some(engine.clone()));
        }
        let Some(bytes) = self.undo_value_ref(UndoTable::Meta, b"consti_engine")? else {
            return Ok(None);
        };
        let engine: ConstiEngine = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        self.auxiliary_cache.borrow_mut().constitution = Some(engine.clone());
        Ok(Some(engine))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
fn probe_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("Linux scaling host");
    status.lines().find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .unwrap().parse().unwrap()
}

#[cfg(test)]
pub(super) fn reserve_test_votes(store: &crate::LmdbStore) {
    let meta = store.meta_store();
    let epoch = BlockHash::new([1; 32]);
    let representative = WalletAddress::new("brst_cache_vote");
    let previous = BlockHash::ZERO;
    let choice = BlockHash::new([2; 32]);
    for final_vote in [false, true] {
        meta.reserve_consensus_vote(&epoch, &representative, &representative,
            &previous, &choice, final_vote, 0).unwrap();
    }
    assert!(meta.reserve_consensus_vote(&epoch, &representative, &representative,
        &previous, &BlockHash::new([3; 32]), true, 0).is_err());
}
