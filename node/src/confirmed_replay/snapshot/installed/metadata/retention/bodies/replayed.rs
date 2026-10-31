use super::*;
use bincode::Options;
use burst_store_lmdb::block_undo::{UndoChange, UndoTable};
use burst_types::{NetworkId, WalletAddress};
use std::ops::ControlFlow;

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    if bytes.len() > 64 << 20 {
        return Err(invalid("replayed image exceeds retention budget"));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)
}
fn hash(bytes: &[u8]) -> Result<BlockHash, StoreError> {
    Ok(BlockHash::new(bytes.try_into().map_err(|_| {
        invalid("invalid replayed block reference")
    })?))
}

impl Roots {
    // Only the opaque replay preparation calls this in production. Stored local
    // BlockUndo bytes must never be fed into this path as authenticated deltas.
    pub(super) fn replayed_change(
        &mut self,
        record: &crate::confirmed_replay::snapshot::installed::ReplayedEconomicChange,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<(), StoreError> {
        self.add(record.block, Source::Undo)?;
        for change in record
            .changes
            .iter()
            .chain(record.governance.iter().flat_map(|g| g.changes()))
        {
            for bytes in change.before.iter().chain(change.after.iter()) {
                self.image_references(change, bytes, network, genesis)?;
            }
        }
        Ok(())
    }

    pub(super) fn image_references(&mut self, change: &UndoChange, bytes: &[u8],
        network: NetworkId, genesis: BlockHash) -> Result<(), StoreError> {
        self.image_references_until(change, bytes, network, genesis, &|| false)
    }

    pub(super) fn image_references_until(&mut self, change: &UndoChange, bytes: &[u8],
        network: NetworkId, genesis: BlockHash, cancelled: &dyn Fn() -> bool) -> Result<(), StoreError> {
        RootCollector { roots: self, cancelled }.image_references(change, bytes, network, genesis)
    }
}

struct RootCollector<'a> {
    roots: &'a mut Roots,
    cancelled: &'a dyn Fn() -> bool,
}
impl RootCollector<'_> {
    fn add(&mut self, hash: BlockHash, source: Source) -> Result<(), StoreError> {
        self.roots.add_until(hash, source, self.cancelled)
    }
    fn trst(&mut self, reference: TrstReference<'_>) -> Result<(), StoreError> {
        match reference {
            TrstReference::Block(hash) => self.add(hash, Source::State),
            other => self.roots.trst(other),
        }
    }
    fn governance(&mut self, reference: burst_governance::record_references::Reference<'_>) -> Result<(), StoreError> {
        match reference {
            burst_governance::record_references::Reference::Block(hash) => self.add(hash, Source::State),
            other => self.roots.governance(other),
        }
    }
    fn replay_block(&mut self, block: &burst_ledger::StateBlock) -> Result<(), StoreError> {
        self.add(block.hash, Source::Replay)?;
        for dependency in block.dependencies() { self.add(dependency, Source::Replay)?; }
        Ok(())
    }

    pub(super) fn image_references(
        &mut self,
        change: &UndoChange,
        bytes: &[u8],
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<(), StoreError> {
        let key = change.key.as_slice();
        match change.table {
            UndoTable::Blocks => {
                let block: burst_ledger::StateBlock = decode(bytes)?;
                if block.hash != hash(key)? || block.compute_hash() != block.hash {
                    return Err(invalid("replayed body differs from key"));
                }
                self.replay_block(&block)
            }
            UndoTable::Accounts => {
                let account: burst_store::account::AccountInfo = decode(bytes)?;
                if account.address.as_str().as_bytes() != key {
                    return Err(invalid("replayed account differs from key"));
                }
                if !account.head.is_zero() {
                    self.add(account.head, Source::Undo)?;
                }
                Ok(())
            }
            UndoTable::Frontiers | UndoTable::Heights => {
                let hash = hash(bytes)?;
                if !hash.is_zero() {
                    self.add(hash, Source::Undo)?;
                }
                Ok(())
            }
            UndoTable::BlockHeights => self.add(hash(key)?, Source::Undo),
            UndoTable::Pending => {
                if key.len() <= 32 {
                    return Err(invalid("invalid replayed pending key"));
                }
                let (owner, source) = key.split_at(key.len() - 32);
                let owner = WalletAddress::new(std::str::from_utf8(owner).map_err(invalid)?);
                let pending: burst_store::pending::PendingInfo = decode(bytes)?;
                match pending.visit_retention_references(&owner, hash(source)?, |r| {
                    self.trst(r)
                        .map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                }) {
                    ControlFlow::Continue(()) => Ok(()),
                    ControlFlow::Break(error) => Err(error),
                }
            }
            // These indexes name transaction/token identities, not block bodies.
            // Their complete images remain in the authenticated preparation.
            UndoTable::Transactions
            | UndoTable::AccountTransactions
            | UndoTable::TrstOrigins
            | UndoTable::TrstExpiry
            | UndoTable::TrstReverse => Ok(()),
            UndoTable::BrnWallets => self.wallet(key, burst_brn::wallet_storage::HEADER, bytes),
            UndoTable::BrnMeta => {
                use burst_brn::{rate_storage, wallet_storage};
                if key.starts_with(wallet_storage::ROW_PREFIX) {
                    let (owner, relative) = wallet_storage::split_row_key(key).map_err(invalid)?;
                    return self.wallet(owner, relative, bytes);
                }
                // These authenticated fields contain economics/parameters, not
                // body addresses. No age or value permits discarding their rows.
                if matches!(
                    key,
                    b"next_stake_id" | b"protocol_params" | b"rate_history" | b"governance_engine"
                ) || key == burst_verification::bootstrap::BootstrapPhase::KEY
                    || key == rate_storage::HEADER
                    || key.starts_with(rate_storage::ROW_PREFIX)
                {
                    return Ok(());
                }
                Err(invalid("unsupported replayed BRN metadata for retention"))
            }
            UndoTable::Meta => self.meta_image(
                std::str::from_utf8(key).map_err(invalid)?,
                bytes,
                network,
                genesis,
            ),
        }
    }

    fn wallet(&mut self, owner: &[u8], relative: &[u8], bytes: &[u8]) -> Result<(), StoreError> {
        let owner = WalletAddress::new(std::str::from_utf8(owner).map_err(invalid)?);
        burst_brn::wallet_storage::visit_retention_references(&owner, relative, bytes, |r| {
            use burst_brn::retention::Reference;
            match r {
                Reference::Block(hash) => self
                    .add(hash, Source::Undo)
                    .map_err(|e| burst_brn::BrnError::Other(e.to_string())),
                Reference::Account(_)
                | Reference::Outcome(_)
                | Reference::Stake(_)
                | Reference::Boundary(_) => Ok(()),
            }
        })
        .map_err(invalid)
    }

    fn meta_image(
        &mut self,
        key: &str,
        bytes: &[u8],
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<(), StoreError> {
        use burst_trst::engine::{holding_rows, WalletRecord};
        use burst_verification::orchestrator::records::{normalized, VerificationRecord};
        let mut trst = |r| {
            self.trst(r)
                .map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
        };
        if key.starts_with(holding_rows::PREFIX) {
            return match holding_rows::Row::from_storage_entry(key, bytes)
                .map_err(invalid)?
                .visit_retention_references(&mut trst)
            {
                ControlFlow::Continue(()) => Ok(()),
                ControlFlow::Break(error) => Err(error),
            };
        }
        if key.starts_with(WalletRecord::STORAGE_PREFIX) {
            return match WalletRecord::from_storage_entry(key, bytes)
                .map_err(invalid)?
                .visit_retention_references(&mut trst)
            {
                ControlFlow::Continue(()) => Ok(()),
                ControlFlow::Break(error) => Err(error),
            };
        }
        if key == burst_trst::TrstEngine::meta_key() {
            return burst_trst::TrstEngine::visit_wallet_snapshot_records(bytes, |row| {
                match row.visit_retention_references(|r| {
                    self.trst(r)
                        .map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                }) {
                    ControlFlow::Continue(()) => Ok(()),
                    ControlFlow::Break(error) => Err(error.to_string()),
                }
            })
            .map_err(invalid);
        }
        if key == "verification_orchestrator" {
            let snapshot =
                burst_verification::OrchestratorSnapshot::decode(bytes).map_err(invalid)?;
            return burst_verification::VerificationOrchestrator::restore(snapshot)
                .visit_records(|row| row.visit_retention_references(|r| self.verification(r)))
                .map_err(invalid);
        }
        if key.starts_with("verification_state/") {
            return if key.starts_with(normalized::PREFIX) {
                normalized::visit_row_retention_references(key, bytes, |r| self.verification(r))
            } else {
                VerificationRecord::from_storage_entry(key, bytes)
                    .and_then(|row| row.visit_retention_references(|r| self.verification(r)))
            }
            .map_err(invalid);
        }
        if crate::verification_commit::retention::PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            return crate::verification_commit::retention::visit_row(key, bytes, |r| {
                use crate::verification_commit::retention::Reference;
                match r {
                    Reference::Block(hash) => self.add(hash, Source::Undo),
                    Reference::Account(_)
                    | Reference::Checkpoint(_)
                    | Reference::CommitteeContext(_)
                    | Reference::Parameters(_)
                    | Reference::Outcome(_)
                    | Reference::RewardOrigin(_)
                    | Reference::Boundary(_) => Ok(()),
                }
            });
        }
        if key.starts_with("governance_endorsement/") {
            return super::super::governance::visit_endorsement(key, bytes, |r| self.governance(r));
        }
        if key.starts_with("key_delegation/") {
            return burst_governance::signing_delegation::scoped::retention::visit_references(
                network,
                genesis,
                key,
                bytes,
                |r| self.governance(r).map_err(|e| e.to_string()),
            )
            .map_err(invalid);
        }
        if key.starts_with("governance_state/") {
            return burst_governance::engine::GovernanceRecord::from_storage_entry(key, bytes)
                .and_then(|row| {
                    row.visit_retention_references(|r| {
                        self.governance(r).map_err(|e| e.to_string())
                    })
                })
                .map_err(invalid);
        }
        if key.starts_with(burst_trst::merger_graph::GraphRecord::STORAGE_PREFIX) {
            return match burst_trst::merger_graph::GraphRecord::from_storage_entry(key, bytes)
                .map_err(invalid)?
                .visit_retention_references(|r| {
                    self.trst(r)
                        .map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                }) {
                ControlFlow::Continue(()) => Ok(()),
                ControlFlow::Break(error) => Err(error),
            };
        }
        // Graph, legacy delegation, constitutional state and the verified-count
        // cache contain logical identities/configuration; all images are retained.
        if matches!(key, "merger_graph" | "consti_engine" | "verified_count")
            || key == burst_governance::DelegationEngine::meta_key()
        {
            return Ok(());
        }
        Err(invalid(format!(
            "unsupported replayed metadata for retention: {key}"
        )))
    }

    fn verification(
        &mut self,
        r: burst_verification::orchestrator::records::retention::Reference<'_>,
    ) -> Result<(), String> {
        use burst_verification::orchestrator::records::retention::Reference;
        match r {
            Reference::Block(hash) => self.add(hash, Source::Undo).map_err(|e| e.to_string()),
            Reference::Account(_)
            | Reference::Checkpoint(_)
            | Reference::CommitteeContext(_)
            | Reference::Outcome(_)
            | Reference::Parameters(_)
            | Reference::Boundary(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replayed_beforeimages_keep_removed_claims_and_causes_without_casting_origins() {
        let dir = tempfile::tempdir().unwrap();
        let owner = WalletAddress::new("brst_retained_beforeimage");
        let source = BlockHash::new([4; 32]);
        let mut pending_key = owner.as_str().as_bytes().to_vec();
        pending_key.extend_from_slice(source.as_bytes());
        let pending = burst_store::pending::PendingInfo {
            source: owner.clone(),
            amount: 1,
            timestamp: burst_types::Timestamp::new(1),
            provenance: Vec::new(),
            return_credit: true,
        };
        let mut account = burst_store::account::AccountInfo::unopened(owner.clone());
        account.head = BlockHash::new([5; 32]);
        let cause = BlockHash::new([7; 32]);
        let origin = burst_types::TxHash::new([9; 32]);
        let (key, bytes) = burst_trst::engine::WalletRecord::Cause { origin, cause }
            .storage_entry()
            .unwrap();
        let mut record = crate::confirmed_replay::snapshot::installed::ReplayedEconomicChange {
            block: BlockHash::new([1; 32]),
            account: owner,
            height: 1,
            governance: None,
            changes: vec![
                UndoChange {
                    table: UndoTable::Pending,
                    key: pending_key,
                    before: Some(bincode::serialize(&pending).unwrap()),
                    after: None,
                },
                UndoChange {
                    table: UndoTable::Accounts,
                    key: account.address.as_str().as_bytes().to_vec(),
                    before: Some(bincode::serialize(&account).unwrap()),
                    after: None,
                },
                UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: Some(bytes),
                    after: None,
                },
            ],
        };
        let mut roots = Roots::new(dir.path()).unwrap();
        roots
            .replayed_change(&record, NetworkId::Dev, BlockHash::new([2; 32]))
            .unwrap();
        for hash in [record.block, source, account.head, cause] {
            assert!(roots.rows.get(hash.as_bytes()).unwrap().is_some());
        }
        assert_eq!(roots.count, 4);
        assert!(roots.rows.get(origin.as_bytes()).unwrap().is_none());
        record.changes[0].before.as_mut().unwrap().push(0);
        assert!(Roots::new(dir.path())
            .unwrap()
            .replayed_change(&record, NetworkId::Dev, BlockHash::new([2; 32]))
            .is_err());
        record.changes = vec![UndoChange {
            table: UndoTable::Meta,
            key: b"future_schema/v9/item".to_vec(),
            before: Some(vec![1]),
            after: None,
        }];
        assert!(Roots::new(dir.path())
            .unwrap()
            .replayed_change(&record, NetworkId::Dev, BlockHash::new([2; 32]))
            .is_err());
    }
}
