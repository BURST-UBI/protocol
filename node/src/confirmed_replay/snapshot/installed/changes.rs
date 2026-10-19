//! Replay-derived deltas, not complete or installable block journals.

use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use crate::confirmed_replay::{ConfirmedReplay, ReplayError};
use burst_store_lmdb::block_undo::{UndoChange, UndoTable};
use burst_types::{BlockHash, Timestamp};

const MAX_BYTES: usize = 64 << 20;
const MAX_ROWS: usize = 131_072;
const MAX_WORK_BYTES: usize = 256 << 20;
mod expiry;
pub(super) mod finality;
mod recorded;
mod rows;

/// Raw ledger and economic rows derived from signed replay, with expiry caches
/// normalized to one recovery cache clock. Arrival-order effects and matching
/// live cache publication still need reconciliation before journal installation.
/// This type grants no rollback authority.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ReplayedEconomicChange {
    pub block: BlockHash,
    pub account: burst_types::WalletAddress,
    pub height: u64,
    pub changes: Vec<UndoChange>,
}

pub(super) struct Rows {
    values: rows::ProjectionRows,
    bytes: usize,
    trst: burst_trst::TrstEngine,
}

fn charge(used: &mut usize, amount: usize, limit: usize) -> Result<(), ReplayError> {
    *used = used
        .checked_add(amount)
        .filter(|total| *total <= limit)
        .ok_or_else(|| crate::confirmed_replay::invalid("replay delta budget exceeded"))?;
    Ok(())
}

fn append_change(
    change: UndoChange,
    changes: &mut Vec<UndoChange>,
    entries: &mut usize,
    retained: &mut usize,
    work: &mut usize,
) -> Result<(), ReplayError> {
    if change.before == change.after {
        return Ok(());
    }
    let bytes = change
        .key
        .len()
        .checked_add(change.before.as_ref().map_or(0, Vec::len))
        .and_then(|bytes| bytes.checked_add(change.after.as_ref().map_or(0, Vec::len)))
        .ok_or_else(|| crate::confirmed_replay::invalid("replay delta size overflow"))?;
    charge(entries, 1, MAX_ROWS)?;
    charge(retained, bytes, MAX_BYTES)?;
    charge(work, bytes, MAX_WORK_BYTES)?;
    changes.push(change);
    Ok(())
}

impl Rows {
    fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            values: rows::ProjectionRows::new(directory)?,
            bytes: 0,
            trst: Default::default(),
        })
    }

    pub(super) fn stage_caches(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.values.visit(|table, key, value| {
            match table {
                UndoTable::Accounts => {
                    let mut account: burst_store::account::AccountInfo =
                        bincode::deserialize(&value).map_err(invalid)?;
                    account.confirmation_height =
                        batch.get_account(&account.address)?.confirmation_height;
                    batch.put_account_info(&account, false)?;
                }
                UndoTable::Meta => {
                    let key = std::str::from_utf8(&key).map_err(invalid)?;
                    // Account writes adjust this counter. Publish its replayed
                    // value only after every account row has been staged.
                    if key == "verified_count" {
                        return Ok(());
                    }
                    if !(burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(
                        key,
                    ) || matches!(key, "verified_count" | "consti_engine")
                        || key == burst_governance::DelegationEngine::meta_key())
                    {
                        return Err(invalid("unexpected replay cache metadata"));
                    }
                    batch.put_meta(key, &value)?;
                }
                UndoTable::BrnMeta => batch.put_brn_meta(&key, &value)?,
                UndoTable::BrnWallets => batch.put_brn_wallet(
                    &burst_types::WalletAddress::new(std::str::from_utf8(&key).map_err(invalid)?),
                    &value,
                )?,
                _ => (),
            }
            Ok(())
        })?;
        let verified = self
            .values
            .get(UndoTable::Meta, b"verified_count")?
            .ok_or_else(|| invalid("replay projection lacks verified count"))?;
        batch.put_meta("verified_count", &verified)?;
        crate::trst_storage::stage_replayed(batch, &self.trst)?;
        crate::graph_storage::stage_replayed(batch, &self.trst.merger_graph)
    }

    fn put(&mut self, table: UndoTable, key: &[u8], value: Vec<u8>) -> Result<(), ReplayError> {
        charge(&mut self.bytes, key.len(), MAX_BYTES)?;
        charge(&mut self.bytes, value.len(), MAX_BYTES)?;
        if self.values.len() >= MAX_ROWS {
            return Err(crate::confirmed_replay::invalid(
                "duplicate or excessive replay rows",
            ));
        }
        self.values
            .insert(table, key, value)
            .map_err(replay_error)?;
        Ok(())
    }

    fn encoded<T: serde::Serialize>(
        &mut self,
        table: UndoTable,
        key: &[u8],
        value: &T,
    ) -> Result<(), ReplayError> {
        let size = bincode::serialized_size(value).map_err(replay_error)?;
        if size > MAX_BYTES.saturating_sub(self.bytes.saturating_add(key.len())) as u64 {
            return Err(crate::confirmed_replay::invalid(
                "replay row exceeds byte budget",
            ));
        }
        self.put(table, key, bincode::serialize(value).map_err(replay_error)?)
    }

    fn capture(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
    ) -> Result<Self, ReplayError> {
        Self::capture_projection(state, cache_at, directory, true)
    }

    fn capture_projection(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        include_recorded: bool,
    ) -> Result<Self, ReplayError> {
        let mut rows = Self::new(directory).map_err(replay_error)?;
        let expiry = expiry::ExpiryView::new(&state.trst, state.params.trst_expiry_secs, cache_at);
        for (address, account) in
            state
                .accounts
                .iter()
                .take(if include_recorded { usize::MAX } else { 0 })
        {
            rows.encoded(
                UndoTable::Accounts,
                address.as_str().as_bytes(),
                &expiry.account(account)?,
            )?;
            if account.block_count > 0 {
                rows.put(
                    UndoTable::Frontiers,
                    address.as_str().as_bytes(),
                    account.head.as_bytes().to_vec(),
                )?;
            }
        }
        let verified = state
            .accounts
            .values()
            .filter(|account| account.state.is_verified())
            .count() as u64;
        rows.put(
            UndoTable::Meta,
            b"verified_count",
            verified.to_be_bytes().to_vec(),
        )?;
        for (token, at) in
            state
                .minted_burns
                .iter()
                .take(if include_recorded { usize::MAX } else { 0 })
        {
            for (table, key, value) in crate::confirmed_replay::mutations::mint_rows(
                *token,
                *at,
                state.params.trst_expiry_secs,
            ) {
                rows.put(table, &key, value)?;
            }
        }
        for ((address, source), pending) in
            state
                .pending
                .iter()
                .take(if include_recorded { usize::MAX } else { 0 })
        {
            let mut key = address.as_str().as_bytes().to_vec();
            key.extend_from_slice(source.as_bytes());
            rows.encoded(UndoTable::Pending, &key, pending)?;
        }
        for (address, wallet) in
            state
                .brn
                .wallets
                .iter()
                .take(if include_recorded { usize::MAX } else { 0 })
        {
            rows.encoded(UndoTable::BrnWallets, address.as_str().as_bytes(), wallet)?;
        }
        rows.encoded(UndoTable::BrnMeta, b"rate_history", &state.brn.rate_history)?;
        rows.put(
            UndoTable::BrnMeta,
            b"next_stake_id",
            state.brn.next_stake_id().to_be_bytes().to_vec(),
        )?;
        rows.trst = expiry.trst;
        rows.put(
            UndoTable::Meta,
            crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
            state
                .verification
                .snapshot()
                .encode()
                .map_err(replay_error)?,
        )?;
        rows.put(
            UndoTable::BrnMeta,
            burst_verification::bootstrap::BootstrapPhase::KEY,
            state.bootstrap_phase.encoding().to_vec(),
        )?;
        rows.encoded(UndoTable::BrnMeta, b"protocol_params", &state.params)?;
        for (key, value) in state
            .projected_lifecycle_metadata()
            .take(if include_recorded { usize::MAX } else { 0 })
        {
            if !burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key) {
                return Err(crate::confirmed_replay::invalid(
                    "non-lifecycle replay metadata",
                ));
            }
            rows.put(UndoTable::Meta, key.as_bytes(), value.clone())?;
        }
        let governance = state
            .governance
            .as_ref()
            .ok_or_else(|| crate::confirmed_replay::invalid("replay lacks governance state"))?;
        rows.put(
            UndoTable::Meta,
            burst_governance::DelegationEngine::meta_key().as_bytes(),
            governance.delegation.save_state(),
        )?;
        rows.encoded(
            UndoTable::BrnMeta,
            b"governance_engine",
            &governance.governance,
        )?;
        rows.encoded(UndoTable::Meta, b"consti_engine", &governance.constitution)?;
        Ok(rows)
    }
}

fn replay_error(error: impl ToString) -> ReplayError {
    crate::confirmed_replay::invalid(error.to_string())
}

fn record_delta(
    before: Option<(String, Vec<u8>)>,
    after: Option<(String, Vec<u8>)>,
    changes: &mut Vec<UndoChange>,
    entries: &mut usize,
    retained: &mut usize,
    work: &mut usize,
) -> Result<(), String> {
    let key = after
        .as_ref()
        .or(before.as_ref())
        .ok_or_else(|| "empty replay economic delta".to_string())?
        .0
        .clone();
    if before.as_ref().is_some_and(|row| row.0 != key) {
        return Err("replay economic delta changed key".to_string());
    }
    let bytes = key
        .len()
        .checked_add(before.as_ref().map_or(0, |row| row.1.len()))
        .and_then(|size| size.checked_add(after.as_ref().map_or(0, |row| row.1.len())))
        .ok_or_else(|| "replay economic delta size overflow".to_string())?;
    charge(entries, 1, MAX_ROWS).map_err(|error| error.to_string())?;
    charge(retained, bytes, MAX_BYTES).map_err(|error| error.to_string())?;
    charge(work, bytes, MAX_WORK_BYTES).map_err(|error| error.to_string())?;
    changes.push(UndoChange {
        table: UndoTable::Meta,
        key: key.into_bytes(),
        before: before.map(|row| row.1),
        after: after.map(|row| row.1),
    });
    Ok(())
}

pub(super) struct DerivedChanges {
    pub records: ScratchSequence<ReplayedEconomicChange>,
    pub rows: Rows,
}

pub(super) fn derive(
    directory: &std::path::Path,
    recovered: &RecoveredSnapshot,
    tail: &super::super::tail::cemented::AppliedTail,
    cache_at: Timestamp,
    ordering: &[(BlockHash, BlockHash)],
    publication: burst_verification::OrchestratorSnapshot,
) -> Result<DerivedChanges, StoreError> {
    if cache_at.as_secs() < recovered.continuation.anchor().cutoff() {
        return Err(invalid(
            "recovery cache clock precedes the authenticated cutoff",
        ));
    }
    let mut previous = Rows::new(directory)?;
    #[cfg(test)]
    let mut oracle_previous = Rows::new(directory)?;
    let mut retained = 0;
    let mut entries = 0;
    let mut work = 0;
    let mut output = ScratchSequence::new(directory)?;
    let mut state = recovered
        .continuation
        .replay_applied_recorded(
            tail,
            &recovered.selections,
            ordering,
            &mut |block, state| {
                let current = Rows::capture_projection(state, cache_at, directory, false)?;
                #[cfg(test)]
                let oracle_current = Rows::capture(state, cache_at, directory)?;
                charge(&mut work, current.bytes, MAX_WORK_BYTES)?;
                if let Some(block) = block {
                    let mut changes = Vec::new();
                    let account = state
                        .account(&block.account)
                        .filter(|account| account.head == block.hash && account.block_count > 0)
                        .ok_or_else(|| {
                            crate::confirmed_replay::invalid(
                                "replayed block has no matching account head",
                            )
                        })?;
                    let mut height_key = block.account.as_str().as_bytes().to_vec();
                    height_key.extend_from_slice(&account.block_count.to_be_bytes());
                    let size = bincode::serialized_size(block).map_err(replay_error)?;
                    if size > MAX_BYTES.saturating_sub(retained) as u64 {
                        return Err(crate::confirmed_replay::invalid(
                            "replayed block exceeds delta budget",
                        ));
                    }
                    for (table, key, after) in [
                        (
                            UndoTable::Blocks,
                            block.hash.as_bytes().to_vec(),
                            bincode::serialize(block).map_err(replay_error)?,
                        ),
                        (
                            UndoTable::Heights,
                            height_key.clone(),
                            block.hash.as_bytes().to_vec(),
                        ),
                        (
                            UndoTable::BlockHeights,
                            block.hash.as_bytes().to_vec(),
                            height_key,
                        ),
                    ] {
                        charge(&mut entries, 1, MAX_ROWS)?;
                        charge(&mut retained, key.len(), MAX_BYTES)?;
                        charge(&mut retained, after.len(), MAX_BYTES)?;
                        changes.push(UndoChange {
                            table,
                            key,
                            before: None,
                            after: Some(after),
                        });
                    }
                    previous
                        .values
                        .visit(|table, key, before| {
                            let after = current.values.get(table, &key)?;
                            if after.as_ref() == Some(&before) {
                                return Ok(());
                            }
                            charge(&mut entries, 1, MAX_ROWS).map_err(invalid)?;
                            charge(&mut retained, key.len(), MAX_BYTES).map_err(invalid)?;
                            charge(&mut retained, before.len(), MAX_BYTES).map_err(invalid)?;
                            charge(&mut retained, after.as_ref().map_or(0, Vec::len), MAX_BYTES)
                                .map_err(invalid)?;
                            changes.push(UndoChange {
                                table,
                                key,
                                before: Some(before),
                                after,
                            });
                            Ok(())
                        })
                        .map_err(replay_error)?;
                    current
                        .values
                        .visit(|table, key, after| {
                            if previous.values.get(table, &key)?.is_some() {
                                return Ok(());
                            }
                            charge(&mut entries, 1, MAX_ROWS).map_err(invalid)?;
                            charge(&mut retained, key.len(), MAX_BYTES).map_err(invalid)?;
                            charge(&mut retained, after.len(), MAX_BYTES).map_err(invalid)?;
                            changes.push(UndoChange {
                                table,
                                key,
                                before: None,
                                after: Some(after),
                            });
                            Ok(())
                        })
                        .map_err(replay_error)?;
                    let mut cache_holders = std::collections::HashSet::new();
                    current
                        .trst
                        .visit_wallet_record_delta(&previous.trst, |before, after| {
                            for row in before.iter().chain(after.iter()) {
                                if let burst_trst::engine::WalletRecord::Holding {
                                    holder, ..
                                } = row
                                {
                                    cache_holders.insert(holder.clone());
                                    if cache_holders.len() > MAX_ROWS {
                                        return Err(
                                            "replay cache holder budget exceeded".to_owned()
                                        );
                                    }
                                }
                            }
                            record_delta(
                                before.map(|row| row.storage_entry()).transpose()?,
                                after.map(|row| row.storage_entry()).transpose()?,
                                &mut changes,
                                &mut entries,
                                &mut retained,
                                &mut work,
                            )
                        })
                        .map_err(replay_error)?;
                    current
                        .trst
                        .merger_graph
                        .visit_record_delta(&previous.trst.merger_graph, |before, after| {
                            record_delta(
                                before.map(|row| row.storage_entry()).transpose()?,
                                after.map(|row| row.storage_entry()).transpose()?,
                                &mut changes,
                                &mut entries,
                                &mut retained,
                                &mut work,
                            )
                        })
                        .map_err(replay_error)?;
                    recorded::visit(
                        state,
                        &previous.trst,
                        &current.trst,
                        cache_at,
                        &cache_holders,
                        |change| {
                            append_change(
                                change,
                                &mut changes,
                                &mut entries,
                                &mut retained,
                                &mut work,
                            )
                        },
                    )?;
                    changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
                    #[cfg(test)]
                    recorded::compare_full_projection(&oracle_previous, &oracle_current, &changes)?;
                    output
                        .push(&ReplayedEconomicChange {
                            block: block.hash,
                            account: block.account.clone(),
                            height: account.block_count,
                            changes,
                        })
                        .map_err(replay_error)?;
                }
                previous = current;
                #[cfg(test)]
                {
                    oracle_previous = oracle_current;
                }
                Ok(())
            },
        )
        .map_err(invalid)?;
    super::publication::materialize(publication, &mut state, &recovered.selections)?;
    let rows = Rows::capture(&state, cache_at, directory).map_err(invalid)?;
    charge(&mut work, rows.bytes, MAX_WORK_BYTES).map_err(invalid)?;
    Ok(DerivedChanges {
        records: output,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_block_projection_does_not_encode_unrelated_accounts_or_claims() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap()
        .state()
        .clone();
        let directory = tempfile::tempdir().unwrap();
        let at = state.cutoff;
        let before = Rows::capture_projection(&state, at, directory.path(), false).unwrap();
        for n in 0..1024 {
            let mut token = [0; 32];
            token[..8].copy_from_slice(&(n as u64).to_be_bytes());
            state
                .minted_burns
                .insert(burst_types::TxHash::new(token), at);
            let holder = burst_types::WalletAddress::new(format!("brst_unrelated_{n}"));
            state.accounts.insert(
                holder.clone(),
                burst_store::account::AccountInfo::unopened(holder.clone()),
            );
            state.pending.insert(
                (holder.clone(), burst_types::TxHash::new([73; 32])),
                burst_store::pending::PendingInfo {
                    source: holder,
                    amount: 1,
                    timestamp: at,
                    provenance: vec![],
                    return_credit: true,
                },
            );
            state.lifecycle_metadata.insert(
                format!("verification_identity_source/unrelated_{n}"),
                vec![0; 32],
            );
        }
        let after = Rows::capture_projection(&state, at, directory.path(), false).unwrap();
        assert_eq!(before.bytes, after.bytes);
        assert_eq!(before.values.len(), after.values.len());
        after
            .values
            .visit(|table, key, value| {
                assert_eq!(before.values.get(table, &key)?.as_ref(), Some(&value));
                assert!(!matches!(
                    table,
                    UndoTable::Accounts
                        | UndoTable::Frontiers
                        | UndoTable::Pending
                        | UndoTable::TrstOrigins
                        | UndoTable::TrstExpiry
                        | UndoTable::TrstReverse
                ));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn streamed_cache_projection_publishes_verified_count_after_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let identity = |table, key: &[u8]| {
            burst_crypto::blake2b_256(&bincode::serialize(&(table, key)).unwrap())
        };
        let counter = identity(UndoTable::Meta, b"verified_count");
        let address = (0..10_000)
            .map(|n| burst_types::WalletAddress::new(format!("brst_projection_{n}")))
            .find(|address| identity(UndoTable::Accounts, address.as_str().as_bytes()) > counter)
            .unwrap();
        let mut account = burst_store::account::AccountInfo::unopened(address.clone());
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        account.state = burst_types::WalletState::Verified;
        let mut rows = Rows::new(directory.path()).unwrap();
        rows.encoded(UndoTable::Accounts, address.as_str().as_bytes(), &account)
            .unwrap();
        rows.put(
            UndoTable::Meta,
            b"verified_count",
            1u64.to_be_bytes().to_vec(),
        )
        .unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            rows.stage_caches(&mut batch).unwrap();
            assert_eq!(batch.verified_account_count().unwrap(), 1);
            assert!(batch.get_account(&address).unwrap().state.is_verified());
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(
                    store
                        .tx_begin_write()
                        .unwrap()
                        .verified_account_count()
                        .unwrap(),
                    0
                );
            }
        }
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .verified_account_count()
                .unwrap(),
            1
        );
    }

    #[test]
    fn replay_rows_enforce_unique_keys_and_aggregate_limits() {
        let directory = tempfile::tempdir().unwrap();
        let mut rows = Rows::new(directory.path()).unwrap();
        rows.put(UndoTable::Meta, b"one", vec![1]).unwrap();
        assert!(rows.put(UndoTable::Meta, b"one", vec![2]).is_err());
        let mut rows = Rows {
            bytes: MAX_BYTES - 3,
            ..Rows::new(directory.path()).unwrap()
        };
        assert!(rows.encoded(UndoTable::BrnMeta, b"x", &1u64).is_err());
        assert_eq!(rows.values.len(), 0);
        let mut used = usize::MAX;
        assert!(charge(&mut used, 1, usize::MAX).is_err());
        let mut used = MAX_WORK_BYTES;
        assert!(charge(&mut used, 1, MAX_WORK_BYTES).is_err());
    }
}
