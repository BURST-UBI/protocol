use super::*;
use burst_store_lmdb::block_undo::{UndoChange, UndoTable};
use burst_types::NetworkId;

/// Additional local protection requirements only. This type cannot authenticate
/// economics, certify closure, restore a journal, or grant deletion eligibility.
/// Any error poisons the whole collection, including already extracted roots.
pub(super) struct ObservedBodyRoots {
    roots: Roots,
    network: NetworkId,
    genesis: BlockHash,
    epoch: BlockHash,
    failed: bool,
}
impl ObservedBodyRoots {
    pub(super) fn new(directory: &std::path::Path, network: NetworkId,
        genesis: BlockHash, epoch: BlockHash, max_roots: u64) -> Result<Self, StoreError> {
        if max_roots == 0 || max_roots > BodyClosureLimits::default().bodies {
            return Err(invalid("invalid observed body root budget"));
        }
        let mut roots = Roots::new(directory)?;
        roots.max_count = Some(max_roots);
        Ok(Self { roots, network, genesis, epoch, failed: false })
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, row: &UndoChange) -> Result<(), StoreError> {
        self.observe_until(row, &|| false)
    }
    pub(super) fn observe_until(&mut self, row: &UndoChange, cancelled: &dyn Fn() -> bool) -> Result<(), StoreError> {
        if self.failed { return Err(invalid("observed body roots already failed")); }
        self.failed = true;
        if row.before.is_none() && row.after.is_none() {
            return Err(invalid("observed mutation has no images"));
        }
        for bytes in row.before.iter().chain(row.after.iter()) { self.image(row, bytes, cancelled)?; }
        self.failed = false;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn visit(&mut self, visit: impl FnMut(BlockHash) -> Result<(), StoreError>) -> Result<(), StoreError> {
        if self.failed { return Err(invalid("observed body roots already failed")); }
        self.failed = true;
        self.roots.visit(visit)?;
        self.failed = false;
        Ok(())
    }
    pub(super) fn into_roots(self) -> Result<Roots, StoreError> {
        if self.failed { return Err(invalid("observed body roots already failed")); }
        Ok(self.roots)
    }
    fn image(&mut self, row: &UndoChange, bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<(), StoreError> {
        if cancelled() { return Err(invalid("body observation cancelled")); }
        if row.key.len().checked_add(bytes.len()).is_none_or(|n| n > 64 << 20) {
            return Err(invalid("observed image exceeds byte budget"));
        }
        if row.table == UndoTable::BrnMeta && row.key == b"governance_engine" {
            return Err(invalid("observed aggregate governance requires fresh preparation"));
        }
        if row.table == UndoTable::Meta {
            let key = row.key.as_slice();
            if key.starts_with(b"block_undo/") || key.starts_with(b"governance_revision/") {
                return burst_store_lmdb::retention::visit_rollback_row(key, bytes, |record| {
                    if let burst_store_lmdb::retention::RollbackRecord::Journal(journal) = record {
                        self.roots.add_until(journal.block, Source::Work, cancelled)?;
                        // Treat even stored before-images only as extra protection.
                        // The checked decoder rejects nested/irreversible journals.
                        for change in &journal.changes {
                            for image in change.before.iter().chain(change.after.iter()) {
                                self.image(change, image, cancelled)?;
                            }
                        }
                    }
                    Ok(())
                });
            }
            if key.starts_with(b"pending_confirmation/") {
                let hash = burst_store_lmdb::retention::pending_confirmation_reference(key, bytes)?;
                return self.roots.add_until(hash, Source::Work, cancelled);
            }
            if key.starts_with(b"consensus_final/") || key.starts_with(b"consensus_time/")
                || key.starts_with(b"consensus_governance/") {
                use super::super::signers::{decode, SignerRecord};
                use crate::confirmed_replay::snapshot::reconciliation::promises::PaymentPromise;
                let key = std::str::from_utf8(key).map_err(invalid)?;
                match decode(key, bytes, self.network, self.genesis)? {
                    SignerRecord::Payment(PaymentPromise::Final { epoch, block, .. }) if epoch == self.epoch =>
                        return self.roots.add_until(block, Source::Signer, cancelled),
                    SignerRecord::Time { epoch, .. } | SignerRecord::Governance { epoch, .. }
                        if epoch == self.epoch => return Ok(()),
                    _ => return Err(invalid("observed signer context changed")),
                }
            }
            // Aggregate changes require fresh preparation; this observation path
            // cannot establish their dependencies.
            if matches!(key, b"merger_graph" | b"consti_engine" | b"delegation_engine_state"
                | b"trst_engine_wallets" | b"verification_orchestrator") {
                return Err(invalid("observed aggregate requires fresh preparation"));
            }
        }
        self.roots.image_references_until(row, bytes, self.network, self.genesis, cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(directory: &std::path::Path, limit: u64) -> ObservedBodyRoots {
        ObservedBodyRoots::new(directory, NetworkId::Dev, BlockHash::new([1;32]), BlockHash::new([2;32]), limit).unwrap()
    }
    fn row(key: impl Into<Vec<u8>>, bytes: Vec<u8>) -> UndoChange {
        UndoChange { table: UndoTable::Meta, key: key.into(), before: None, after: Some(bytes) }
    }
    fn roots(value: &mut ObservedBodyRoots) -> Vec<BlockHash> {
        let mut result = Vec::new(); value.visit(|hash| { result.push(hash); Ok(()) }).unwrap(); result
    }
    #[test]
    fn cancellation_inside_compaction_poisons_observed_roots() {
        let directory = tempfile::tempdir().unwrap();
        let mut value = context(directory.path(), 1024);
        for n in 0..512u64 {
            let hash = BlockHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()));
            value.roots.add_with_compaction(hash, Source::Work, false).unwrap();
        }
        let body = BlockHash::new([7;32]);
        let mut key = b"pending_confirmation/v1/".to_vec();
        key.extend_from_slice(body.as_bytes());
        let calls = std::cell::Cell::new(0);
        assert!(value.observe_until(&row(key, vec![1]), &|| {
            calls.set(calls.get() + 1);
            calls.get() == 4
        }).is_err());
        assert_eq!(calls.get(), 4);
        assert!(value.visit(|_| Ok(())).is_err());
        assert!(value.into_roots().is_err());
    }

    #[test]
    fn observed_promises_and_handoffs_protect_bodies_not_context_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = context(dir.path(), 8);
        let signer = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0;32]).public).to_string();
        let epoch = BlockHash::new([2;32]); let body = BlockHash::new([3;32]); let predecessor = BlockHash::new([4;32]);
        value.observe(&row(format!("consensus_final/v2/{epoch}/{signer}/{signer}/{predecessor}"), body.as_bytes().to_vec())).unwrap();
        value.observe(&row(format!("consensus_time/v1/{epoch}/{signer}"), 100u64.to_be_bytes().to_vec())).unwrap();
        value.observe(&row(format!("consensus_governance/v1/{epoch}/{signer}"), 100u64.to_be_bytes().to_vec())).unwrap();
        let mut key = b"pending_confirmation/v1/".to_vec(); key.extend_from_slice(body.as_bytes());
        value.observe(&row(key, vec![1])).unwrap();
        assert_eq!(roots(&mut value), vec![body]);
        let mut wrong = context(dir.path(), 8);
        assert!(wrong.observe(&row(format!("consensus_time/v1/{predecessor}/{signer}"), 100u64.to_be_bytes().to_vec())).is_err());
        assert!(wrong.visit(|_| Ok(())).is_err());
    }
    #[test]
    fn observed_journal_images_only_add_protection_and_bound_unique_roots() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = BlockHash::new([3;32]); let before = BlockHash::new([4;32]); let after = BlockHash::new([5;32]);
        let change = UndoChange { table: UndoTable::Frontiers, key: b"owner".to_vec(),
            before: Some(before.as_bytes().to_vec()), after: Some(after.as_bytes().to_vec()) };
        let journal = burst_store_lmdb::block_undo::BlockUndo { block: carrier,
            account: burst_types::WalletAddress::new("brst_observed"), height: 1, changes: vec![change.clone()] };
        let mut key = b"block_undo/v1/".to_vec(); key.extend_from_slice(carrier.as_bytes());
        let journal_row = row(key, bincode::serialize(&journal).unwrap());
        let mut value = context(dir.path(), 3);
        value.observe(&journal_row).unwrap(); value.observe(&change).unwrap();
        assert_eq!(roots(&mut value), vec![carrier, before, after]);
        let mut exhausted = context(dir.path(), 2);
        assert!(exhausted.observe(&journal_row).is_err());
        assert!(exhausted.visit(|_| Ok(())).is_err());
        let mut corrupt = journal_row.clone();
        corrupt.after.as_mut().unwrap().push(0);
        let mut value = context(dir.path(), 8);
        assert!(value.observe(&corrupt).is_err());
        assert!(value.visit(|_| Ok(())).is_err());
    }
    #[test]
    fn committed_observations_collect_protection_without_claiming_body_availability() {
        use burst_store::{meta::MetaStore, block::BlockStore};
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let epoch = BlockHash::new([2;32]);
        let carrier = BlockHash::new([3;32]);
        let frontier = BlockHash::new([4;32]);
        let promised = BlockHash::new([5;32]);
        let account = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0;32]).public);
        store.meta_store().put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let mut watch = store.tx_begin_write().unwrap().watch_body_changes(32, 1 << 20).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        writer.begin_block_undo(carrier, account.clone(), 1).unwrap();
        writer.put_frontier(&account, &frontier).unwrap();
        writer.commit().unwrap();
        store.meta_store().reserve_consensus_vote(&epoch, &account, &account,
            &BlockHash::ZERO, &promised, true, 0).unwrap();
        let mut value = context(dir.path(), 3);
        let cut = watch.visit(&store, |row| value.observe(row)).unwrap();
        assert_eq!(roots(&mut value), vec![carrier, frontier, promised]);
        assert!(store.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
        // This fixture has references only. Extraction is not closure or proof
        // that any referenced body is available, authenticated or deletable.
        for hash in roots(&mut value) { assert!(!store.block_store().exists(&hash).unwrap()); }
    }

    #[test]
    fn unsupported_layouts_unknown_rows_and_bad_handoffs_poison_observations() {
        let dir = tempfile::tempdir().unwrap();
        for (key, bytes) in [(b"unknown".to_vec(), vec![1]), (b"payment_epoch/v1".to_vec(), vec![2;32]),
            (b"merger_graph".to_vec(), vec![1]), (b"pending_confirmation/v1/bad".to_vec(), vec![1])] {
            let mut value = context(dir.path(), 8);
            assert!(value.observe(&row(key, bytes)).is_err());
            assert!(value.visit(|_| Ok(())).is_err());
        }
    }
}
