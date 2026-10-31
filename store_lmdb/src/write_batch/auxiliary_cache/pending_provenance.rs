//! Reverse indexes from token origins and revoked chunks to pending transfers.
//! A cold cache decodes all pending rows; warm calls refresh changed rows and
//! graph dependencies, then decode selected results. Selection returns whole
//! rows in primary-key order, without pagination or changes to stored claims.
use super::*;
use burst_store::pending::{validate_pending_provenance, PendingInfo};
use burst_trst::{MergerGraph};
use burst_utils::{SharedMap, SharedSet};
use burst_types::TrstBasis;
use std::sync::Arc;

type Entry = (WalletAddress, TxHash, PendingInfo);
type Reverse = SharedMap<TxHash, PendingKeys>;

#[derive(Clone, Default)]
enum PendingKeys {
    #[default]
    Empty,
    One(Arc<Vec<u8>>),
    Many(SharedSet<Vec<u8>>),
}

impl PendingKeys {
    fn insert(&mut self, key: Vec<u8>) {
        match self {
            Self::Empty => *self = Self::One(Arc::new(key)),
            Self::One(first) if first.as_ref() != &key => {
                let mut keys = SharedSet::default();
                keys.insert(first.as_ref().clone()); keys.insert(key);
                *self = Self::Many(keys);
            }
            Self::One(_) => {}
            Self::Many(keys) => { keys.insert(key); }
        }
    }

    fn remove(&mut self, key: &Vec<u8>) {
        let replacement = match self {
            Self::One(first) if first.as_ref() == key => Some(Self::Empty),
            Self::Many(keys) => {
                keys.remove(key);
                match keys.len() {
                    0 => Some(Self::Empty),
                    1 => Some(Self::One(Arc::new(keys.iter().next().unwrap().clone()))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(replacement) = replacement { *self = replacement; }
    }

    fn is_empty(&self) -> bool { matches!(self, Self::Empty) }

    fn iter(&self) -> impl Iterator<Item = &Vec<u8>> {
        let (one, many) = match self {
            Self::Empty => (None, None),
            Self::One(key) => (Some(key.as_ref()), None),
            Self::Many(keys) => (None, Some(keys)),
        };
        one.into_iter().chain(many.into_iter().flat_map(|keys| keys.iter()))
    }

    #[cfg(test)]
    fn contains(&self, key: &Vec<u8>) -> bool {
        match self {
            Self::Empty => false,
            Self::One(first) => first.as_ref() == key,
            Self::Many(keys) => keys.contains(key),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Dependencies {
    origins: Box<[TxHash]>,
    chunks: Box<[TxHash]>,
    unrestored: Box<[TxHash]>,
    invalid: Option<String>,
    bases: Vec<(TxHash, TrstBasis)>,
}

impl Dependencies {
    fn read(info: &PendingInfo) -> Self {
        let mut result = Self::default();
        if info.return_credit { return result; }
        let mut origins = Vec::new();
        let mut chunks = Vec::new();
        let mut unrestored = Vec::new();
        result.invalid = validate_pending_provenance(info).err();
        for portion in &info.provenance {
            origins.push(portion.origin);
            if let TrstBasis::Single(origin) = portion.basis { origins.push(origin); }
            let split = portion.revoked_amounts.values()
                .try_fold(0u128, |sum, chunk| sum.checked_add(chunk.amount));
            if split.is_some_and(|split| split < portion.amount) && portion.basis != TrstBasis::Entire {
                result.bases.push((portion.origin, portion.basis));
            }
            for (origin, chunk) in &portion.revoked_amounts {
                let origin = TxHash::new(*origin);
                chunks.push(origin);
                if !chunk.restored { unrestored.push(origin); }
            }
        }
        result.origins = compact_origins(origins);
        result.chunks = compact_origins(chunks);
        result.unrestored = compact_origins(unrestored);
        result
    }
}

fn compact_origins(mut origins: Vec<TxHash>) -> Box<[TxHash]> {
    origins.sort_unstable();
    origins.dedup();
    origins.into_boxed_slice()
}

#[derive(Clone, Default)]
pub(super) struct PendingDependencies {
    // Row dependencies are replaced together; sharing the record keeps persistent
    // map-path clones cheap without a separate tree for each small origin set.
    rows: SharedMap<Vec<u8>, Arc<Dependencies>>,
    origins: Reverse,
    chunks: Reverse,
    unrestored: Reverse,
    invalid: SharedMap<Vec<u8>, String>,
    dirty: SharedSet<Vec<u8>>,
    basis_dirty: SharedSet<Vec<u8>>,
    basis_origins: Reverse,
    basis_revisions: Reverse,
    basis_graph: Option<MergerGraph>,
    restoration_revoked: Option<SharedSet<TxHash>>,
    restoration_dirty: SharedSet<TxHash>,
    restorable: SharedSet<TxHash>,
}

fn remove(index: &mut Reverse, origins: &[TxHash], key: &Vec<u8>) {
    for origin in origins.iter() {
        if let Some(keys) = index.get_mut(origin) {
            keys.remove(key);
            if keys.is_empty() { index.remove(origin); }
        }
    }
}

fn insert(index: &mut Reverse, origins: &[TxHash], key: &Vec<u8>) {
    for origin in origins.iter() {
        index.entry(*origin).or_default().insert(key.to_vec());
    }
}

fn basis_dependencies(row: &Dependencies) -> (Box<[TxHash]>, Box<[TxHash]>) {
    let mut origins = Vec::new();
    let mut revisions = Vec::new();
    for (origin, basis) in &row.bases {
        origins.push(*origin);
        if let TrstBasis::Remainder(revision) = basis { revisions.push(*revision); }
    }
    (compact_origins(origins), compact_origins(revisions))
}

impl PendingDependencies {
    fn update(&mut self, key: Vec<u8>, info: Option<&PendingInfo>) {
        self.basis_dirty.remove(&key);
        if let Some(old) = self.rows.remove(&key) {
            self.restoration_dirty.extend(old.unrestored.iter().copied());
            let (origins, revisions) = basis_dependencies(&old);
            remove(&mut self.basis_origins, &origins, &key);
            remove(&mut self.basis_revisions, &revisions, &key);
            remove(&mut self.origins, &old.origins, &key);
            remove(&mut self.chunks, &old.chunks, &key);
            remove(&mut self.unrestored, &old.unrestored, &key);
            self.invalid.remove(&key);
        }
        if let Some(info) = info {
            let next = Dependencies::read(info);
            self.restoration_dirty.extend(next.unrestored.iter().copied());
            if !next.bases.is_empty() { self.basis_dirty.insert(key.clone()); }
            let (origins, revisions) = basis_dependencies(&next);
            insert(&mut self.basis_origins, &origins, &key);
            insert(&mut self.basis_revisions, &revisions, &key);
            insert(&mut self.origins, &next.origins, &key);
            insert(&mut self.chunks, &next.chunks, &key);
            insert(&mut self.unrestored, &next.unrestored, &key);
            if let Some(error) = &next.invalid { self.invalid.insert(key.clone(), error.clone()); }
            self.rows.insert(key, Arc::new(next));
        }
    }

    fn refresh_restorable(&mut self, revoked: &SharedSet<TxHash>) {
        let mut changed = std::mem::take(&mut self.restoration_dirty);
        if let Some(before) = &self.restoration_revoked {
            changed.extend(revoked.changed_keys(before).copied());
        } else {
            changed.extend(self.unrestored.keys().copied());
        }
        for origin in changed.iter() {
            if self.unrestored.contains_key(origin) && !revoked.contains(origin) {
                self.restorable.insert(*origin);
            } else {
                self.restorable.remove(origin);
            }
        }
        self.restoration_revoked = Some(revoked.clone());
    }
}

impl AuxiliaryCache {
    pub(crate) fn mark_pending_provenance(&mut self, table: UndoTable, key: &[u8]) {
        if table == UndoTable::Pending {
            if let Some(cache) = self.pending_provenance.as_mut() { cache.dirty.insert(key.to_vec()); }
        }
    }
}

impl PendingDependencies {
    fn validate_bases(&mut self, graph: &MergerGraph) -> Result<(), StoreError> {
        let cache = self;
        let mut checks = std::mem::take(&mut cache.basis_dirty);
        if let Some(before) = &cache.basis_graph {
            let (merges, revisions) = graph.basis_validation_changes(before);
            for (changed, index) in [(merges, &cache.basis_origins), (revisions, &cache.basis_revisions)] {
                for id in changed {
                    if let Some(rows) = index.get(&id) { checks.extend(rows.iter().cloned()); }
                }
            }
        } else {
            checks.extend(cache.basis_origins.values().flat_map(|rows| rows.iter().cloned()));
        }
        for key in checks.iter() {
            if cache.rows.get(key).is_some_and(|row| row.bases.iter()
                .any(|(origin, basis)| !graph.valid_basis(origin, *basis))) {
                return Err(StoreError::Corruption("invalid pending composition basis".into()));
            }
        }
        cache.basis_graph = Some(graph.clone());
        Ok(())
    }
}

enum Selection<'a> {
    Revocation(&'a [TxHash], &'a MergerGraph),
    Restoration(&'a MergerGraph),
    ValidateBases(&'a MergerGraph),
    Prepare(&'a MergerGraph),
}

impl LmdbWriteTransaction<'_> {
    /// Select ordinary pending rows referencing the supplied origins or revoked
    /// chunks. Reject any cached provenance-validation error, even in unrelated
    /// rows. A nonempty origin list also validates affected composition bases
    /// against the caller-supplied graph for this writer's state.
    pub fn pending_for_revocation(&mut self, origins: &[TxHash], graph: &MergerGraph) -> Result<Vec<Entry>, StoreError> {
        self.select_pending_dependencies(Selection::Revocation(origins, graph)).map(|(rows, _)| rows)
    }

    /// Select ordinary pending rows with any unrestored chunk whose origin is
    /// absent from the graph's revoked set. This does not validate composition
    /// bases or reject cached semantic errors; it decodes selected rows and
    /// checks their dependencies against the cache. The caller applies restoration.
    pub fn pending_for_restoration(&mut self, graph: &MergerGraph) -> Result<Vec<Entry>, StoreError> {
        self.select_pending_dependencies(Selection::Restoration(graph)).map(|(rows, _)| rows)
    }

    /// Prepare dependency and restoration indexes at a validated economic cut
    /// before opening admission. Cold work scans all pending rows; commit is
    /// required to publish the cache, and later writer changes remain tracked.
    /// The caller supplies the graph belonging to this same writer's state.
    pub fn prepare_pending_dependencies(&mut self, graph: &MergerGraph) -> Result<(), StoreError> {
        self.select_pending_dependencies(Selection::Prepare(graph)).map(|_| ())
    }

    pub(crate) fn validate_pending_bases(&mut self, graph: &MergerGraph) -> Result<(), StoreError> {
        self.select_pending_dependencies(Selection::ValidateBases(graph)).map(|_| ())
    }

    fn select_pending_dependencies(&mut self, selection: Selection<'_>) -> Result<(Vec<Entry>, usize), StoreError> {
        let result = (|| {
            let previous = self.auxiliary_cache.get_mut().pending_provenance.take();
            let cold = previous.is_none();
            let mut cache = previous.unwrap_or_default();
            let mut decoded = 0;
            if cold {
                for row in self.env.pending_db.iter(&self.txn).map_err(LmdbError::from)? {
                    let (key, bytes) = row.map_err(LmdbError::from)?;
                    let (_, _, info) = crate::pending::decode_pending_entry(key, bytes)?;
                    decoded += 1;
                    cache.update(key.to_vec(), Some(&info));
                }
            } else {
                for key in std::mem::take(&mut cache.dirty).iter() {
                    let entry = self.env.pending_db.get(&self.txn, key).map_err(LmdbError::from)?
                        .map(|bytes| crate::pending::decode_pending_entry(key, bytes)).transpose()?;
                    decoded += usize::from(entry.is_some());
                    cache.update(key.clone(), entry.as_ref().map(|(_, _, info)| info));
                }
            }
            let mut keys = SharedSet::default();
            match selection {
                Selection::Revocation(origins, graph) => {
                    if let Some((_, error)) = cache.invalid.iter().next() {
                        return Err(StoreError::Corruption(error.clone()));
                    }
                    if !origins.is_empty() { cache.validate_bases(graph)?; }
                    for origin in origins {
                        for index in [&cache.origins, &cache.chunks] {
                            if let Some(found) = index.get(origin) { keys.extend(found.iter().cloned()); }
                        }
                    }
                }
                Selection::ValidateBases(graph) => {
                    if let Some((_, error)) = cache.invalid.iter().next() {
                        return Err(StoreError::Corruption(error.clone()));
                    }
                    cache.validate_bases(graph)?;
                }
                Selection::Prepare(graph) => {
                    if let Some((_, error)) = cache.invalid.iter().next() {
                        return Err(StoreError::Corruption(error.clone()));
                    }
                    cache.validate_bases(graph)?;
                    cache.refresh_restorable(graph.revoked_origins());
                }
                Selection::Restoration(graph) => {
                    cache.refresh_restorable(graph.revoked_origins());
                    for origin in cache.restorable.iter() {
                        if let Some(found) = cache.unrestored.get(origin) {
                            keys.extend(found.iter().cloned());
                        }
                    }
                }
            }
            let mut entries = Vec::with_capacity(keys.len());
            for key in keys.iter() {
                let bytes = self.env.pending_db.get(&self.txn, key).map_err(LmdbError::from)?
                    .ok_or_else(|| StoreError::Corruption("pending provenance index has no primary row".into()))?;
                let entry = crate::pending::decode_pending_entry(key, bytes)?;
                decoded += 1;
                if cache.rows.get(key).map(Arc::as_ref) != Some(&Dependencies::read(&entry.2)) {
                    return Err(StoreError::Corruption("pending provenance index disagrees with primary row".into()));
                }
                entries.push(entry);
            }
            self.auxiliary_cache.get_mut().pending_provenance = Some(cache);
            Ok((entries, decoded))
        })();
        if result.is_err() { self.failed = true; }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::pending::PendingProvenance;
    use burst_types::{TrstBasis, TrstRevocationChunk};

    fn id(n: u64) -> TxHash {
        let mut bytes = [0; 32]; bytes[..8].copy_from_slice(&n.to_be_bytes()); TxHash::new(bytes)
    }
    fn address(n: u64) -> WalletAddress { WalletAddress::new(format!("brst_pending_dependencies_{n:04}")) }
    fn info(n: u64) -> PendingInfo {
        PendingInfo { source: address(999), amount: 10, timestamp: Timestamp::new(100), return_credit: false,
            provenance: vec![PendingProvenance { basis: TrstBasis::Entire, amount: 10, origin: id(n),
                origin_wallet: address(n), origin_timestamp: Timestamp::new(100),
                effective_origin_timestamp: Timestamp::new(100), revoked_amounts: Default::default() }] }
    }
    fn seed(batch: &mut LmdbWriteTransaction<'_>) {
        for n in 0..64 {
            let mut row = info(n);
            if n == 1 || n == 2 {
                row.provenance[0].revoked_amounts.insert(*id(500).as_bytes(), TrstRevocationChunk { amount: 3, restored: n == 1 });
            }
            batch.put_pending(&address(n), id(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        }
    }
    fn selected(batch: &mut LmdbWriteTransaction<'_>) -> (Vec<u64>, usize) {
        let (rows, decoded) = batch.select_pending_dependencies(Selection::Revocation(&[id(500)], &MergerGraph::new())).unwrap();
        (rows.iter().map(|(_, hash, _)| u64::from_be_bytes(hash.as_bytes()[..8].try_into().unwrap())).collect(), decoded)
    }

    #[test]
    fn preparation_publishes_only_on_commit_and_rejects_changed_invalid_basis() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let graph = MergerGraph::new();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch);
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.prepare_pending_dependencies(&graph).unwrap();
        assert!(batch.auxiliary_cache.borrow().pending_provenance.as_ref().unwrap().basis_graph.is_some());
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.auxiliary_cache.borrow().pending_provenance.is_none());
        batch.prepare_pending_dependencies(&graph).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2], 2));
        let (restorable, decoded) = batch.select_pending_dependencies(Selection::Restoration(&graph)).unwrap();
        assert_eq!(decoded, 1); assert_eq!(restorable[0].1, id(2));
        let mut invalid = info(0);
        invalid.provenance[0].basis = TrstBasis::Remainder(id(999));
        batch.put_pending(&address(0), id(0).as_bytes(), &bincode::serialize(&invalid).unwrap()).unwrap();
        assert!(batch.prepare_pending_dependencies(&graph).is_err());
        assert!(batch.commit().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch.prepare_pending_dependencies(&graph).unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2], 2));
    }

    #[test]
    fn reverse_membership_promotes_demotes_and_preserves_shared_ordered_cuts() {
        let mut keys = PendingKeys::default();
        let mut expected = std::collections::BTreeSet::new();
        for n in (0u64..1000).rev().chain(0..1000) {
            let key = n.to_be_bytes().to_vec();
            keys.insert(key.clone()); expected.insert(key);
        }
        assert_eq!(keys.iter().cloned().collect::<Vec<_>>(), expected.iter().cloned().collect::<Vec<_>>());
        let before = keys.clone();
        for n in 0u64..999 {
            let key = n.to_be_bytes().to_vec();
            keys.remove(&key); expected.remove(&key);
        }
        assert!(matches!(keys, PendingKeys::One(_)));
        assert_eq!(keys.iter().cloned().collect::<Vec<_>>(), expected.iter().cloned().collect::<Vec<_>>());
        assert_eq!(before.iter().count(), 1000);
        let singleton = keys.clone();
        if let (PendingKeys::One(a), PendingKeys::One(b)) = (&keys, &singleton) {
            assert!(Arc::ptr_eq(a, b));
        } else { panic!("singleton representation lost"); }
        keys.remove(&999u64.to_be_bytes().to_vec());
        assert!(keys.is_empty());
        assert_eq!(singleton.iter().count(), 1);
        keys.remove(&999u64.to_be_bytes().to_vec());
        assert!(keys.is_empty());
    }

    #[test]
    fn compact_dependencies_preserve_duplicate_origin_membership_and_shared_cuts() {
        let mut row = info(9);
        row.provenance[0].basis = TrstBasis::Single(id(3));
        row.provenance[0].revoked_amounts.insert(*id(8).as_bytes(), TrstRevocationChunk { amount: 3, restored: true });
        let mut other = row.provenance[0].clone();
        other.revoked_amounts.get_mut(id(8).as_bytes()).unwrap().restored = false;
        row.provenance.push(other); row.amount = 20;
        validate_pending_provenance(&row).unwrap();
        let dependencies = Dependencies::read(&row);
        assert_eq!(&*dependencies.origins, &[id(3), id(9)]);
        assert_eq!(&*dependencies.chunks, &[id(8)]);
        assert_eq!(&*dependencies.unrestored, &[id(8)]);
        assert_eq!(dependencies.bases.len(), 2);
        assert_eq!(&*basis_dependencies(&dependencies).0, &[id(9)]);
        let key = b"shared pending record".to_vec();
        let mut cache = PendingDependencies::default();
        cache.update(key.clone(), Some(&row));
        let before = cache.clone();
        assert!(Arc::ptr_eq(cache.rows.get(&key).unwrap(), before.rows.get(&key).unwrap()));
        row.provenance[1].revoked_amounts.get_mut(id(8).as_bytes()).unwrap().restored = true;
        cache.update(key.clone(), Some(&row));
        assert!(cache.rows.get(&key).unwrap().unrestored.is_empty());
        assert_eq!(&*before.rows.get(&key).unwrap().unrestored, &[id(8)]);
        assert!(before.unrestored.get(&id(8)).unwrap().contains(&key));
        assert!(!cache.unrestored.contains_key(&id(8)));
        row.return_credit = true;
        cache.update(key.clone(), Some(&row));
        assert!(cache.rows.get(&key).unwrap().origins.is_empty());
        assert!(!cache.origins.contains_key(&id(3)));
    }

    #[test]
    fn pending_dependencies_select_chunks_and_keep_writer_abort_undo_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap(); seed(&mut batch);
        assert_eq!(selected(&mut batch), (vec![1, 2], 66));
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2], 2));
        let mut graph = MergerGraph::new(); graph.apply_revocation(id(500));
        assert!(batch.pending_for_restoration(&graph).unwrap().is_empty());
        graph.apply_restoration(&id(500));
        let restored = batch.pending_for_restoration(&graph).unwrap();
        assert_eq!(restored.len(), 1); assert_eq!(restored[0].1, id(2));
        let mut changed = info(5); changed.provenance[0].origin = id(500);
        batch.put_pending(&address(5), id(5).as_bytes(), &bincode::serialize(&changed).unwrap()).unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2, 5], 4));
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2], 2));
        let key = crate::pending::pending_key_raw(&address(1), id(1).as_bytes());
        let bytes = batch.env.pending_db.get(&batch.txn, &key).unwrap().unwrap().to_vec();
        batch.mutate_raw(UndoTable::Pending, &key, None).unwrap();
        assert_eq!(selected(&mut batch), (vec![2], 1));
        batch.mutate_raw(UndoTable::Pending, &key, Some(&bytes)).unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2], 3));
        batch.clear_checkpoint_rows(UndoTable::Pending, None).unwrap();
        assert_eq!(selected(&mut batch), (vec![], 0));
        batch.commit().unwrap();
    }

    #[test]
    fn pending_dependencies_reject_unrelated_invalid_rows_but_restore_keeps_prior_rules() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap(); seed(&mut batch);
        selected(&mut batch); batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut invalid = info(40); invalid.amount = 0;
        batch.put_pending(&address(40), id(40).as_bytes(), &bincode::serialize(&invalid).unwrap()).unwrap();
        // Restoration does not reject semantic errors in unrelated ordinary rows.
        assert_eq!(batch.pending_for_restoration(&MergerGraph::new()).unwrap().len(), 1);
        assert!(batch.pending_for_revocation(&[], &MergerGraph::new()).is_err());
        assert!(batch.commit().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        invalid.return_credit = true;
        batch.put_pending(&address(40), id(40).as_bytes(), &bincode::serialize(&invalid).unwrap()).unwrap();
        assert_eq!(selected(&mut batch).0, vec![1, 2]);
        let key = crate::pending::pending_key_raw(&address(41), id(41).as_bytes());
        assert!(batch.mutate_raw(UndoTable::Pending, &key, Some(b"corrupt")).is_err());
        assert!(batch.commit().is_err());
        // Bypass the checked writer to exercise cold corruption detection after
        // an unknown adapter commit invalidates the committed-generation cache.
        let mut batch = store.tx_begin_write().unwrap();
        batch.env.pending_db.put(&mut batch.txn, &key, b"corrupt").unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.pending_for_restoration(&MergerGraph::new()).is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    fn pending_dependencies_rebuild_after_adapter_commit_and_reopen() {
        use burst_store::pending::PendingStore;
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap(); seed(&mut batch);
        assert_eq!(selected(&mut batch), (vec![1, 2], 66));
        batch.commit().unwrap();
        let mut row = info(5); row.provenance[0].origin = id(500);
        store.pending_store().put_pending(&address(5), &id(5), &row).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2, 5], 67));
        assert_eq!(selected(&mut batch), (vec![1, 2, 5], 3));
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(selected(&mut batch), (vec![1, 2, 5], 67));
    }

    fn basis_graph() -> MergerGraph {
        let mut graph = MergerGraph::new();
        graph.record_merge(burst_trst::MergeNode {
            merge_tx: id(900), total_amount: 10,
            source_origins: vec![burst_trst::MergeSource { origin: id(500), amount: 3 },
                burst_trst::MergeSource { origin: id(501), amount: 7 }],
            revoked_contribs: Default::default(),
        });
        graph
    }

    #[test]
    fn pending_single_basis_is_selected_and_unrelated_invalid_basis_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let graph = basis_graph();
        let mut batch = store.tx_begin_write().unwrap();
        let mut row = info(1); row.provenance[0].origin = id(900);
        row.provenance[0].basis = TrstBasis::Single(id(500));
        batch.put_pending(&address(1), id(1).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        // No revoked chunk and no requested merge origin: the Single basis is essential.
        assert_eq!(batch.pending_for_revocation(&[id(500)], &graph).unwrap().len(), 1);
        assert!(batch.pending_for_revocation(&[id(999)], &graph).unwrap().is_empty());
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        // Graph rewind removes the merge and must invalidate the unrelated row's proof.
        assert!(batch.pending_for_revocation(&[id(999)], &MergerGraph::new()).is_err());
        assert!(batch.commit().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        row.provenance[0].basis = TrstBasis::Single(TxHash::ZERO);
        row.provenance[0].revoked_amounts.insert(*id(501).as_bytes(), TrstRevocationChunk { amount: 10, restored: false });
        batch.put_pending(&address(1), id(1).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        // A fully split portion has no clean amount requiring basis validation.
        assert!(batch.pending_for_revocation(&[id(999)], &graph).unwrap().is_empty());
        row.provenance[0].revoked_amounts.clear();
        batch.put_pending(&address(1), id(1).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        // Refresh through restoration first; the later revocation must still check the dirty basis.
        batch.pending_for_restoration(&graph).unwrap();
        assert!(batch.pending_for_revocation(&[id(999)], &graph).is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    fn pending_remainder_basis_revalidates_removed_revision_with_merge_preserved() {
        use burst_trst::merger_graph::GraphRecord;
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let before = basis_graph();
        let mut after = before.clone(); after.apply_revocation(id(500));
        let mut revision = None;
        after.visit_records(|row| {
            if let GraphRecord::CurrentRevision { revision: hash, .. } = row { revision = Some(hash); }
            Ok::<_, String>(())
        }).unwrap();
        let mut row = info(1); row.provenance[0].origin = id(900);
        row.provenance[0].basis = TrstBasis::Remainder(revision.unwrap());
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_pending(&address(1), id(1).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        assert!(batch.pending_for_revocation(&[id(999)], &after).unwrap().is_empty());
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.pending_for_revocation(&[id(999)], &before).is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    #[ignore = "explicit capped-host pending dependency scaling and process RSS"]
    fn pending_dependency_selection_scaling() {
        use std::time::Instant;
        for population in [100u64, 1_000, 10_000, 100_000] {
            let dir = tempfile::tempdir().unwrap();
            let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            for n in 0..population {
                let mut row = info(n);
                if n == 0 {
                    row.provenance[0].revoked_amounts.insert(*id(1_000_000).as_bytes(),
                        TrstRevocationChunk { amount: 3, restored: false });
                }
                batch.put_pending(&address(n), id(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
            }
            batch.commit().unwrap();
            let rss = || std::fs::read_to_string("/proc/self/status").unwrap().lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                .collect::<Vec<_>>().join(" ");
            println!("pending_dependency_memory population={population} stage=seed {}", rss());
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (cold, decoded) = batch.select_pending_dependencies(Selection::Revocation(&[id(0)], &MergerGraph::new())).unwrap();
            let cold_ns = start.elapsed().as_nanos();
            assert_eq!(decoded, population as usize + 1);
            assert_eq!(cold.len(), 1); assert_eq!(cold[0].1, id(0));
            batch.commit().unwrap();
            println!("pending_dependency_memory population={population} stage=indexed {}", rss());
            let batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let scan = batch.all_pending().unwrap();
            for (_, _, row) in &scan { validate_pending_provenance(row).unwrap(); }
            let expected: Vec<_> = scan.into_iter().filter(|(_, _, row)|
                row.provenance.iter().any(|p| p.origin == id(0))).collect();
            let scan_ns = start.elapsed().as_nanos();
            assert_eq!(bincode::serialize(&cold).unwrap(), bincode::serialize(&expected).unwrap());
            drop(batch);
            let graph = MergerGraph::new();
            for sample in 0..3 {
                let mut batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let (warm, decoded) = batch.select_pending_dependencies(Selection::Revocation(&[id(0)], &MergerGraph::new())).unwrap();
                let warm_ns = start.elapsed().as_nanos();
                assert_eq!(decoded, 1);
                assert_eq!(bincode::serialize(&warm).unwrap(), bincode::serialize(&expected).unwrap());
                let start = Instant::now();
                let (restored, decoded) = batch.select_pending_dependencies(Selection::Restoration(&graph)).unwrap();
                let restore_ns = start.elapsed().as_nanos();
                assert_eq!(decoded, 1);
                assert_eq!(bincode::serialize(&restored).unwrap(), bincode::serialize(&expected).unwrap());
                let mut changed = info(1); changed.provenance[0].origin = id(0);
                batch.put_pending(&address(1), id(1).as_bytes(), &bincode::serialize(&changed).unwrap()).unwrap();
                let start = Instant::now();
                let (dirty, decoded) = batch.select_pending_dependencies(Selection::Revocation(&[id(0)], &MergerGraph::new())).unwrap();
                let dirty_ns = start.elapsed().as_nanos();
                assert_eq!(decoded, 3); assert_eq!(dirty.len(), 2);
                assert_eq!(dirty.iter().map(|(_, hash, _)| *hash).collect::<Vec<_>>(), vec![id(0), id(1)]);
                println!("pending_dependency_scale population={population} sample={sample} cold_ns={cold_ns} scan_ns={scan_ns} warm_ns={warm_ns} restore_ns={restore_ns} dirty_ns={dirty_ns}");
                // Abort mutation and refreshed index before the next sample.
            }
        }
    }

    fn revision_basis_case(population: u64) {
        use burst_trst::merger_graph::{GraphRecord, MergeNode, MergeSource};
        use std::time::Instant;
        let mut before = MergerGraph::new();
        for (merge, first) in [(900, 1), (901, 3)] {
            before.record_merge(MergeNode { merge_tx: id(merge), total_amount: 10,
                source_origins: vec![MergeSource { origin: id(first), amount: 5 },
                    MergeSource { origin: id(first + 1), amount: 5 }],
                revoked_contribs: Default::default() });
        }
        before.apply_revocation(id(1));
        let mut after = before.clone();
        after.apply_revocation(id(3));
        let head = |graph: &MergerGraph, merge| {
            let mut result = None;
            graph.visit_records(|record| {
                if let GraphRecord::CurrentRevision { merge: found, revision } = record {
                    if found == id(merge) { result = Some(revision); }
                }
                Ok::<(), String>(())
            }).unwrap();
            result.unwrap()
        };
        let retained = head(&before, 900);
        let removed = head(&after, 901);
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for n in 0..population {
            let mut row = info(n);
            row.provenance[0].origin = id(900);
            row.provenance[0].basis = TrstBasis::Remainder(retained);
            batch.put_pending(&address(n), id(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        let (rows, decoded) = batch.select_pending_dependencies(Selection::ValidateBases(&after)).unwrap();
        let cold_ns = start.elapsed().as_nanos();
        assert!(rows.is_empty()); assert_eq!(decoded, population as usize);
        batch.commit().unwrap();
        for sample in 0..3 {
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (rows, decoded) = batch.select_pending_dependencies(Selection::ValidateBases(&before)).unwrap();
            let warm_ns = start.elapsed().as_nanos();
            assert!(rows.is_empty()); assert_eq!(decoded, 0);
            let mut row = info(0);
            row.provenance[0].origin = id(901);
            row.provenance[0].basis = TrstBasis::Remainder(removed);
            batch.put_pending(&address(0), id(0).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
            let start = Instant::now();
            let (_, decoded) = batch.select_pending_dependencies(Selection::ValidateBases(&after)).unwrap();
            let dirty_ns = start.elapsed().as_nanos();
            assert_eq!(decoded, 1);
            let start = Instant::now();
            assert!(batch.validate_pending_bases(&before).is_err());
            let rejection_ns = start.elapsed().as_nanos();
            assert!(batch.commit().is_err());
            let memory: Vec<_> = std::fs::read_to_string("/proc/self/status").unwrap().lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                .map(str::to_owned).collect();
            println!("pending_revision_basis population={population} sample={sample} cold_ns={cold_ns} warm_ns={warm_ns} dirty_ns={dirty_ns} rejection_ns={rejection_ns} memory={memory:?}");
        }
        // Failed candidate updates must not reach durable primary rows or caches.
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.select_pending_dependencies(Selection::ValidateBases(&before)).unwrap().1, 0);
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.select_pending_dependencies(Selection::ValidateBases(&before)).unwrap().1, population as usize);
    }

    #[test]
    fn revision_basis_validation_retains_unrelated_pending_and_aborts_orphan_reference() {
        revision_basis_case(64);
    }

    #[test]
    #[ignore = "explicit capped-host pending revision undo dependency diagnostic"]
    fn pending_revision_basis_scaling() {
        for population in [100, 1_000, 10_000, 100_000] { revision_basis_case(population); }
    }

    fn assert_restoration(batch: &mut LmdbWriteTransaction<'_>, graph: &MergerGraph) -> Vec<TxHash> {
        let expected: Vec<_> = batch.all_pending().unwrap().into_iter().filter(|(_, _, row)|
            !row.return_credit && row.provenance.iter().any(|p| p.revoked_amounts.iter()
                .any(|(origin, chunk)| !chunk.restored && !graph.is_origin_revoked(&TxHash::new(*origin)))))
            .collect();
        let selected = batch.pending_for_restoration(graph).unwrap();
        assert_eq!(bincode::serialize(&selected).unwrap(), bincode::serialize(&expected).unwrap());
        selected.into_iter().map(|(_, hash, _)| hash).collect()
    }

    #[test]
    fn restoration_eligibility_tracks_graph_rewind_dirty_rows_abort_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut graph = MergerGraph::new();
        let mut batch = store.tx_begin_write().unwrap();
        for n in 0..64 {
            let mut row = info(n);
            row.provenance[0].revoked_amounts.insert(*id(1000+n).as_bytes(), TrstRevocationChunk { amount: 3, restored: false });
            graph.apply_revocation(id(1000+n));
            batch.put_pending(&address(n), id(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        }
        assert!(assert_restoration(&mut batch, &graph).is_empty());
        batch.commit().unwrap();
        let before = graph.clone();
        graph.apply_restoration(&id(1002));
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        assert!(assert_restoration(&mut batch, &before).is_empty());
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let key = crate::pending::pending_key_raw(&address(2), id(2).as_bytes());
        let bytes = batch.env.pending_db.get(&batch.txn, &key).unwrap().unwrap().to_vec();
        batch.mutate_raw(UndoTable::Pending, &key, None).unwrap();
        assert!(assert_restoration(&mut batch, &graph).is_empty());
        batch.mutate_raw(UndoTable::Pending, &key, Some(&bytes)).unwrap();
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        let mut row: PendingInfo = bincode::deserialize(&bytes).unwrap();
        row.provenance[0].revoked_amounts.get_mut(id(1002).as_bytes()).unwrap().restored = true;
        batch.put_pending(&address(2), id(2).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        let mut added = info(3);
        added.provenance[0].revoked_amounts.insert(*id(42).as_bytes(), TrstRevocationChunk { amount: 3, restored: false });
        batch.put_pending(&address(3), id(3).as_bytes(), &bincode::serialize(&added).unwrap()).unwrap();
        // Revocation may refresh primary dependencies before the next restoration.
        batch.pending_for_revocation(&[], &graph).unwrap();
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(3)]);
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(assert_restoration(&mut batch, &graph), vec![id(2)]);
        batch.clear_checkpoint_rows(UndoTable::Pending, None).unwrap();
        assert!(assert_restoration(&mut batch, &graph).is_empty());
        batch.commit().unwrap();
    }

    #[test]
    #[ignore = "explicit capped-host many-origin pending restoration scaling"]
    fn pending_restoration_origin_scaling() {
        use std::time::Instant;
        for population in [100u64, 1_000, 10_000, 100_000] {
            let dir = tempfile::tempdir().unwrap();
            let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
            let mut graph = MergerGraph::new();
            let mut batch = store.tx_begin_write().unwrap();
            for n in 0..population {
                let mut row = info(n);
                row.provenance[0].revoked_amounts.insert(*id(1_000_000+n).as_bytes(), TrstRevocationChunk { amount: 3, restored: false });
                graph.apply_revocation(id(1_000_000+n));
                batch.put_pending(&address(n), id(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
            }
            graph.apply_restoration(&id(1_000_000));
            let start = Instant::now();
            assert_eq!(batch.pending_for_restoration(&graph).unwrap().len(), 1);
            let cold_ns = start.elapsed().as_nanos();
            batch.commit().unwrap();
            for sample in 0..3 {
                let mut batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let cache = batch.auxiliary_cache.get_mut().pending_provenance.as_ref().unwrap();
                let expected: SharedSet<Vec<u8>> = cache.unrestored.iter().filter(|(origin, _)| !graph.is_origin_revoked(origin))
                    .flat_map(|(_, rows)| rows.iter().cloned()).collect();
                let scan_ns = start.elapsed().as_nanos();
                assert_eq!(expected.len(), 1);
                let start = Instant::now();
                let (rows, decoded) = batch.select_pending_dependencies(Selection::Restoration(&graph)).unwrap();
                let warm_ns = start.elapsed().as_nanos();
                assert_eq!(decoded, 1); assert_eq!(rows[0].1, id(0));
                assert_eq!(crate::pending::pending_key_raw(&rows[0].0, rows[0].1.as_bytes()), *expected.iter().next().unwrap());
                let mut changed = graph.clone(); changed.apply_restoration(&id(1_000_001));
                let start = Instant::now();
                let (rows, decoded) = batch.select_pending_dependencies(Selection::Restoration(&changed)).unwrap();
                let changed_ns = start.elapsed().as_nanos();
                assert_eq!(decoded, 2); assert_eq!(rows.iter().map(|(_, hash, _)| *hash).collect::<Vec<_>>(), vec![id(0), id(1)]);
                let rss = std::fs::read_to_string("/proc/self/status").unwrap().lines()
                    .filter(|line| line.starts_with("VmRSS:")).collect::<Vec<_>>().join(" ");
                println!("pending_restoration origins={population} sample={sample} cold_ns={cold_ns} scan_ns={scan_ns} warm_ns={warm_ns} changed_ns={changed_ns} {rss}");
            }
        }
    }
}
