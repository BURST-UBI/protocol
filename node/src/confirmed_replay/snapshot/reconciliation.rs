//! Local finality reconciliation before compact installation. The successful
//! result owns the writer, so confirmations cannot race the checked state.
//! Gated startup recovers runtime state before admission opens; the live
//! installer publishes the projection under the runtime's engine locks.

use super::{archive::AuthenticatedSnapshot, history::HistoricalBlock};
use crate::confirmed_replay::installation::{authority_bytes, CUTOFF_KEY};
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use std::collections::HashSet;

pub(in crate::confirmed_replay::snapshot) mod promises;
mod projection;
mod rows;
pub use projection::{PreparedSnapshotState, ProjectedSnapshot};

const MAX_ACCOUNTS: usize = 131_072;

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

/// An exclusive, uncommitted installation boundary. Dropping it aborts the
/// transaction. This is not an installed node or permission to prune history.
pub struct ReconciledSnapshot<'db> {
    transaction: LmdbWriteTransaction<'db>,
    snapshot: AuthenticatedSnapshot,
    confirmed_accounts: usize,
    confirmation_targets: Vec<BlockHash>,
    retained_promises: Vec<BlockHash>,
    witnesses: rows::Rows,
}

/// Download hints, not authorization. Local state can change after this read;
/// reconciliation checks current finality again under its exclusive writer.
pub struct InstallationNeeds {
    pub cemented_heads: Vec<crate::confirmed_history::HistoryHead>,
    pub confirmation_targets: Vec<BlockHash>,
    /// Bodies needed to index every opened account in the compact projection.
    /// A local body still needs authentication against the snapshot head.
    pub snapshot_heads: Vec<crate::confirmed_history::HistoryHead>,
}

impl ReconciledSnapshot<'_> {
    pub fn snapshot(&self) -> &AuthenticatedSnapshot {
        &self.snapshot
    }
    pub fn local_epoch(&self) -> Result<BlockHash, StoreError> {
        self.transaction.payment_epoch()
    }
    pub fn confirmed_accounts(&self) -> usize {
        self.confirmed_accounts
    }
    pub fn confirmation_targets(&self) -> &[BlockHash] {
        &self.confirmation_targets
    }
    /// These choices remain durable whether included, excluded, or not covered
    /// by the supplied witnesses. Missing membership is not proof of exclusion.
    pub fn retained_promises(&self) -> &[BlockHash] {
        &self.retained_promises
    }
}

impl AuthenticatedSnapshot {
    /// Identify old references needing membership proofs without taking the
    /// writer or downloading every account's entire prefix.
    pub fn installation_needs(
        &self,
        store: &burst_store_lmdb::LmdbStore,
    ) -> Result<InstallationNeeds, StoreError> {
        let txn = store.tx_begin_read()?;
        let mut cemented_heads = Vec::new();
        let account_store = store.account_store();
        for (index, account) in account_store.account_entries(&txn)?.enumerate() {
            if index >= MAX_ACCOUNTS {
                return Err(invalid("account inventory exceeds replay limit"));
            }
            let account = account?;
            let height = account.confirmation_height;
            if height == 0 {
                continue;
            }
            let hash = store
                .block_store()
                .block_at_height_txn(&txn, &account.address, height)?
                .ok_or_else(|| invalid("local cemented head is missing"))?;
            let target = self
                .state
                .account(&account.address)
                .ok_or_else(|| invalid("snapshot omits a cemented account"))?;
            if target.block_count < height || (target.block_count == height && target.head != hash)
            {
                return Err(invalid("snapshot conflicts with local finality"));
            }
            if target.block_count > height {
                cemented_heads.push(crate::confirmed_history::HistoryHead {
                    account: account.address,
                    height,
                    hash,
                });
            }
        }
        let mut snapshot_heads: Vec<_> = self
            .state
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
            .map(|account| crate::confirmed_history::HistoryHead {
                account: account.address.clone(),
                height: account.block_count,
                hash: account.head,
            })
            .collect();
        snapshot_heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        let heads: HashSet<_> = snapshot_heads.iter().map(|head| head.hash).collect();
        let confirmation_targets = store
            .meta_store()
            .pending_confirmations_txn(&txn)?
            .into_iter()
            .filter(|hash| !heads.contains(hash))
            .collect();
        Ok(InstallationNeeds {
            cemented_heads,
            confirmation_targets,
            snapshot_heads,
        })
    }

    /// Supply witnesses before acquiring the writer; never wait for peers while
    /// holding it. A missing proof fails and drops the whole transaction. Retry
    /// with newly obtained witnesses; no stale read-only approval is cached.
    pub fn reconcile_installation<'db>(
        self,
        transaction: LmdbWriteTransaction<'db>,
        network: NetworkId,
        initial_params: &ProtocolParams,
        witnesses: &[HistoricalBlock],
    ) -> Result<ReconciledSnapshot<'db>, StoreError> {
        transaction.require_clean_checkpoint_boundary()?;
        self.validate_installation_base(network, initial_params)?;
        self.validate_selections().map_err(invalid)?;
        let epoch = transaction.payment_epoch()?;
        let mut local = None;
        let local_tip = crate::checkpoint_store::reconcile_snapshot_path(
            &transaction,
            self.authority_path().map(|result| {
                let anchor = result.map_err(invalid)?;
                if anchor.hash() == epoch {
                    local = Some(anchor.clone());
                }
                Ok(anchor)
            }),
        )?;
        crate::checkpoint_agreement::reconcile_snapshot_journals(
            &transaction,
            self.authority_path().map(|result| result.map_err(invalid)),
        )?;
        let local =
            local.ok_or_else(|| invalid("snapshot does not descend from the installed epoch"))?;
        if self.anchor().height() <= local.height() {
            return Err(invalid("snapshot does not advance the installed epoch"));
        }
        if local_tip < local.height() {
            return Err(invalid("installed epoch has no accepted local checkpoint"));
        }
        let genesis = crate::genesis_state::block(network, initial_params);
        if transaction.block_at_height(&genesis.account, 1)? != Some(genesis.hash)
            || transaction.block_height(&genesis.account, &genesis.hash)? != 1
            || transaction.get_block(&genesis.hash)?
                != bincode::serialize(&genesis).map_err(invalid)?
        {
            return Err(invalid("snapshot differs from the local genesis"));
        }
        if let Some(bytes) = transaction.get_meta("genesis_params/v1")? {
            use bincode::Options;
            let pinned: ProtocolParams = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .with_limit(64 * 1024)
                .deserialize(&bytes)
                .map_err(invalid)?;
            if pinned.params_hash() != initial_params.params_hash() {
                return Err(invalid("snapshot differs from pinned genesis parameters"));
            }
        }
        if transaction
            .get_meta("ledger_network")?
            .is_some_and(|bytes| bytes != network.as_str().as_bytes())
        {
            return Err(invalid("snapshot belongs to a different local network"));
        }
        let authority = crate::payment_authority::storage::read_at(|key| transaction.get_meta(key))?;
        let cutoff = transaction.get_meta(CUTOFF_KEY)?;
        let installed_cutoff = if local.height() == 0 {
            genesis.timestamp.as_secs()
        } else {
            local.cutoff()
        };
        if local.height() == 0 && authority.is_none() {
            if transaction.has_confirmed_governance_storage()? {
                return Err(invalid("genesis epoch has orphaned governance metadata"));
            }
            for key in [
                CUTOFF_KEY,
                burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
                burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                crate::confirmed_governance::KEY,
            ] {
                if transaction.get_meta(key)?.is_some() {
                    return Err(invalid("genesis epoch has orphaned authority metadata"));
                }
            }
        } else if authority != Some(authority_bytes(&local, installed_cutoff)?)
            || cutoff.as_deref() != Some(installed_cutoff.to_be_bytes().as_slice())
        {
            return Err(invalid(
                "installed snapshot authority or cutoff is inconsistent",
            ));
        }
        if authority.is_some() {
            let bytes = transaction
                .get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
                .ok_or_else(|| invalid("installed vote limit is missing"))?;
            let limit = burst_store::meta::decode_payment_vote_limit(&bytes, epoch)?;
            if limit.is_some_and(|at| at <= installed_cutoff)
                || (local.height() == 0 && limit.is_some())
            {
                return Err(invalid("installed vote limit is inconsistent"));
            }
            crate::confirmed_governance::ConfirmedGovernance::load(&transaction)?
                .ok_or_else(|| invalid("installed governance schedule is missing"))?;
        }
        let context = self.history_context();
        let mut positions = std::collections::HashMap::new();
        let mut included = HashSet::new();
        for account in self
            .state
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
        {
            positions.insert((account.address.clone(), account.block_count), account.head);
            included.insert(account.head);
        }
        let mut seen = HashSet::new();
        let mut bytes = 0u64;
        for witness in witnesses {
            bytes = bytes
                .checked_add(bincode::serialized_size(witness.block()).map_err(invalid)?)
                .filter(|size| *size <= super::tail::MAX_TAIL_BYTES as u64)
                .ok_or_else(|| invalid("installation witnesses exceed byte budget"))?;
            if seen.len() >= super::tail::MAX_TAIL_BLOCKS
                || witness.context() != context
                || !seen.insert(witness.block().hash)
            {
                return Err(invalid(
                    "duplicate, oversized or foreign installation witness",
                ));
            }
            if positions
                .insert(
                    (witness.block().account.clone(), witness.height()),
                    witness.block().hash,
                )
                .is_some_and(|hash| hash != witness.block().hash)
            {
                return Err(invalid(
                    "installation witnesses disagree on an account position",
                ));
            }
            included.insert(witness.block().hash);
        }
        let mut confirmed_accounts = 0;
        for account in transaction.account_entries()? {
            let account = account?;
            if account.head.is_zero() != (account.block_count == 0) {
                return Err(invalid("invalid local account head"));
            }
            if account.confirmation_height == 0 {
                continue;
            }
            let height = account.confirmation_height;
            let actual = transaction
                .block_at_height(&account.address, height)?
                .ok_or_else(|| invalid("local cemented head is missing"))?;
            if transaction.block_height(&account.address, &actual)? != height {
                return Err(invalid("local cemented head has inconsistent indexes"));
            }
            if account.block_count == height && actual != account.head {
                return Err(invalid("local cemented head disagrees with its account"));
            }
            if positions.get(&(account.address, height)) != Some(&actual) {
                return Err(invalid(
                    "snapshot lacks proof preserving a local cemented head",
                ));
            }
            confirmed_accounts += 1;
        }
        let prefix = b"pending_confirmation/v1/";
        let mut confirmation_targets = Vec::new();
        for (key, value) in transaction.checkpoint_metadata(prefix)? {
            let hash = BlockHash::new(key[prefix.len()..].try_into().map_err(invalid)?);
            if hash.is_zero() || value != [1] || !included.contains(&hash) {
                return Err(invalid("snapshot omits a durable confirmation target"));
            }
            confirmation_targets.push(hash);
        }
        let retained_promises = retained_promises(&transaction, epoch)?;
        let mut staged = rows::Rows::new(transaction.storage_directory())?;
        for witness in witnesses {
            staged.insert(witness)?;
        }
        Ok(ReconciledSnapshot {
            transaction,
            snapshot: self,
            confirmed_accounts,
            confirmation_targets,
            retained_promises,
            witnesses: staged,
        })
    }

    fn validate_installation_base(
        &self,
        network: NetworkId,
        params: &ProtocolParams,
    ) -> Result<(), StoreError> {
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, params)?;
        let base = &self.authority_base;
        if base.hash() != anchor.hash()
            || base.network() != network
            || base.genesis_hash() != anchor.genesis_hash()
            || base.height() != 0
            || base.cutoff() != anchor.cutoff()
            || base.roster().commitment() != anchor.roster().commitment()
            || base.quorum_bps() != anchor.quorum_bps()
        {
            return Err(invalid(
                "snapshot installation needs authority from configured genesis",
            ));
        }
        Ok(())
    }
}

pub(in crate::confirmed_replay::snapshot) fn retained_promises(
    transaction: &LmdbWriteTransaction<'_>,
    epoch: BlockHash,
) -> Result<Vec<BlockHash>, StoreError> {
    let mut promises = HashSet::new();
    let mut rows = [0usize; 2];
    let mut bytes = [0usize; 2];
    visit_retained_promises(transaction, epoch, |hash, size| {
        let namespace = usize::from(hash.is_none());
        rows[namespace] += 1;
        bytes[namespace] = bytes[namespace]
            .checked_add(size)
            .ok_or_else(|| invalid("payment promise inventory overflow"))?;
        if rows[namespace] > 131_072 || bytes[namespace] > 64 << 20 {
            return Err(invalid(
                "payment promise inventory exceeds operation budget",
            ));
        }
        if let Some(hash) = hash {
            promises.insert(hash);
        }
        Ok(())
    })?;
    let mut promises: Vec<_> = promises.into_iter().collect();
    promises.sort_by_key(|hash| *hash.as_bytes());
    Ok(promises)
}

pub(in crate::confirmed_replay::snapshot) fn visit_retained_promises(
    transaction: &LmdbWriteTransaction<'_>,
    epoch: BlockHash,
    visit: impl FnMut(Option<BlockHash>, usize) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    visit_retained_promises_view(&transaction.read_view(), epoch, visit)
}

pub(in crate::confirmed_replay::snapshot) fn visit_retained_promises_view(
    transaction: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    epoch: BlockHash,
    mut visit: impl FnMut(Option<BlockHash>, usize) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    for prefix in [b"consensus_final/v1/".as_slice(), b"consensus_fence/v1/"] {
        if transaction
            .checkpoint_metadata_entries(prefix)?
            .next()
            .transpose()?
            .is_some()
        {
            return Err(invalid("legacy signing promises require reconstruction"));
        }
    }
    for namespace in ["consensus_final", "consensus_fence"] {
        let prefix = format!("{namespace}/v2/{epoch}/");
        for row in transaction.checkpoint_metadata_entries(prefix.as_bytes())? {
            let (key, value) = row?;
            let promise = promises::decode(std::str::from_utf8(key).map_err(invalid)?, value)?;
            let block = match promise {
                promises::PaymentPromise::Final { block, .. } => Some(block),
                promises::PaymentPromise::Fence { .. } => None,
            };
            visit(block, key.len() + value.len())?;
        }
    }
    Ok(())
}
