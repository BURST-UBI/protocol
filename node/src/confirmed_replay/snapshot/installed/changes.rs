//! Replay-derived deltas, not complete or installable block journals.

use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use crate::confirmed_replay::{ConfirmedReplay, ReplayError};
use burst_store_lmdb::block_undo::{UndoChange, UndoTable};
use burst_types::{BlockHash, Timestamp};

const MAX_BYTES: usize = 64 << 20;
pub(super) const MAX_ROWS: usize = 131_072;
const MAX_WORK_BYTES: usize = 256 << 20;
mod aggregates;
mod expiry;
pub(super) mod finality;
mod recorded;
mod rows;
mod verified_count;

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
    pub governance: Option<burst_store_lmdb::write_batch::GovernanceUndoProjection>,
}

pub(super) struct Rows {
    values: rows::ProjectionRows,
    publication_accounts: Option<
        std::collections::HashMap<burst_types::WalletAddress, burst_store::account::AccountInfo>,
    >,
    publication_account_rows: usize,
    publication_lifecycle: Option<std::collections::HashMap<String, Vec<u8>>>,
    bytes: usize,
    trst: burst_trst::TrstEngine,
    stable_holdings: bool,
    source_stable_holdings: bool,
    rate_records: bool,
    source_rate_records: bool,
    wallet_records: bool,
    source_wallet_records: bool,
    wallets: burst_brn::wallet_states::WalletStates,
    complete_wallets: bool,
    aggregates: Option<aggregates::Aggregates>,
    governance_projection: Option<burst_governance::GovernanceEngine>,
    verification_projection: Option<burst_verification::VerificationOrchestrator>,
    source_normalized_verification: Option<bool>,
    source_keyed_governance: Option<bool>,
    keyed_governance: bool,
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
            publication_accounts: None,
            publication_account_rows: 0,
            publication_lifecycle: None,
            bytes: 0,
            trst: Default::default(),
            stable_holdings: false,
            source_stable_holdings: false,
            rate_records: false,
            source_rate_records: false,
            wallet_records: false,
            source_wallet_records: false,
            wallets: Default::default(),
            complete_wallets: false,
            aggregates: None,
            governance_projection: None,
            verification_projection: None,
            source_normalized_verification: None,
            source_keyed_governance: None,
            keyed_governance: false,
        })
    }

    pub(super) fn verify_format(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        if let Some(expected) = self.source_keyed_governance {
            if batch.uses_governance_records()? != expected {
                return Err(invalid(
                    "governance storage format changed during replay derivation",
                ));
            }
        }
        if let Some(expected) = self.source_normalized_verification {
            if batch.uses_normalized_verification_records()? != expected {
                return Err(invalid(
                    "verification storage format changed during replay derivation",
                ));
            }
        }
        if batch.uses_brn_wallet_records()? != self.source_wallet_records {
            return Err(invalid(
                "BRN wallet storage format changed during replay derivation",
            ));
        }
        if batch.uses_brn_rate_records()? != self.source_rate_records {
            return Err(invalid(
                "BRN rate storage format changed during replay derivation",
            ));
        }
        if batch.uses_stable_trst_records()? != self.source_stable_holdings {
            return Err(invalid(
                "TRST storage format changed during replay derivation",
            ));
        }
        Ok(())
    }

    /// Stage caches in the caller's transaction. On error, discard the transaction;
    /// earlier writes may already have changed it. This method does not commit.
    pub(super) fn stage_caches(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.verify_format(batch)?;
        if self.wallet_records && !self.complete_wallets {
            return Err(invalid("cannot stage a partial wallet projection"));
        }
        if self.publication_account_rows > 0 && self.publication_accounts.is_none() {
            return Err(invalid("incomplete typed account projection"));
        }
        if let Some(accounts) = &self.publication_accounts {
            for account in accounts.values() {
                let mut account = account.clone();
                account.confirmation_height =
                    batch.get_account(&account.address)?.confirmation_height;
                batch.put_account_info(&account, false)?;
            }
        }
        if let Some(metadata) = &self.publication_lifecycle {
            for (key, value) in metadata {
                if !burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key)
                    || crate::verification_storage::is_state_key(key)
                {
                    return Err(invalid("unexpected typed lifecycle metadata"));
                }
                batch.put_meta(key, value)?;
            }
        }
        if self.rate_records && !self.source_rate_records {
            // Matching normalized journals are installed in this same writer.
            batch.delete_brn_meta(b"rate_history")?;
        }
        if self.wallet_records {
            batch.begin_brn_wallet_records_projection()?;
        }
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
                UndoTable::BrnWallets => {
                    let address = burst_types::WalletAddress::new(
                        std::str::from_utf8(&key).map_err(invalid)?,
                    );
                    if self.wallet_records {
                        batch.put_checkpoint_brn_wallet_header(&address, &value)?;
                    } else {
                        batch.put_brn_wallet(&address, &value)?;
                    }
                }
                _ => (),
            }
            Ok(())
        })?;
        if let Some(verification) = &self.verification_projection {
            batch.publish_recovered_verification(verification)?;
        }
        if let Some(governance) = &self.governance_projection {
            batch.publish_recovered_governance(governance)?;
            if self.keyed_governance && self.source_keyed_governance == Some(false) {
                // Publish the authenticated state before converting it, including
                // an absent legacy engine. Matching keyed undo shares this writer.
                batch.migrate_governance_records()?;
            }
        }
        if self.wallet_records {
            // Start from a validated empty cut. Each typed write updates exact
            // physical-byte accounting and retains the authenticated proof roots.
            batch.read_brn_engine(0, 0)?;
            for (address, wallet) in &self.wallets {
                batch.put_brn_wallet_state(address, wallet)?;
            }
            batch.read_brn_engine(MAX_ROWS, MAX_BYTES)?;
        }
        let verified = self
            .values
            .get(UndoTable::Meta, b"verified_count")?
            .ok_or_else(|| invalid("replay projection lacks verified count"))?;
        batch.put_meta("verified_count", &verified)?;
        if self.stable_holdings {
            crate::trst_storage::stage_replayed_stable(batch, &self.trst)?;
        } else {
            crate::trst_storage::stage_replayed(batch, &self.trst)?;
        }
        crate::graph_storage::stage_replayed(batch, &self.trst.merger_graph)
    }

    fn put(&mut self, table: UndoTable, key: &[u8], value: Vec<u8>) -> Result<(), ReplayError> {
        if self.wallet_records
            && (table == UndoTable::BrnWallets
                || table == UndoTable::BrnMeta
                    && key.starts_with(burst_brn::wallet_storage::ROW_PREFIX))
        {
            return Err(crate::confirmed_replay::invalid(
                "normalized wallet rows must use the complete wallet stream",
            ));
        }
        self.charge_row(key.len(), value.len())?;
        self.values
            .insert(table, key, value)
            .map_err(replay_error)?;
        Ok(())
    }

    fn charge_row(&mut self, key_bytes: usize, value_bytes: usize) -> Result<(), ReplayError> {
        self.charge_bytes(key_bytes, value_bytes)?;
        let publication = self.publication_lifecycle.is_some();
        if self.publication_account_rows > 2 * MAX_ROWS || self.wallets.len() > MAX_ROWS {
            return Err(crate::confirmed_replay::invalid(
                "excessive publication accounts or wallets",
            ));
        }
        let wallet_rows = if self.complete_wallets {
            if publication {
                usize::from(!self.wallets.is_empty())
            } else {
                self.wallets.len()
            }
        } else {
            0
        };
        if self
            .values
            .len()
            .checked_add(if publication {
                usize::from(self.publication_account_rows > 0)
            } else {
                self.publication_account_rows
            })
            .and_then(|count| count.checked_add(usize::from(self.publication_lifecycle.is_some())))
            .and_then(|count| count.checked_add(wallet_rows))
            .and_then(|count| count.checked_add(usize::from(self.governance_projection.is_some())))
            .and_then(|count| {
                count.checked_add(usize::from(self.verification_projection.is_some()))
            })
            .is_none_or(|count| count >= MAX_ROWS)
        {
            return Err(crate::confirmed_replay::invalid(
                "duplicate or excessive replay rows",
            ));
        }
        Ok(())
    }

    fn byte_budget(&self) -> usize {
        // Publication keeps the replay maps and allows a larger byte total than
        // an undo projection. This accounting does not bound CPU time or RAM.
        if self.publication_lifecycle.is_some() {
            MAX_WORK_BYTES
        } else {
            MAX_BYTES
        }
    }

    fn charge_bytes(&mut self, key_bytes: usize, value_bytes: usize) -> Result<(), ReplayError> {
        let size = key_bytes
            .checked_add(value_bytes)
            .filter(|size| *size <= MAX_BYTES)
            .ok_or_else(|| crate::confirmed_replay::invalid("replay row exceeds byte budget"))?;
        let limit = self.byte_budget();
        charge(&mut self.bytes, size, limit)
    }

    fn encoded<T: serde::Serialize>(
        &mut self,
        table: UndoTable,
        key: &[u8],
        value: &T,
    ) -> Result<(), ReplayError> {
        let size = bincode::serialized_size(value).map_err(replay_error)?;
        if size > MAX_BYTES.saturating_sub(key.len()) as u64
            || size
                > self
                    .byte_budget()
                    .saturating_sub(self.bytes.saturating_add(key.len())) as u64
        {
            return Err(crate::confirmed_replay::invalid(
                "replay row exceeds byte budget",
            ));
        }
        self.put(table, key, bincode::serialize(value).map_err(replay_error)?)
    }

    #[cfg(test)]
    fn capture(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        rate_records: bool,
    ) -> Result<Self, ReplayError> {
        Self::capture_projection(state, cache_at, directory, true, rate_records)
    }

    #[cfg(test)]
    fn capture_projection(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        include_recorded: bool,
        rate_records: bool,
    ) -> Result<Self, ReplayError> {
        Self::capture_projection_records(
            state,
            cache_at,
            directory,
            include_recorded,
            rate_records,
            false,
        )
    }

    #[cfg(test)]
    fn capture_projection_records(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        include_recorded: bool,
        rate_records: bool,
        wallet_records: bool,
    ) -> Result<Self, ReplayError> {
        Self::capture_projection_inner(
            state,
            cache_at,
            directory,
            include_recorded,
            rate_records,
            wallet_records,
            None,
            false,
        )
    }

    fn capture_publication(
        mut state: ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        rate_records: bool,
        wallet_records: bool,
    ) -> Result<Self, ReplayError> {
        let mut rows = Self::capture_projection_inner(
            &state,
            cache_at,
            directory,
            true,
            rate_records,
            wallet_records,
            None,
            true,
        )?;
        let mut accounts = std::mem::take(&mut state.accounts);
        for (address, account) in &mut accounts {
            if address != &account.address {
                return Err(crate::confirmed_replay::invalid(
                    "publication account key differs from address",
                ));
            }
            *account = expiry::normalized_account(account, &rows.trst, cache_at)?;
        }
        rows.publication_accounts = Some(accounts);
        let mut metadata = std::mem::take(&mut state.lifecycle_metadata);
        metadata.retain(|key, _| !crate::verification_storage::is_state_key(key));
        rows.publication_lifecycle = Some(metadata);
        Ok(rows)
    }

    fn capture_projection_inner(
        state: &ConfirmedReplay,
        cache_at: Timestamp,
        directory: &std::path::Path,
        include_recorded: bool,
        rate_records: bool,
        wallet_records: bool,
        transition_count: Option<u64>,
        typed_publication: bool,
    ) -> Result<Self, ReplayError> {
        let mut rows = Self::new(directory).map_err(replay_error)?;
        rows.values = rows::ProjectionRows::bulk(directory).map_err(replay_error)?;
        if typed_publication {
            if state.accounts.len() > MAX_ROWS || state.brn.wallets.len() > MAX_ROWS {
                return Err(crate::confirmed_replay::invalid(
                    "excessive publication accounts or wallets",
                ));
            }
            // capture_publication moves the lifecycle map into these rows.
            // Count its key/value bytes without building a second scratch index.
            rows.publication_lifecycle = Some(Default::default());
            rows.verification_projection = Some(state.verification.storage_clone());
            rows.governance_projection = Some(
                state
                    .governance
                    .as_ref()
                    .ok_or_else(|| {
                        crate::confirmed_replay::invalid("replay lacks governance state")
                    })?
                    .governance
                    .clone(),
            );
        }
        rows.rate_records = rate_records;
        rows.source_rate_records = rate_records;
        rows.wallet_records = wallet_records;
        rows.source_wallet_records = wallet_records;
        if wallet_records {
            rows.wallets = state.brn.wallets.clone();
        }
        rows.complete_wallets = wallet_records && include_recorded;
        let expiry = expiry::ExpiryView::new(&state.trst, state.params.trst_expiry_secs, cache_at)?;
        for (address, account) in
            state
                .accounts
                .iter()
                .take(if include_recorded { usize::MAX } else { 0 })
        {
            if typed_publication {
                let account = expiry.account(account)?;
                let bytes =
                    usize::try_from(bincode::serialized_size(&account).map_err(replay_error)?)
                        .map_err(replay_error)?;
                rows.charge_row(address.as_str().len(), bytes)?;
                rows.publication_account_rows += 1;
                if account.block_count > 0 {
                    rows.charge_row(address.as_str().len(), 32)?;
                    rows.publication_account_rows += 1;
                }
            } else {
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
        }
        let verified = transition_count.unwrap_or_else(|| verified_count::full(state));
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
            if wallet_records {
                recorded::visit_wallet_delta(address, None, Some(wallet), |change| {
                    // Publication streams child records from retained roots.
                    // Count their encoded keys and values without indexing each proof.
                    rows.charge_bytes(
                        change.key.len(),
                        change.after.as_ref().expect("new wallet row").len(),
                    )
                })?;
            } else {
                rows.encoded(UndoTable::BrnWallets, address.as_str().as_bytes(), wallet)?;
            }
        }
        rows.put(
            UndoTable::BrnMeta,
            b"next_stake_id",
            state.brn.next_stake_id().to_be_bytes().to_vec(),
        )?;
        rows.trst = expiry.trst;
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
            if typed_publication {
                rows.charge_bytes(key.len(), value.len())?;
            } else {
                rows.put(UndoTable::Meta, key.as_bytes(), value.clone())?;
            }
        }
        let aggregates = aggregates::Aggregates::capture(state)?;
        if transition_count.is_some() {
            rows.aggregates = Some(aggregates);
        } else {
            aggregates.write_all(&mut rows)?;
        }
        rows.values = rows.values.finish_bulk().map_err(replay_error)?;
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
    #[cfg(test)]
    pub projected_transitions: usize,
    pub records: ScratchSequence<ReplayedEconomicChange>,
    pub rows: Rows,
}

pub(super) fn derive(
    directory: &std::path::Path,
    recovered: &RecoveredSnapshot,
    tail: &super::super::tail::cemented::AppliedTail,
    cache_at: Timestamp,
    provisional: &std::collections::HashSet<BlockHash>,
    economics: &super::economics::AppliedEconomics,
    stable_holdings: bool,
    source_stable_holdings: bool,
    rate_records: bool,
    source_rate_records: bool,
    wallet_records: bool,
    source_wallet_records: bool,
    normalized_verification: bool,
    keyed_governance: bool,
    source_keyed_governance: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<DerivedChanges>, StoreError> {
    if cancelled() {
        return Ok(None);
    }
    if cache_at.as_secs() < recovered.continuation.anchor().cutoff() {
        return Err(invalid(
            "recovery cache clock precedes the authenticated cutoff",
        ));
    }
    #[cfg(test)]
    let derivation_started = std::time::Instant::now();
    #[cfg(test)]
    let mut oracle_time = std::time::Duration::ZERO;
    #[cfg(test)]
    let mut projected_transitions = 0;
    let mut previous = Rows::new(directory)?;
    #[cfg(test)]
    let mut oracle_previous = Rows::new(directory)?;
    let mut retained = 0;
    let mut entries = 0;
    let mut work = 0;
    let mut verified = None;
    let mut output = ScratchSequence::new(directory)?;
    let mut interrupted = false;
    let state = if provisional.is_empty() {
        // Economics already replayed this applied tail and authenticated its
        // published engines. With no undo transitions to derive, reuse that
        // final state; publication still validates the live writer afterward.
        Ok(economics.state.clone())
    } else {
        recovered.continuation.replay_applied_recorded_transitions(
            tail,
            &recovered.selections,
            &economics.ordering,
            !wallet_records,
            &mut |before, block, state| {
                if cancelled() {
                    interrupted = true;
                    return Err(crate::confirmed_replay::invalid(
                        "rollback derivation cancelled",
                    ));
                }
                if block.is_none() {
                    verified = Some(verified_count::full(state));
                    return Ok(());
                }
                let count = verified.as_mut().ok_or_else(|| {
                    crate::confirmed_replay::invalid("missing recovery verified count")
                })?;
                if !before {
                    *count = verified_count::advance(
                        *count,
                        state.mutations.as_ref().ok_or_else(|| {
                            crate::confirmed_replay::invalid("missing recovery mutations")
                        })?,
                    )?;
                }
                let Some(selected) = block.filter(|block| provisional.contains(&block.hash)) else {
                    return Ok(());
                };
                if before {
                    #[cfg(test)]
                    {
                        projected_transitions += 1;
                    }
                    previous = Rows::capture_projection_inner(
                        state,
                        cache_at,
                        directory,
                        false,
                        rate_records,
                        wallet_records,
                        Some(*count),
                        false,
                    )?;
                    #[cfg(test)]
                    {
                        let oracle_started = std::time::Instant::now();
                        oracle_previous = Rows::capture_projection_records(
                            state,
                            cache_at,
                            directory,
                            true,
                            rate_records,
                            wallet_records,
                        )?;
                        oracle_time += oracle_started.elapsed();
                    }
                    return Ok(());
                }
                let block = Some(selected);
                let mut current = Rows::capture_projection_inner(
                    state,
                    cache_at,
                    directory,
                    false,
                    rate_records,
                    wallet_records,
                    Some(*count),
                    false,
                )?;
                let aggregates = aggregates::materialize_transition(
                    &mut previous,
                    &mut current,
                    normalized_verification,
                    keyed_governance,
                )?;
                charge(&mut work, previous.bytes, MAX_WORK_BYTES)?;
                #[cfg(test)]
                let oracle_current = {
                    let oracle_started = std::time::Instant::now();
                    let rows = Rows::capture_projection_records(
                        state,
                        cache_at,
                        directory,
                        true,
                        rate_records,
                        wallet_records,
                    )?;
                    oracle_time += oracle_started.elapsed();
                    rows
                };
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
                            if cancelled() {
                                interrupted = true;
                                return Err(invalid("rollback row derivation cancelled"));
                            }
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
                            if cancelled() {
                                interrupted = true;
                                return Err(invalid("rollback row derivation cancelled"));
                            }
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
                    if stable_holdings {
                        current
                            .trst
                            .visit_portfolio_changes(&previous.trst, |holder, old, new| {
                                use burst_trst::engine::holding_rows;
                                holding_rows::visit_delta(
                                    holder,
                                    old,
                                    new,
                                    |error| error,
                                    |old, new| {
                                        if old.iter().chain(new.iter()).any(|row| {
                                            matches!(row, holding_rows::Row::Holding { .. })
                                        }) {
                                            cache_holders.insert(holder.clone());
                                            if cache_holders.len() > MAX_ROWS {
                                                return Err("replay cache holder budget exceeded"
                                                    .to_owned());
                                            }
                                        }
                                        record_delta(
                                            old.map(|row| row.storage_entry()).transpose()?,
                                            new.map(|row| row.storage_entry()).transpose()?,
                                            &mut changes,
                                            &mut entries,
                                            &mut retained,
                                            &mut work,
                                        )
                                    },
                                )
                            })
                            .map_err(replay_error)?;
                        current
                            .trst
                            .visit_wallet_metadata_delta(&previous.trst, |old, new| {
                                record_delta(
                                    old.map(|row| row.storage_entry()).transpose()?,
                                    new.map(|row| row.storage_entry()).transpose()?,
                                    &mut changes,
                                    &mut entries,
                                    &mut retained,
                                    &mut work,
                                )
                            })
                            .map_err(replay_error)?;
                    } else {
                        current
                            .trst
                            .visit_wallet_record_delta(&previous.trst, |before, after| {
                                for row in before.iter().chain(after.iter()) {
                                    if let burst_trst::engine::WalletRecord::Holding {
                                        holder,
                                        ..
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
                    }
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
                    recorded::visit_with_wallet_records(
                        state,
                        &previous.trst,
                        &current.trst,
                        cache_at,
                        &cache_holders,
                        wallet_records.then_some(&previous.wallets),
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
                    if let Some((before, after)) = aggregates.verification {
                        let base = burst_store_lmdb::block_undo::BlockUndo {
                            block: block.hash,
                            account: block.account.clone(),
                            height: account.block_count,
                            changes: std::mem::take(&mut changes),
                        };
                        let projected =
                            burst_store_lmdb::block_undo::verification::project_transition(
                                &base, &before, &after,
                            )
                            .map_err(replay_error)?;
                        changes = base.changes;
                        for row in projected {
                            append_change(
                                row,
                                &mut changes,
                                &mut entries,
                                &mut retained,
                                &mut work,
                            )?;
                        }
                    }
                    let governance = if let Some((before, after)) = aggregates.governance {
                        let base = burst_store_lmdb::block_undo::BlockUndo {
                            block: block.hash,
                            account: block.account.clone(),
                            height: account.block_count,
                            changes: std::mem::take(&mut changes),
                        };
                        let projection = burst_store_lmdb::write_batch::prepare_governance_undo(
                            &base, &before, &after,
                        )
                        .map_err(replay_error)?;
                        changes = base.changes;
                        let (count, bytes) = projection.retained_budget();
                        charge(&mut entries, count, MAX_ROWS)?;
                        charge(&mut retained, bytes, MAX_BYTES)?;
                        charge(&mut work, bytes, MAX_WORK_BYTES)?;
                        Some(projection)
                    } else {
                        None
                    };
                    changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
                    #[cfg(test)]
                    {
                        let oracle_started = std::time::Instant::now();
                        let mut projected = changes.clone();
                        if let Some(projection) = &governance {
                            projected.extend_from_slice(projection.changes());
                        }
                        projected.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
                        recorded::compare_full_projection(
                            &oracle_previous,
                            &oracle_current,
                            &projected,
                        )?;
                        oracle_time += oracle_started.elapsed();
                    }
                    output
                        .push(&ReplayedEconomicChange {
                            block: block.hash,
                            account: block.account.clone(),
                            height: account.block_count,
                            changes,
                            governance,
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
    };
    if interrupted {
        return Ok(None);
    }
    let mut state = state.map_err(invalid)?;
    if cancelled() {
        return Ok(None);
    }
    #[cfg(test)]
    let publication_started = std::time::Instant::now();
    if !provisional.is_empty() {
        super::publication::materialize(
            economics.state.verification.snapshot(),
            &mut state,
            &recovered.selections,
            directory,
        )?;
    }
    if cancelled() {
        return Ok(None);
    }
    let projection_started = std::time::Instant::now();
    let mut rows =
        Rows::capture_publication(state, cache_at, directory, rate_records, wallet_records)
            .map_err(invalid)?;
    tracing::debug!(
        elapsed_ms = projection_started.elapsed().as_millis(),
        rows = rows.values.len(),
        "recovery publication captured projection rows"
    );
    if cancelled() {
        return Ok(None);
    }
    rows.stable_holdings = stable_holdings;
    rows.source_stable_holdings = source_stable_holdings;
    rows.source_rate_records = source_rate_records;
    rows.source_wallet_records = source_wallet_records;
    rows.source_normalized_verification = Some(normalized_verification);
    rows.source_keyed_governance = Some(source_keyed_governance);
    rows.keyed_governance = keyed_governance;
    charge(&mut work, rows.bytes, MAX_WORK_BYTES).map_err(invalid)?;
    #[cfg(test)]
    if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
        eprintln!("ROLLBACK_DERIVATION provisional={} transitions={} total_us={} oracle_us={} publication_us={} projection_us={}",
            provisional.len(), projected_transitions, derivation_started.elapsed().as_micros(),
            oracle_time.as_micros(), publication_started.elapsed().as_micros(),
            projection_started.elapsed().as_micros());
    }
    Ok(Some(DerivedChanges {
        #[cfg(test)]
        projected_transitions,
        records: output,
        rows,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    mod wallet_stream;

    #[test]
    fn replay_projection_rechecks_selected_holding_format() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut rows = Rows::new(directory.path()).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        rows.verify_format(&batch).unwrap();
        batch
            .put_meta(crate::trst_storage::FORMAT_KEY, &[2])
            .unwrap();
        assert!(rows.verify_format(&batch).is_err());
        rows.source_stable_holdings = true;
        rows.verify_format(&batch).unwrap();
        batch
            .put_meta(crate::trst_storage::FORMAT_KEY, &[1])
            .unwrap();
        assert!(rows.verify_format(&batch).is_err());
        rows.source_stable_holdings = false;
        rows.verify_format(&batch).unwrap();
        batch.initialize_brn_wallet_records_if_empty().unwrap();
        assert!(rows.verify_format(&batch).is_err());
        rows.source_wallet_records = true;
        rows.verify_format(&batch).unwrap();
        batch
            .put_meta(
                crate::endorsement_commit::SNAPSHOT_KEY,
                &burst_verification::VerificationOrchestrator::new()
                    .snapshot()
                    .encode()
                    .unwrap(),
            )
            .unwrap();
        rows.source_normalized_verification = Some(false);
        rows.verify_format(&batch).unwrap();
        batch.normalize_verification_records().unwrap();
        assert!(rows.verify_format(&batch).is_err());
        rows.source_normalized_verification = Some(true);
        rows.verify_format(&batch).unwrap();
        rows.source_keyed_governance = Some(false);
        rows.keyed_governance = true; // Destination must not replace the source race guard.
        rows.verify_format(&batch).unwrap();
        batch
            .initialize_governance_records(&burst_governance::GovernanceEngine::new())
            .unwrap();
        assert!(rows.verify_format(&batch).is_err());
        rows.source_keyed_governance = Some(true);
        rows.verify_format(&batch).unwrap();
    }

    #[test]
    fn certified_governance_projection_migrates_absent_or_legacy_state_atomically() {
        let engine = burst_governance::GovernanceEngine::new();
        let expected = bincode::serialize(&engine).unwrap();
        for legacy_present in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            if legacy_present {
                let mut batch = store.tx_begin_write().unwrap();
                batch.put_brn_meta(b"governance_engine", &expected).unwrap();
                batch.commit().unwrap();
            }
            let mut rows = Rows::new(directory.path()).unwrap();
            rows.put(
                UndoTable::Meta,
                b"verified_count",
                0u64.to_be_bytes().to_vec(),
            )
            .unwrap();
            rows.governance_projection = Some(engine.clone());
            rows.source_keyed_governance = Some(false);
            rows.keyed_governance = true;
            for commit in [false, true] {
                let mut batch = store.tx_begin_write().unwrap();
                rows.stage_caches(&mut batch).unwrap();
                assert!(batch.uses_governance_records().unwrap());
                assert!(batch
                    .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")
                    .unwrap()
                    .is_none());
                assert_eq!(
                    bincode::serialize(&crate::governance_storage::load(&batch).unwrap().unwrap())
                        .unwrap(),
                    expected
                );
                if commit {
                    batch.commit().unwrap();
                } else {
                    drop(batch);
                    let batch = store.tx_begin_write().unwrap();
                    assert!(!batch.uses_governance_records().unwrap());
                    assert_eq!(
                        batch
                            .undo_value(UndoTable::BrnMeta, b"governance_engine")
                            .unwrap(),
                        legacy_present.then(|| expected.clone())
                    );
                }
            }
            drop(store);
            let reopened = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            let batch = reopened.tx_begin_write().unwrap();
            assert!(batch.uses_governance_records().unwrap());
            assert_eq!(
                bincode::serialize(&crate::governance_storage::load(&batch).unwrap().unwrap())
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn rate_projection_preserves_selected_format_and_checks_it_before_staging() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap()
        .into_state();
        state
            .brn
            .rate_history
            .apply_rate_change(17, Timestamp::new(1001))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        for normalized in [false, true] {
            let rows =
                Rows::capture(&state, Timestamp::new(1002), directory.path(), normalized).unwrap();
            let legacy = rows
                .values
                .get(UndoTable::BrnMeta, b"rate_history")
                .unwrap();
            assert_eq!(legacy.is_none(), normalized);
            if normalized {
                let header = rows
                    .values
                    .get(UndoTable::BrnMeta, burst_brn::rate_storage::HEADER)
                    .unwrap()
                    .unwrap();
                let mut rates = Vec::new();
                rows.values
                    .visit(|table, key, value| {
                        if table == UndoTable::BrnMeta
                            && key.starts_with(burst_brn::rate_storage::ROW_PREFIX)
                        {
                            rates.push((key, value));
                        }
                        Ok(())
                    })
                    .unwrap();
                rates.sort_by(|a, b| a.0.cmp(&b.0));
                assert_eq!(
                    burst_brn::rate_storage::decode(&header, rates.into_iter().map(Ok)).unwrap(),
                    state.brn.rate_history
                );
            } else {
                assert_eq!(
                    legacy.unwrap(),
                    bincode::serialize(&state.brn.rate_history).unwrap()
                );
            }
            let db = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(db.path(), 32, 16 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            if normalized {
                batch.put_brn_rate_history(&state.brn.rate_history).unwrap();
            }
            rows.verify_format(&batch).unwrap();
            if normalized {
                batch
                    .delete_brn_meta(burst_brn::rate_storage::HEADER)
                    .unwrap();
            } else {
                batch.put_brn_rate_history(&state.brn.rate_history).unwrap();
            }
            assert!(rows.verify_format(&batch).is_err());
        }
    }

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
        let before = Rows::capture_projection(&state, at, directory.path(), false, false).unwrap();
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
        let after = Rows::capture_projection(&state, at, directory.path(), false, false).unwrap();
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
            assert_eq!(batch.verified_accounts().unwrap(), vec![address.clone()]);
            assert!(batch.get_account(&address).unwrap().state.is_verified());
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert!(store
                    .tx_begin_write()
                    .unwrap()
                    .verified_accounts()
                    .unwrap()
                    .is_empty());
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
            store.tx_begin_write().unwrap().verified_accounts().unwrap(),
            vec![address]
        );
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
    fn typed_account_publication_matches_serialized_projection_and_preserves_limits() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 64, 32 << 20).unwrap();
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap()
        .into_state();
        let prototype = state.accounts.values().next().unwrap().clone();
        for n in 1..=1024u64 {
            let mut account = prototype.clone();
            account.address = burst_types::WalletAddress::new(format!("brst_projection_{n}"));
            account.head = BlockHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()));
            account.expired_trst = 9;
            account.revoked_trst = 7;
            state.accounts.insert(account.address.clone(), account);
        }
        for n in 0..1024 {
            state.lifecycle_metadata.insert(
                format!("verification_identity_source/v1/brst_projection_{n}"),
                vec![n as u8; 32],
            );
        }
        let expected_metadata = state.lifecycle_metadata.clone();
        let metadata_key = expected_metadata.keys().next().unwrap().clone();
        let metadata_allocation = state.lifecycle_metadata[&metadata_key].as_ptr();
        let at = state.cutoff;
        let mut batch = store.tx_begin_write().unwrap();
        for account in state.accounts.values() {
            let mut account = account.clone();
            account.confirmation_height = 0;
            batch.put_account_info(&account, false).unwrap();
        }
        batch.commit().unwrap();
        let legacy = Rows::capture(&state, at, directory.path(), false).unwrap();
        let expected_bytes = legacy.bytes;
        let mut batch = store.tx_begin_write().unwrap();
        legacy.stage_caches(&mut batch).unwrap();
        let expected: Vec<_> = state
            .accounts
            .keys()
            .map(|address| (address.clone(), batch.get_account(address).unwrap()))
            .collect();
        drop(batch);
        drop(legacy);
        let address = expected[0].0.clone();
        let allocation =
            state.accounts.get(&address).unwrap() as *const burst_store::account::AccountInfo;
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
            directory.path(),
            128 << 10,
        )
        .unwrap();
        let rows = Rows::capture_publication(state, at, directory.path(), false, false).unwrap();
        assert_eq!(
            rows.bytes, expected_bytes,
            "typed publication must keep all logical byte charges"
        );
        assert_eq!(rows.publication_account_rows, 2 * expected.len());
        assert_eq!(
            rows.publication_lifecycle.as_ref().unwrap(),
            &expected_metadata
        );
        assert_eq!(
            rows.publication_lifecycle.as_ref().unwrap()[&metadata_key].as_ptr(),
            metadata_allocation,
            "publication must transfer metadata values, not clone them"
        );
        assert_eq!(
            rows.publication_accounts
                .as_ref()
                .unwrap()
                .get(&address)
                .unwrap() as *const _,
            allocation,
            "publication must transfer the replay map, not clone it"
        );
        rows.values
            .visit(|table, key, _| {
                assert!(!matches!(table, UndoTable::Accounts | UndoTable::Frontiers));
                assert!(
                    !(table == UndoTable::Meta
                        && key.starts_with(b"verification_identity_source/"))
                );
                Ok(())
            })
            .unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            for key in expected_metadata.keys() {
                assert!(
                    batch.get_meta(key).unwrap().is_none(),
                    "aborted metadata must not leak"
                );
            }
            rows.stage_caches(&mut batch).unwrap();
            for (key, value) in &expected_metadata {
                assert_eq!(batch.get_meta(key).unwrap().as_ref(), Some(value));
            }
            for (address, account) in &expected {
                assert_eq!(&batch.get_account(address).unwrap(), account);
                assert_eq!(account.confirmation_height, 0);
            }
            if commit {
                batch.commit().unwrap();
            }
        }
        drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        let mut rejected = Rows::new(directory.path()).unwrap();
        rejected.publication_account_rows = 2 * MAX_ROWS + 1;
        assert!(rejected.put(UndoTable::Meta, b"overflow", vec![1]).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert!(rejected
            .stage_caches(&mut batch)
            .unwrap_err()
            .to_string()
            .contains("incomplete typed account"));
    }

    #[test]
    fn lifecycle_publication_transfers_large_inventory_without_relaxing_delta_limits() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 64, crate::node::DEFAULT_MAP_SIZE).unwrap();
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap()
        .into_state();
        let prototype = state.accounts.values().next().unwrap().clone();
        for n in 1..100_000u64 {
            let mut account = prototype.clone();
            account.address = burst_types::WalletAddress::new(format!("brst_{n:060}"));
            state.brn.wallets.insert(
                account.address.clone(),
                burst_brn::BrnWalletState::new(state.cutoff),
            );
            let address = account.address.clone();
            state.accounts.insert(account.address.clone(), account);
            for prefix in [
                "verification_identity_source",
                "verification_identity_binding",
                "verification_identity_bound",
            ] {
                state
                    .lifecycle_metadata
                    .insert(format!("{prefix}/v1/{address}"), vec![1; 32]);
            }
        }
        let at = state.cutoff;
        let count = state.projected_lifecycle_metadata().count();
        assert!(2 * state.accounts.len() + count > MAX_ROWS);
        assert!(
            Rows::capture(&state, at, directory.path(), false).is_err(),
            "full delta capture retains its aggregate row limit"
        );
        let mut batch = store.tx_begin_write().unwrap();
        for account in state.accounts.values() {
            let mut account = account.clone();
            account.confirmation_height = 0;
            batch.put_account_info(&account, false).unwrap();
        }
        batch.begin_brn_wallet_records_projection().unwrap();
        batch
            .put_brn_meta(b"next_stake_id", &state.brn.next_stake_id().to_be_bytes())
            .unwrap();
        batch
            .put_brn_meta(
                b"rate_history",
                &bincode::serialize(&state.brn.rate_history).unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
            directory.path(),
            128 << 10,
        )
        .unwrap();
        let rows = Rows::capture_publication(state, at, directory.path(), false, true).unwrap();
        assert_eq!(rows.publication_lifecycle.as_ref().unwrap().len(), count);
        assert_eq!(rows.publication_accounts.as_ref().unwrap().len(), 100_000);
        assert_eq!(rows.wallets.len(), 100_000);
        assert!(
            rows.bytes > MAX_BYTES,
            "exercise streamed publication beyond one undo image"
        );
        assert!(rows.bytes < MAX_WORK_BYTES);
        assert!(
            rows.values.len() < 32,
            "lifecycle rows must not enter the scratch index"
        );
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch
                .read_brn_engine(MAX_ROWS, MAX_BYTES)
                .unwrap()
                .wallets
                .is_empty());
            for key in rows.publication_lifecycle.as_ref().unwrap().keys() {
                assert!(
                    batch.get_meta(key).unwrap().is_none(),
                    "aborted metadata must not leak"
                );
            }
            rows.stage_caches(&mut batch).unwrap();
            assert_eq!(
                batch.read_brn_engine(MAX_ROWS, MAX_BYTES).unwrap().wallets,
                rows.wallets
            );
            for (key, value) in rows.publication_lifecycle.as_ref().unwrap() {
                assert_eq!(batch.get_meta(key).unwrap().as_ref(), Some(value));
            }
            for account in rows.publication_accounts.as_ref().unwrap().values() {
                let mut expected = account.clone();
                expected.confirmation_height = 0;
                assert_eq!(batch.get_account(&account.address).unwrap(), expected);
            }
            if commit {
                batch.commit().unwrap();
            }
        }
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.read_brn_engine(MAX_ROWS, MAX_BYTES).unwrap().wallets,
            rows.wallets
        );
        assert_eq!(batch.account_entries().unwrap().count(), 100_000);
        for (key, value) in rows.publication_lifecycle.as_ref().unwrap() {
            assert_eq!(batch.get_meta(key).unwrap().as_ref(), Some(value));
        }
        drop(batch);
        drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn publication_keeps_individual_row_and_total_work_limits() {
        let directory = tempfile::tempdir().unwrap();
        let mut rows = Rows::new(directory.path()).unwrap();
        rows.publication_lifecycle = Some(Default::default());
        assert!(rows.charge_bytes(1, MAX_BYTES).is_err());
        assert_eq!(rows.bytes, 0);
        rows.charge_bytes(0, MAX_BYTES).unwrap();
        rows.charge_bytes(0, MAX_BYTES).unwrap();
        rows.charge_bytes(0, MAX_BYTES).unwrap();
        rows.charge_bytes(0, MAX_BYTES).unwrap();
        assert_eq!(rows.bytes, MAX_WORK_BYTES);
        assert!(rows.charge_bytes(1, 0).is_err());
        let mut delta = Rows::new(directory.path()).unwrap();
        delta.charge_bytes(0, MAX_BYTES).unwrap();
        assert!(delta.charge_bytes(1, 0).is_err());
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
