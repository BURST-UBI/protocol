//! Rollback planning and a transactional branch-replacement primitive.
//!
//! Preflight is read-only. Replacement replans in its own writer transaction,
//! then invokes the caller's validation/application against the rewound state.
//! The runtime payment-fork path uses this primitive. The caller must serialize
//! every affected engine and publish its rebuilt state after commit.
//! Explicit block dependencies and journaled outcome/reward prerequisites are
//! covered. Disjoint TRST snapshot entries and independent consumers of shared
//! origins survive replacement. Changes to the same holding or merge still
//! conflict. Verification snapshots preserve independent targets and verifier
//! windows. Governance snapshots preserve independent proposals, delegation
//! owners and amendments. Overlapping progress is rejected. Lifecycle branch switching remains
//! gated separately.

use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_store_lmdb::{
    block_undo::{UndoChange, UndoTable},
    LmdbStore, LmdbWriteTransaction,
};
use burst_types::BlockHash;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BLOCKS: usize = 131_072;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub struct RollbackPlan {
    blocks: Vec<BlockHash>,
    changes: Vec<UndoChange>,
}

impl RollbackPlan {
    /// Descendants precede their prerequisites, including receive/source edges.
    pub fn blocks(&self) -> &[BlockHash] {
        &self.blocks
    }
    /// Before/after images in rollback order. They are not an executable token.
    pub fn changes(&self) -> &[UndoChange] {
        &self.changes
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

#[derive(Default)]
struct SnapshotUndo {
    bytes: usize,
}

impl SnapshotUndo {
    fn reconcile(
        &mut self,
        key: &[u8],
        before: &[u8],
        after: &[u8],
        current: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        self.bytes = self
            .bytes
            .checked_add(before.len())
            .and_then(|n| n.checked_add(after.len()))
            .and_then(|n| n.checked_add(current.len()))
            .ok_or_else(|| invalid("rollback reconciliation size overflow"))?;
        if self.bytes > MAX_BYTES * 4 {
            return Err(invalid(
                "rollback requires incremental snapshot reconciliation",
            ));
        }
        match key {
            b"trst_engine_wallets" => {
                burst_trst::TrstEngine::rewind_wallet_snapshot(before, after, current)
            }
            b"merger_graph" => burst_trst::MergerGraph::rewind_snapshot(before, after, current),
            b"verification_orchestrator" => {
                burst_verification::OrchestratorSnapshot::rewind(before, after, current)
            }
            b"governance_engine" => {
                burst_governance::GovernanceEngine::rewind_snapshot(before, after, current)
            }
            b"delegation_engine_state" => {
                burst_governance::DelegationSnapshot::rewind(before, after, current)
            }
            b"consti_engine" => burst_consti::ConstiEngine::rewind_snapshot(before, after, current),
            _ => return Err(invalid("unsupported rollback snapshot")),
        }
        .map_err(StoreError::Corruption)
    }
}

fn load(batch: &LmdbWriteTransaction<'_>, hash: &BlockHash) -> Result<StateBlock, StoreError> {
    let bytes = batch.get_block(hash)?;
    if bytes.len() > 128 * 1024 {
        return Err(invalid("oversized rollback block"));
    }
    let block: StateBlock =
        bincode::deserialize(&bytes).map_err(|e| StoreError::Serialization(e.to_string()))?;
    if block.hash != *hash || block.compute_hash() != *hash {
        return Err(invalid("rollback block hash mismatch"));
    }
    Ok(block)
}

fn dependencies(
    batch: &LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<BTreeSet<BlockHash>, StoreError> {
    let mut sources: BTreeSet<_> = block.dependencies().collect();
    sources.extend(crate::verification_commit::dependencies::read(
        batch, block,
    )?);
    Ok(sources)
}

/// Inspect without changing disk state or in-memory engines. Missing historical
/// undo records are an error; reconstructing economics from balances is unsafe.
pub fn preflight(store: &LmdbStore, target: BlockHash) -> Result<RollbackPlan, StoreError> {
    let batch = store.tx_begin_write()?;
    plan(&batch, target)
}

/// Returned only after the replacement and its new undo journal are durable.
/// `state` belongs to the caller; it must be rebuilt from the rewound writer
/// snapshot, not cloned from live engines that still describe the old branch.
pub struct BranchReplacement<T> {
    pub removed: Vec<StateBlock>,
    pub state: T,
}

/// Atomically replace a branch, or leave it unchanged. This is a storage
/// primitive, not election authorization or a complete block validator.
///
/// The callback must validate signatures, work, parameters and economics using
/// the rewound transaction, stage all primary/lifecycle effects (including
/// engine snapshots), and return unpublished state. It must not mutate live
/// engines or perform side effects. Errors, including after several rewinds,
/// abort the entire transaction. The callback cannot commit the borrowed batch.
pub fn replace_with<T>(
    store: &LmdbStore,
    target: BlockHash,
    winner: &StateBlock,
    apply: impl FnOnce(&mut LmdbWriteTransaction<'_>, &StateBlock) -> Result<T, StoreError>,
) -> Result<BranchReplacement<T>, StoreError> {
    let mut batch = store.tx_begin_write()?;
    let (replacement, height) = stage_with(&mut batch, target, winner, apply)?;
    batch.commit_block(&winner.hash, &winner.account, height)?;
    Ok(replacement)
}

pub(crate) fn stage_with<T>(
    batch: &mut LmdbWriteTransaction<'_>,
    target: BlockHash,
    winner: &StateBlock,
    apply: impl FnOnce(&mut LmdbWriteTransaction<'_>, &StateBlock) -> Result<T, StoreError>,
) -> Result<(BranchReplacement<T>, u64), StoreError> {
    let losing = load(batch, &target)?;
    if !crate::ledger_updater::blocks_compete(&losing, winner)
        || winner.hash.is_zero()
        || winner.compute_hash() != winner.hash
        || bincode::serialized_size(winner).map_err(|e| StoreError::Serialization(e.to_string()))?
            > 128 * 1024
    {
        return Err(invalid("replacement is not a competing block"));
    }
    let plan = plan(batch, target)?;
    let removed = plan
        .blocks
        .iter()
        .map(|hash| load(batch, hash))
        .collect::<Result<Vec<_>, _>>()?;
    let mut snapshots = SnapshotUndo::default();
    for hash in &plan.blocks {
        batch.rewind_block_with_snapshots(hash, |key, before, after, current| {
            snapshots.reconcile(key, before, after, current)
        })?;
    }
    // A candidate cannot depend on a block removed with the losing branch.
    for dependency in dependencies(batch, winner)? {
        load(batch, &dependency)?;
    }
    let previous = match batch.get_account(&winner.account) {
        Ok(account) => Some(account),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(error),
    };
    if previous
        .as_ref()
        .filter(|a| a.block_count != 0)
        .map_or(BlockHash::ZERO, |a| a.head)
        != winner.previous
    {
        return Err(invalid("replacement does not extend the rewound account"));
    }
    let height = previous
        .as_ref()
        .map_or(0, |a| a.block_count)
        .checked_add(1)
        .ok_or_else(|| invalid("replacement account height overflow"))?;
    let state = apply(batch, winner)?;
    let account = batch.get_account(&winner.account)?;
    let encoded =
        bincode::serialize(winner).map_err(|e| StoreError::Serialization(e.to_string()))?;
    if batch.get_block(&winner.hash)? != encoded
        || batch.block_height(&winner.account, &winner.hash)? != height
        || account.head != winner.hash
        || account.block_count != height
        || batch.undo_value(UndoTable::Frontiers, winner.account.as_str().as_bytes())?
            != Some(winner.hash.as_bytes().to_vec())
    {
        return Err(invalid(
            "replacement application did not install the candidate",
        ));
    }
    Ok((BranchReplacement { removed, state }, height))
}

fn plan(batch: &LmdbWriteTransaction<'_>, target: BlockHash) -> Result<RollbackPlan, StoreError> {
    let target_block = load(batch, &target)?;
    let account = batch.get_account(&target_block.account)?;
    let height = batch.block_height(&target_block.account, &target)?;
    if height == 0 || height <= account.confirmation_height {
        return Err(invalid("cannot roll back a cemented block"));
    }
    let mut blocks = BTreeMap::new();
    let mut prerequisites = BTreeMap::new();
    let mut dependents: BTreeMap<BlockHash, BTreeSet<BlockHash>> = BTreeMap::new();
    let mut total_bytes = 0usize;
    for account in batch.account_entries()? {
        let account = account?;
        if account.block_count == account.confirmation_height {
            continue;
        }
        let mut current = account.head;
        for height in (account.confirmation_height + 1..=account.block_count).rev() {
            if blocks.len() == MAX_BLOCKS {
                return Err(invalid("rollback requires incremental traversal"));
            }
            let block = load(batch, &current)?;
            if block.account != account.address
                || batch.block_height(&account.address, &current)? != height
            {
                return Err(invalid("rollback account chain mismatch"));
            }
            total_bytes = total_bytes
                .checked_add(
                    bincode::serialized_size(&block)
                        .map_err(|e| StoreError::Serialization(e.to_string()))?
                        as usize,
                )
                .ok_or_else(|| invalid("rollback size overflow"))?;
            if total_bytes > MAX_BYTES {
                return Err(invalid("rollback exceeds traversal byte limit"));
            }
            let sources = dependencies(batch, &block)?;
            total_bytes = total_bytes
                .checked_add(
                    sources
                        .len()
                        .checked_mul(32)
                        .ok_or_else(|| invalid("rollback dependency size overflow"))?,
                )
                .ok_or_else(|| invalid("rollback dependency size overflow"))?;
            if total_bytes > MAX_BYTES {
                return Err(invalid("rollback dependencies exceed traversal byte limit"));
            }
            for dependency in &sources {
                dependents.entry(*dependency).or_default().insert(current);
            }
            prerequisites.insert(current, sources);
            current = block.previous;
            if blocks.insert(block.hash, block).is_some() {
                return Err(invalid("duplicate rollback account block"));
            }
        }
        if account.block_count > account.confirmation_height {
            if account.confirmation_height == 0 {
                if !current.is_zero() {
                    return Err(invalid("rollback chain has a non-open first block"));
                }
            } else {
                let boundary = load(batch, &current)?;
                if boundary.account != account.address
                    || batch.block_height(&account.address, &current)?
                        != account.confirmation_height
                {
                    return Err(invalid(
                        "rollback chain does not reach its confirmed boundary",
                    ));
                }
            }
        }
    }
    if !blocks.contains_key(&target) {
        return Err(invalid("rollback target is not on an unconfirmed chain"));
    }
    let mut stack = vec![(target, false)];
    let mut visited = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut order = Vec::new();
    while let Some((hash, complete)) = stack.pop() {
        if visited.contains(&hash) {
            continue;
        }
        if complete {
            visiting.remove(&hash);
            visited.insert(hash);
            order.push(hash);
        } else {
            if !visiting.insert(hash) {
                return Err(invalid("cyclic rollback dependencies"));
            }
            stack.push((hash, true));
            if let Some(children) = dependents.get(&hash) {
                for child in children.iter().rev() {
                    stack.push((*child, false));
                }
            }
        }
    }
    let mut records = BTreeMap::new();
    let mut undo_bytes = 0usize;
    for hash in &order {
        if batch.confirmation_pending(hash)? {
            return Err(invalid("cannot replace a durable confirmation decision"));
        }
        let block = &blocks[hash];
        let record = batch
            .block_undo(hash)?
            .ok_or_else(|| invalid("rollback requires a missing historical undo record"))?;
        if record.account != block.account
            || record.height != batch.block_height(&block.account, hash)?
        {
            return Err(invalid("rollback undo identity mismatch"));
        }
        for change in &record.changes {
            undo_bytes = undo_bytes
                .checked_add(
                    change.key.len()
                        + change.before.as_ref().map_or(0, Vec::len)
                        + change.after.as_ref().map_or(0, Vec::len)
                        + 64,
                )
                .ok_or_else(|| invalid("rollback undo size overflow"))?;
            if undo_bytes > MAX_BYTES {
                return Err(invalid("rollback undo exceeds byte limit"));
            }
        }
        records.insert(*hash, record);
    }
    // DAG-independent branches can still have updated the same local snapshot.
    // Among dependency-ready blocks, undo a matching after-image first.
    let mut counts = BTreeMap::new();
    let mut ready = BTreeSet::new();
    for hash in &order {
        let count = dependents.get(hash).map_or(0, |children| {
            children.iter().filter(|c| records.contains_key(c)).count()
        });
        counts.insert(*hash, count);
        if count == 0 {
            ready.insert(*hash);
        }
    }
    let mut values: BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
    let mut changes = Vec::new();
    let mut result = Vec::new();
    let mut attempts = 0usize;
    let mut snapshots = SnapshotUndo::default();
    let mut restored_bytes = 0usize;
    while !records.is_empty() {
        let mut selected = None;
        for hash in &ready {
            attempts += 1;
            if attempts > MAX_BLOCKS * 4 {
                return Err(invalid("rollback requires incremental conflict scheduling"));
            }
            let mut candidate = Vec::new();
            let mut compatible = true;
            for change in &records[hash].changes {
                let mut change = change.clone();
                let key = (change.table, change.key.clone());
                let current = match values.get(&key) {
                    Some(value) => value.clone(),
                    None => batch.undo_value(change.table, &change.key)?,
                };
                if let Err(error) = change.match_current_with_snapshots(
                    current,
                    &mut |key, before, after, current| {
                        snapshots.reconcile(key, before, after, current)
                    },
                ) {
                    if snapshots.bytes > MAX_BYTES * 4 {
                        return Err(error);
                    }
                    compatible = false;
                    break;
                }
                candidate.push(change);
            }
            if compatible {
                selected = Some((*hash, candidate));
                break;
            }
        }
        let (hash, candidate) = selected.ok_or_else(|| {
            invalid("rollback conflicts with state outside the dependency closure")
        })?;
        for change in candidate {
            restored_bytes = restored_bytes
                .checked_add(change.key.len())
                .and_then(|n| n.checked_add(change.before.as_ref().map_or(0, Vec::len)))
                .and_then(|n| n.checked_add(change.after.as_ref().map_or(0, Vec::len)))
                .and_then(|n| n.checked_add(64))
                .ok_or_else(|| invalid("rollback restored snapshot size overflow"))?;
            if restored_bytes > MAX_BYTES {
                return Err(invalid("rollback restored snapshots exceed byte limit"));
            }
            values.insert((change.table, change.key.clone()), change.before.clone());
            changes.push(change);
        }
        records.remove(&hash);
        ready.remove(&hash);
        result.push(hash);
        for dependency in &prerequisites[&hash] {
            if records.contains_key(dependency) {
                let count = counts
                    .get_mut(dependency)
                    .ok_or_else(|| invalid("missing rollback dependency count"))?;
                *count = count
                    .checked_sub(1)
                    .ok_or_else(|| invalid("invalid rollback dependency count"))?;
                if *count == 0 {
                    ready.insert(*dependency);
                }
            }
        }
    }
    Ok(RollbackPlan {
        blocks: result,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, CURRENT_BLOCK_VERSION};
    use burst_store::{
        account::{AccountInfo, AccountStore},
        block::BlockStore,
    };
    use burst_types::{Signature, Timestamp, TxHash};

    #[test]
    fn snapshot_reconciliation_has_a_shared_work_budget() {
        let mut snapshots = SnapshotUndo {
            bytes: MAX_BYTES * 4,
        };
        assert!(snapshots
            .reconcile(b"trst_engine_wallets", &[0], &[], &[])
            .unwrap_err()
            .to_string()
            .contains("incremental snapshot reconciliation"));
        let mut snapshots = SnapshotUndo { bytes: usize::MAX };
        assert!(snapshots
            .reconcile(b"merger_graph", &[0], &[], &[])
            .unwrap_err()
            .to_string()
            .contains("size overflow"));
    }

    fn block(person: u8, kind: BlockType, previous: BlockHash, link: BlockHash) -> StateBlock {
        let keys = burst_crypto::keypair_from_seed(&[person; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: kind,
            representative: account.clone(),
            account,
            previous,
            link,
            brn_balance: 0,
            trst_balance: 0,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(100),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
        block
    }

    fn save(store: &LmdbStore, block: &StateBlock, sequence: u8, record: bool) {
        let first = store.block_store().block_count().unwrap() == 0;
        let mut batch = store.tx_begin_write().unwrap();
        let previous = batch.get_account(&block.account).ok();
        let height = previous.as_ref().map_or(1, |a| a.block_count + 1);
        if record {
            batch
                .begin_block_undo(block.hash, block.account.clone(), height)
                .unwrap();
        }
        let mut info = previous.unwrap_or_else(|| {
            crate::genesis_state::account_to_write(None, block.account.clone(), block.hash).unwrap()
        });
        info.head = block.hash;
        info.block_count = height;
        batch.put_account_info(&info, false).unwrap();
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                height,
            )
            .unwrap();
        batch.put_frontier(&block.account, &block.hash).unwrap();
        batch
            .put_meta("test_economic_snapshot", &[sequence])
            .unwrap();
        batch.commit().unwrap();
        if first {
            let mut batch = store.tx_begin_write().unwrap();
            crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &info).unwrap();
            batch.commit().unwrap();
        }
    }

    fn history(store: &LmdbStore, omit: Option<usize>) -> Vec<StateBlock> {
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, BlockHash::new([1; 32])).unwrap();
        batch.commit().unwrap();
        let a = block(1, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let send = block(1, BlockType::Send, a.hash, BlockHash::new([10; 32]));
        let receive = block(2, BlockType::Open, BlockHash::ZERO, send.hash);
        let next_a = block(
            1,
            BlockType::ChangeRepresentative,
            send.hash,
            BlockHash::ZERO,
        );
        let forward = block(2, BlockType::Send, receive.hash, BlockHash::new([11; 32]));
        let next_receive = block(3, BlockType::Open, BlockHash::ZERO, forward.hash);
        let blocks = vec![a, send, receive, next_a, forward, next_receive];
        for (i, block) in blocks.iter().enumerate() {
            save(store, block, i as u8, Some(i) != omit);
        }
        blocks
    }

    #[test]
    fn plans_cross_account_descendants_in_reverse_effect_order_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        crate::causal_cementation::cement(&store, blocks[0].hash).unwrap();
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let planned = preflight(&store, blocks[1].hash).unwrap();
        let expected: Vec<_> = blocks[1..].iter().rev().map(|b| b.hash).collect();
        assert_eq!(planned.blocks(), expected);
        let account_changes: Vec<_> = planned
            .changes()
            .iter()
            .filter(|c| {
                c.table == UndoTable::Accounts && c.key == blocks[0].account.as_str().as_bytes()
            })
            .collect();
        assert_eq!(account_changes.len(), 2);
        for change in account_changes {
            let info: AccountInfo = bincode::deserialize(change.before.as_ref().unwrap()).unwrap();
            assert_eq!(info.confirmation_height, 1);
        }
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_meta("test_economic_snapshot").unwrap(),
            Some(vec![5])
        );
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
        }
        assert_eq!(
            batch.get_account(&blocks[0].account).unwrap().head,
            blocks[3].hash
        );
        assert!(batch.block_undo(&blocks[1].hash).unwrap().is_some());
    }

    #[test]
    fn planning_skips_settled_account_inventory_beyond_the_former_account_limit() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 128 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        crate::causal_cementation::cement(&store, blocks[0].hash).unwrap();
        let expected = preflight(&store, blocks[1].hash).unwrap().blocks().to_vec();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0..131_073u64 {
            let mut key = [93; 32];
            key[..8].copy_from_slice(&index.to_be_bytes());
            let address = burst_crypto::derive_address(&burst_types::PublicKey(key));
            // Empty account records have no provisional chain to traverse.
            let mut account =
                crate::genesis_state::account_to_write(None, address, BlockHash::ZERO).unwrap();
            account.block_count = 0;
            account.state = burst_types::WalletState::Unverified;
            account.verified_at = None;
            batch.put_account_info(&account, false).unwrap();
        }
        batch.commit().unwrap();
        assert_eq!(
            preflight(&store, blocks[1].hash).unwrap().blocks(),
            expected
        );
    }

    #[test]
    fn cemented_targets_and_missing_history_are_not_rollback_candidates() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, Some(3));
        assert!(preflight(&store, blocks[1].hash)
            .err()
            .unwrap()
            .to_string()
            .contains("missing historical undo"));
        crate::causal_cementation::cement(&store, blocks[1].hash).unwrap();
        assert!(preflight(&store, blocks[1].hash)
            .err()
            .unwrap()
            .to_string()
            .contains("cemented"));
        assert_eq!(
            store
                .account_store()
                .get_account(&blocks[0].account)
                .unwrap()
                .head,
            blocks[3].hash
        );
    }

    #[test]
    fn external_economic_change_blocks_the_entire_plan() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("test_economic_snapshot", b"another operation")
            .unwrap();
        batch.commit().unwrap();
        assert!(preflight(&store, blocks[1].hash)
            .err()
            .unwrap()
            .to_string()
            .contains("outside the dependency closure"));
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_meta("test_economic_snapshot").unwrap(),
            Some(b"another operation".to_vec())
        );
        for block in &blocks {
            assert!(batch.block_undo(&block.hash).unwrap().is_some());
        }
    }

    #[test]
    fn corrupt_block_and_account_indexes_fail_before_planning() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let mut corrupted = blocks[4].clone();
        corrupted.trst_balance = 99;
        store
            .block_store()
            .put_block(&corrupted.hash, &bincode::serialize(&corrupted).unwrap())
            .unwrap();
        assert!(preflight(&store, blocks[1].hash)
            .err()
            .unwrap()
            .to_string()
            .contains("hash mismatch"));
        store
            .block_store()
            .put_block(&blocks[4].hash, &bincode::serialize(&blocks[4]).unwrap())
            .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut account = batch.get_account(&blocks[0].account).unwrap();
        account.confirmation_height = account.block_count + 1;
        batch
            .put_account(&account.address, &bincode::serialize(&account).unwrap())
            .unwrap();
        batch.commit().unwrap();
        assert!(preflight(&store, blocks[2].hash).is_err());
    }

    #[test]
    fn independent_unconfirmed_accounts_are_not_silently_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let a = block(1, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let b = block(2, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        save(&store, &a, 1, true);
        save(&store, &b, 2, true);
        assert!(preflight(&store, a.hash).is_err());
        assert_eq!(preflight(&store, b.hash).unwrap().blocks(), &[b.hash]);
        assert!(store.block_store().get_block(&a.hash).is_ok());
        assert!(store.block_store().get_block(&b.hash).is_ok());
    }

    fn stage_replacement(
        batch: &mut LmdbWriteTransaction<'_>,
        winner: &StateBlock,
    ) -> Result<u8, StoreError> {
        let previous = match batch.get_account(&winner.account) {
            Ok(account) => Some(account),
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        // This fixture tests the storage boundary, not economic admission.
        crate::ledger_updater::stage_block(batch, winner, previous.as_ref(), 0, None, None)
            .map_err(StoreError::Corruption)?;
        batch.put_meta("test_economic_snapshot", &[42])?;
        Ok(42)
    }

    #[test]
    fn replacement_commits_cross_account_rewind_and_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        crate::causal_cementation::cement(&store, blocks[0].hash).unwrap();
        let winner = block(
            1,
            BlockType::ChangeRepresentative,
            blocks[0].hash,
            BlockHash::ZERO,
        );
        let representative = &blocks[0].account;
        store
            .meta_store()
            .reserve_consensus_vote(
                &burst_types::BlockHash::new([1; 32]),
                representative,
                &blocks[1].account,
                &blocks[1].previous,
                &blocks[1].hash,
                true,
                0,
            )
            .unwrap();
        store
            .meta_store()
            .reserve_checkpoint_vote(
                burst_types::NetworkId::Dev,
                &blocks[0].hash,
                representative,
                1,
                &blocks[1].hash,
            )
            .unwrap();
        let receipt = replace_with(&store, blocks[1].hash, &winner, stage_replacement).unwrap();
        assert_eq!(receipt.state, 42);
        assert_eq!(
            receipt.removed.iter().map(|b| b.hash).collect::<Vec<_>>(),
            blocks[1..].iter().rev().map(|b| b.hash).collect::<Vec<_>>()
        );
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        // Following another branch never grants permission to equivocate.
        assert!(store
            .meta_store()
            .reserve_consensus_vote(
                &burst_types::BlockHash::new([1; 32]),
                representative,
                &winner.account,
                &winner.previous,
                &winner.hash,
                true,
                0
            )
            .is_err());
        assert_eq!(
            store
                .meta_store()
                .reserve_consensus_vote(
                    &burst_types::BlockHash::new([1; 32]),
                    representative,
                    &blocks[1].account,
                    &blocks[1].previous,
                    &blocks[1].hash,
                    true,
                    0
                )
                .unwrap(),
            2
        );
        assert!(store
            .meta_store()
            .reserve_checkpoint_vote(
                burst_types::NetworkId::Dev,
                &blocks[0].hash,
                representative,
                1,
                &winner.hash
            )
            .is_err());
        let batch = store.tx_begin_write().unwrap();
        for old in &blocks[1..] {
            assert!(matches!(
                batch.get_block(&old.hash),
                Err(StoreError::NotFound(_))
            ));
            assert!(batch.block_height(&old.account, &old.hash).is_err());
            assert!(batch.block_undo(&old.hash).unwrap().is_none());
        }
        assert!(batch.get_account(&blocks[2].account).is_err());
        assert!(batch.get_account(&blocks[5].account).is_err());
        assert_eq!(
            batch.get_account(&winner.account).unwrap().head,
            winner.hash
        );
        assert_eq!(
            batch
                .get_account(&winner.account)
                .unwrap()
                .confirmation_height,
            1
        );
        assert_eq!(
            batch.get_meta("test_economic_snapshot").unwrap(),
            Some(vec![42])
        );
        assert_eq!(
            batch.block_height(&winner.account, &winner.hash).unwrap(),
            2
        );
        assert!(batch.block_undo(&winner.hash).unwrap().is_some());
        drop(batch);

        // Replacing a replacement starts from the common ancestor, never from
        // the discarded branch or its removed cross-account descendants.
        let second = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        replace_with(&store, winner.hash, &second, |batch, block| {
            assert_eq!(batch.get_meta("test_economic_snapshot")?, Some(vec![0]));
            assert_eq!(batch.get_account(&block.account)?.head, blocks[0].hash);
            assert!(batch.get_account(&blocks[2].account).is_err());
            stage_replacement(batch, block)
        })
        .unwrap();
        crate::causal_cementation::cement(&store, second.hash).unwrap();
        assert!(replace_with(&store, second.hash, &winner, stage_replacement).is_err());
    }

    #[test]
    fn replacement_failure_after_staging_restores_every_original_journal() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let originals = {
            let batch = store.tx_begin_write().unwrap();
            blocks
                .iter()
                .map(|b| batch.block_undo(&b.hash).unwrap())
                .collect::<Vec<_>>()
        };
        let winner = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        let result = replace_with(&store, blocks[1].hash, &winner, |batch, block| {
            stage_replacement(batch, block)?;
            Err::<(), _>(invalid("injected failure after candidate writes"))
        });
        assert!(result.is_err());
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let batch = store.tx_begin_write().unwrap();
        for (block, journal) in blocks.iter().zip(originals) {
            assert_eq!(
                batch.get_block(&block.hash).unwrap(),
                bincode::serialize(block).unwrap()
            );
            assert_eq!(batch.block_undo(&block.hash).unwrap(), journal);
        }
        assert_eq!(
            batch.get_meta("test_economic_snapshot").unwrap(),
            Some(vec![5])
        );
        assert_eq!(
            batch.get_account(&blocks[0].account).unwrap().head,
            blocks[3].hash
        );
        assert!(batch.get_block(&winner.hash).is_err());
        assert!(batch.block_undo(&winner.hash).unwrap().is_none());
    }

    #[test]
    fn replacement_rejects_unrelated_candidates_and_removed_dependencies_before_application() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let other = block(4, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let dangling = block(1, BlockType::Receive, blocks[0].hash, blocks[4].hash);
        let mut corrupt = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        corrupt.brn_balance = 99;
        for candidate in [&other, &dangling, &corrupt, &blocks[1]] {
            assert!(
                replace_with::<()>(&store, blocks[1].hash, candidate, |_, _| {
                    panic!("invalid replacement reached application")
                })
                .is_err()
            );
        }
        let batch = store.tx_begin_write().unwrap();
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
        }
    }

    #[test]
    fn replacement_callback_cannot_omit_the_candidate_or_its_journal() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let winner = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        assert!(replace_with(&store, blocks[1].hash, &winner, |_, _| Ok(())).is_err());
        assert!(
            replace_with(&store, blocks[1].hash, &winner, |batch, block| {
                let previous = batch.get_account(&block.account)?;
                batch.put_block_with_account(
                    &block.hash,
                    &bincode::serialize(block).unwrap(),
                    &block.account,
                    2,
                )?;
                batch.put_frontier(&block.account, &block.hash)?;
                crate::ledger_updater::update_account_on_block(batch, block, Some(&previous), 0)
                    .map_err(StoreError::Corruption)?;
                Ok(())
            })
            .err()
            .unwrap()
            .to_string()
            .contains("undo boundary")
        );
        let batch = store.tx_begin_write().unwrap();
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
        }
        assert!(batch.get_block(&winner.hash).is_err());
    }

    #[test]
    fn a_failed_later_rewind_poisons_the_whole_writer() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&blocks[5].hash).unwrap();
        // Skip a dependent. Even if a caller ignores the error, no partial
        // rollback may commit.
        assert!(batch.rewind_block(&blocks[1].hash).is_err());
        assert!(batch.commit().is_err());
        let batch = store.tx_begin_write().unwrap();
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
            assert!(batch.block_undo(&block.hash).unwrap().is_some());
        }
    }

    #[test]
    fn a_queued_confirmation_protects_its_unconfirmed_ancestors() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        store
            .meta_store()
            .enqueue_confirmation(&blocks[4].hash)
            .unwrap();
        let winner = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        assert!(replace_with::<()>(&store, blocks[1].hash, &winner, |_, _| {
            panic!("queued finality must be checked before candidate application")
        })
        .err()
        .unwrap()
        .to_string()
        .contains("durable confirmation"));
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&blocks[5].hash).unwrap();
        assert!(batch.rewind_block(&blocks[4].hash).is_err());
        assert!(batch.commit().is_err());
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![blocks[4].hash]
        );
        let batch = store.tx_begin_write().unwrap();
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
        }
    }

    #[test]
    fn ignored_map_full_during_replacement_cannot_commit_a_partial_branch() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = history(&store, None);
        let winner = block(1, BlockType::VerifierOptIn, blocks[0].hash, BlockHash::ZERO);
        assert!(
            replace_with(&store, blocks[1].hash, &winner, |batch, block| {
                stage_replacement(batch, block)?;
                assert!(batch
                    .put_meta("too_large", &vec![7; 32 * 1024 * 1024])
                    .is_err());
                Ok(()) // Even incorrectly ignoring the storage failure is safe.
            })
            .is_err()
        );
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let batch = store.tx_begin_write().unwrap();
        for block in &blocks {
            assert!(batch.get_block(&block.hash).is_ok());
            assert!(batch.block_undo(&block.hash).unwrap().is_some());
        }
        assert_eq!(
            batch.get_meta("test_economic_snapshot").unwrap(),
            Some(vec![5])
        );
        assert!(batch.get_block(&winner.hash).is_err());
        assert!(batch.get_meta("too_large").unwrap().is_none());
    }

    #[test]
    fn replacing_a_received_transfer_restores_portfolios_and_pending_provenance() {
        use crate::ledger_updater::{persist_block, persist_economics, stage_block};
        use burst_store::{meta::MetaStore, pending::PendingStore};
        use burst_trst::{MergerGraph, TrstEngine};
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut brn = burst_brn::BrnEngine::with_rate(100, Timestamp::new(0));
        let mut trst = TrstEngine::with_expiry(1_000);
        let mut creator = block(4, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        creator.timestamp = Timestamp::new(0);
        creator.hash = creator.compute_hash();
        save(&store, &creator, 0, true);
        brn.track_wallet(
            creator.account.clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(0)),
        );
        let mut funded = block(1, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let mut burn = block(
            4,
            BlockType::Burn,
            creator.hash,
            BlockHash::new(burst_crypto::decode_address(funded.account.as_str()).unwrap()),
        );
        burn.brn_balance = 100;
        burn.timestamp = Timestamp::new(10);
        burn.hash = burn.compute_hash();
        let crate::ledger_bridge::EconomicResult::BurnAndMint {
            mint_token: Some(token),
            ..
        } = crate::ledger_bridge::process_block_economics(
            &burn,
            &mut brn,
            &mut trst,
            burn.timestamp,
            1_000,
            0,
        )
        else {
            panic!("funding burn failed")
        };
        let origin = token.origin;
        let provenance = vec![burst_trst::ConsumedProvenance {
            basis: token.basis,
            amount: token.amount,
            origin: token.origin,
            origin_wallet: token.origin_wallet,
            origin_timestamp: token.origin_timestamp,
            effective_origin_timestamp: token.effective_origin_timestamp,
        }];
        let previous = store.account_store().get_account(&creator.account).unwrap();
        persist_block(
            &store,
            &burn,
            Some(&previous),
            0,
            Some((100, funded.account.clone(), provenance)).as_ref(),
            Some(&(origin, token.id, Timestamp::new(1_010))),
            (&brn, &trst),
        )
        .unwrap();
        funded.link = burn.hash;
        funded.trst_balance = 100;
        funded.hash = funded.compute_hash();
        let pending = store
            .pending_store()
            .get_pending(&funded.account, &burn.hash.into_tx_hash())
            .unwrap();
        trst.receive_token(
            crate::ledger_bridge::create_received_token(&funded, &pending, 1_000),
            funded.timestamp,
        );
        // Storage/economic test, not network admission: identity verification,
        // signatures and work are covered by the node admission tests.
        persist_block(&store, &funded, None, 0, None, None, (&brn, &trst)).unwrap();
        let receiver = block(2, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO).account;
        let destination = block(3, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO).account;
        let mut send = block(
            1,
            BlockType::Send,
            funded.hash,
            BlockHash::new(burst_crypto::decode_address(receiver.as_str()).unwrap()),
        );
        send.trst_balance = 60;
        send.origin = origin;
        send.hash = send.compute_hash();
        let provenance = trst.debit_wallet_with_provenance(&send.account, &origin, 40);
        let previous = store.account_store().get_account(&send.account).unwrap();
        persist_block(
            &store,
            &send,
            Some(&previous),
            0,
            Some(&(40, receiver.clone(), provenance)),
            None,
            (&brn, &trst),
        )
        .unwrap();
        let pending = store
            .pending_store()
            .get_pending(&receiver, &send.hash.into_tx_hash())
            .unwrap();
        let mut receive = block(2, BlockType::Open, BlockHash::ZERO, send.hash);
        receive.trst_balance = 40;
        receive.hash = receive.compute_hash();
        trst.receive_token(
            crate::ledger_bridge::create_received_token(&receive, &pending, 1_000),
            receive.timestamp,
        );
        persist_block(&store, &receive, None, 0, None, None, (&brn, &trst)).unwrap();
        assert_eq!(trst.transferable_balance_snapshot(&receiver), Some(40));

        let mut winner = send.clone();
        winner.link = BlockHash::new(burst_crypto::decode_address(destination.as_str()).unwrap());
        winner.trst_balance = 30;
        winner.hash = winner.compute_hash();
        let installed = replace_with(&store, send.hash, &winner, |batch, winner| {
            let bytes = batch.get_meta(TrstEngine::meta_key())?.unwrap();
            let mut restored = TrstEngine::load_wallets(&bytes, 1_000).unwrap();
            restored.merger_graph =
                MergerGraph::from_bytes(&batch.get_meta("merger_graph")?.unwrap()).unwrap();
            restored.rebuild_indexes();
            assert_eq!(
                restored.transferable_balance_snapshot(&winner.account),
                Some(100)
            );
            assert!(restored.get_portfolio(&receiver).is_none());
            let previous = batch.get_account(&winner.account)?;
            let consumed =
                restored.debit_wallet_with_provenance(&winner.account, &winner.origin, 70);
            assert_eq!(consumed.iter().map(|p| p.amount).sum::<u128>(), 70);
            stage_block(
                batch,
                winner,
                Some(&previous),
                0,
                Some(&(70, destination.clone(), consumed)),
                None,
            )
            .map_err(StoreError::Corruption)?;
            persist_economics(batch, &brn, &restored).map_err(StoreError::Corruption)?;
            Ok(restored)
        })
        .unwrap();
        assert_eq!(
            installed.removed.iter().map(|b| b.hash).collect::<Vec<_>>(),
            vec![receive.hash, send.hash]
        );
        assert_eq!(
            installed
                .state
                .transferable_balance_snapshot(&funded.account),
            Some(30)
        );
        assert!(installed.state.get_portfolio(&receiver).is_none());
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let restored = TrstEngine::load_wallets(
            &store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            1_000,
        )
        .unwrap();
        assert_eq!(
            restored.transferable_balance_snapshot(&funded.account),
            Some(30)
        );
        assert!(restored.get_portfolio(&receiver).is_none());
        assert!(!store.account_store().exists(&receiver).unwrap());
        assert!(store
            .pending_store()
            .get_pending(&receiver, &send.hash.into_tx_hash())
            .is_err());
        let pending = store
            .pending_store()
            .get_pending(&destination, &winner.hash.into_tx_hash())
            .unwrap();
        assert_eq!(pending.amount, 70);
        assert_eq!(pending.provenance.len(), 1);
        let provenance = &pending.provenance[0];
        assert_eq!(provenance.amount, 70);
        assert_eq!(provenance.origin, origin);
        assert_eq!(provenance.origin_wallet, creator.account);
        assert_eq!(provenance.origin_timestamp, Timestamp::new(10));
        assert_eq!(provenance.effective_origin_timestamp, Timestamp::new(10));
    }
}
