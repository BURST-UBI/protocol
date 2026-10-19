//! One-view cemented suffix discovery for compact startup validation.
//! Account afterimages select a local cut; they never authenticate its prefix.

use super::super::account_inventory::AccountInventory;
use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

struct Row {
    account: burst_types::WalletAddress,
    height: u64,
    hash: BlockHash,
    length: usize,
    digest: [u8; 32],
}

pub struct IndexedTailPlan<const CONFIRMED: bool> {
    plan: SnapshotTailPlan,
    accounts: AccountInventory,
    rows: Vec<Row>,
}

/// Indexed history with authenticated old-body dependencies. The confirmed
/// and applied forms are distinct types; neither certifies new economics.
pub struct IndexedTail<const CONFIRMED: bool> {
    tail: SnapshotTail,
    accounts: AccountInventory,
    rows: Vec<Row>,
}

pub type CementedTailPlan = IndexedTailPlan<true>;
pub type CementedTail = IndexedTail<true>;
pub type AppliedTailPlan = IndexedTailPlan<false>;
pub type AppliedTail = IndexedTail<false>;

pub(in crate::confirmed_replay::snapshot) fn plan_installed(
    continuation: &super::super::SnapshotContinuation,
    directory: &std::path::Path,
) -> Result<SnapshotTailPlan, ReplayError> {
    let state = continuation.state();
    let heads: Vec<_> = state
        .accounts
        .values()
        .filter(|account| account.block_count > 0)
        .map(|account| HistoryHead {
            account: account.address.clone(),
            height: account.block_count,
            hash: account.head,
        })
        .collect();
    let base = Base {
        anchor: continuation.anchor(),
        context: continuation.history_context(),
        state,
    };
    SnapshotTailPlan::read(
        base,
        directory,
        u64::MAX,
        &heads,
        |_| {
            Err(super::invalid(
                "installed witness plan unexpectedly reads a suffix",
            ))
        },
        |_| {
            Err(StoreError::Corruption(
                "installed witness plan unexpectedly reads content".into(),
            ))
        },
    )
}

/// Capture the empty suffix of a fully installed checkpoint from its writer view.
/// Retained-prefix membership is checked separately by the installation inventory.
pub(in crate::confirmed_replay::snapshot) fn capture_installed<const CONFIRMED: bool>(
    continuation: &super::super::SnapshotContinuation,
    batch: &LmdbWriteTransaction<'_>,
    directory: &std::path::Path,
    witnesses: Arc<HistoricalBlocks>,
) -> Result<IndexedTail<CONFIRMED>, StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    let state = continuation.state();
    let mut heads = Vec::new();
    let mut rows = Vec::new();
    let mut count = 0;
    for account in batch.account_entries()? {
        let account = account?;
        count += 1;
        let expected = state
            .account(&account.address)
            .ok_or_else(|| invalid("post-install account has no replayed origin"))?;
        if account.head != expected.head
            || account.block_count != expected.block_count
            || account.confirmation_height != account.block_count
        {
            return Err(invalid("post-install account differs from replayed cut"));
        }
        if account.block_count == 0 {
            continue;
        }
        if batch.block_at_height(&account.address, account.block_count)? != Some(account.head)
            || batch.block_height(&account.address, &account.head)? != account.block_count
        {
            return Err(invalid(
                "post-install account head index differs from replay",
            ));
        }
        let bytes = batch.get_block(&account.head)?;
        rows.push(Row {
            account: account.address.clone(),
            height: account.block_count,
            hash: account.head,
            length: bytes.len(),
            digest: burst_crypto::blake2b_256(&bytes),
        });
        heads.push(HistoryHead {
            account: account.address,
            height: account.block_count,
            hash: account.head,
        });
    }
    if count != state.accounts.len() {
        return Err(invalid(
            "post-install account inventory differs from replay",
        ));
    }
    let base = Base {
        anchor: continuation.anchor(),
        context: continuation.history_context(),
        state,
    };
    let plan = SnapshotTailPlan::read(
        base,
        directory,
        u64::MAX,
        &heads,
        |hash| batch.get_block(&hash).map_err(super::invalid),
        |key| batch.get_meta(key),
    )
    .map_err(|error| StoreError::Corruption(error.to_string()))?;
    let tail = plan
        .finish_staged(witnesses)
        .map_err(|error| StoreError::Corruption(error.to_string()))?;
    if tail.block_count() != 0 {
        return Err(invalid(
            "post-install capture contains an uninstalled suffix",
        ));
    }
    Ok(IndexedTail {
        tail,
        rows,
        accounts: AccountInventory::read(batch.account_entries()?)?,
    })
}

impl<const CONFIRMED: bool> IndexedTailPlan<CONFIRMED> {
    pub fn witness_plan(&self) -> &SnapshotTailPlan {
        &self.plan
    }

    pub fn finish(
        self,
        retained: Vec<HistoricalBlock>,
    ) -> Result<IndexedTail<CONFIRMED>, ReplayError> {
        Ok(IndexedTail {
            tail: self.plan.finish(retained)?,
            accounts: self.accounts,
            rows: self.rows,
        })
    }
}

impl<const CONFIRMED: bool> IndexedTail<CONFIRMED> {
    pub fn tail(&self) -> &SnapshotTail {
        &self.tail
    }

    pub(crate) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        let invalid = |message: &str| StoreError::Corruption(message.into());
        if batch.payment_epoch()? != self.tail.context().epoch {
            return Err(invalid(
                "cemented suffix belongs to another installed epoch",
            ));
        }
        let current = AccountInventory::read(batch.account_entries()?.map(|account| {
            let account = account?;
            if account.block_count > 0
                && (batch.block_at_height(&account.address, account.block_count)?
                    != Some(account.head)
                    || batch.block_height(&account.address, &account.head)? != account.block_count)
            {
                return Err(invalid(
                    "account head index changed during compact startup validation",
                ));
            }
            Ok(account)
        }))?;
        if current != self.accounts {
            return Err(invalid(
                "account state changed during compact startup validation",
            ));
        }
        for row in &self.rows {
            let bytes = batch.get_block(&row.hash)?;
            if batch.block_at_height(&row.account, row.height)? != Some(row.hash)
                || batch.block_height(&row.account, &row.hash)? != row.height
                || bytes.len() != row.length
                || burst_crypto::blake2b_256(&bytes) != row.digest
            {
                return Err(invalid(
                    "cemented suffix changed during compact startup validation",
                ));
            }
        }
        Ok(())
    }
}

pub(in crate::confirmed_replay::snapshot) fn plan(
    base: Base<'_>,
    store: &LmdbStore,
) -> Result<CementedTailPlan, ReplayError> {
    plan_indexed::<true>(base, store)
}

pub(in crate::confirmed_replay::snapshot) fn plan_applied(
    base: Base<'_>,
    store: &LmdbStore,
) -> Result<AppliedTailPlan, ReplayError> {
    plan_indexed::<false>(base, store)
}

fn plan_indexed<const CONFIRMED: bool>(
    base: Base<'_>,
    store: &LmdbStore,
) -> Result<IndexedTailPlan<CONFIRMED>, ReplayError> {
    let params = base.state.params.clone();
    let txn = store.tx_begin_read().map_err(invalid)?;
    let account_store = store.account_store();
    let blocks = store.block_store();
    let mut heads = Vec::new();
    let mut base_accounts = 0usize;
    let mut rows = Vec::new();
    let mut used = 0usize;
    let mut retain =
        |account: &burst_types::WalletAddress, height, hash| -> Result<(), ReplayError> {
            if rows.len() >= MAX_TAIL_BLOCKS {
                return Err(invalid("compact startup rows exceed block limit"));
            }
            let bytes = blocks.get_block_txn(&txn, &hash).map_err(invalid)?;
            used = used
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("compact startup rows exceed byte limit"))?;
            rows.push(Row {
                account: account.clone(),
                height,
                hash,
                length: bytes.len(),
                digest: burst_crypto::blake2b_256(&bytes),
            });
            Ok(())
        };
    for (index, account) in account_store
        .account_entries(&txn)
        .map_err(invalid)?
        .enumerate()
    {
        if index >= MAX_HEADS {
            return Err(invalid("compact account inventory exceeds limit"));
        }
        let account = account.map_err(invalid)?;
        if account.head.is_zero() != (account.block_count == 0) {
            return Err(invalid("invalid compact account boundary"));
        }
        let previous = base.state.account(&account.address);
        base_accounts += usize::from(previous.is_some());
        let floor = previous.map_or(0, |account| account.block_count);
        if account.confirmation_height < floor {
            return Err(invalid("local finality falls below authenticated snapshot"));
        }
        if account.block_count > 0
            && blocks
                .block_at_height_txn(&txn, &account.address, account.block_count)
                .map_err(invalid)?
                != Some(account.head)
        {
            return Err(invalid("local account head differs from its height index"));
        }
        if floor > 0 {
            let previous = previous.expect("positive base height");
            if blocks
                .block_at_height_txn(&txn, &account.address, floor)
                .map_err(invalid)?
                != Some(previous.head)
            {
                return Err(invalid(
                    "retained snapshot head index differs from authenticated state",
                ));
            }
            let mut cursor = super::super::history::HistoryCursor::new(
                base.anchor,
                base.context.state_hash,
                &account.address,
                Some(previous),
            )?
            .ok_or_else(|| invalid("missing authenticated account cursor"))?;
            let bytes = blocks
                .get_block_txn(&txn, &previous.head)
                .map_err(invalid)?;
            cursor.accept(&[bytes])?;
            retain(&account.address, floor, previous.head)?;
        }
        let height = if CONFIRMED {
            account.confirmation_height
        } else {
            account.block_count
        };
        if height > 0 {
            let hash = blocks
                .block_at_height_txn(&txn, &account.address, height)
                .map_err(invalid)?
                .ok_or_else(|| invalid("cemented head index is missing"))?;
            heads.push(HistoryHead {
                account: account.address.clone(),
                height,
                hash,
            });
        }
    }
    if base_accounts != base.state.accounts.len() {
        return Err(invalid("local ledger omits a snapshot account"));
    }
    let genesis = crate::genesis_key::genesis_address(base.anchor.network());
    let plan = SnapshotTailPlan::read(
        base,
        store.env().path(),
        u64::MAX,
        &heads,
        |hash| blocks.get_block_txn(&txn, &hash).map_err(invalid),
        |key| store.meta_store().get_meta_txn(&txn, key),
    )?;
    if !CONFIRMED {
        for hash in plan.blocks.hashes() {
            let hash = hash.map_err(invalid)?;
            let block = plan
                .blocks
                .get(&hash)
                .map_err(invalid)?
                .ok_or_else(|| invalid("staged tail block missing"))?;
            if block.block_type == burst_ledger::BlockType::Epoch {
                crate::payment_transition::authenticate_epoch_contents(&block, &params, &genesis)
                    .map_err(invalid)?;
            } else {
                crate::payment_transition::authenticate_signed_contents(&block, &params)
                    .map_err(invalid)?;
            }
        }
    }
    for head in &heads {
        let mut hash = head.hash;
        let mut height = head.height;
        while let Some(block) = plan.blocks.get(&hash).map_err(invalid)? {
            if blocks
                .block_at_height_txn(&txn, &head.account, height)
                .map_err(invalid)?
                != Some(hash)
            {
                return Err(invalid(
                    "cemented suffix has a missing or conflicting height index",
                ));
            }
            retain(&head.account, height, hash)?;
            hash = block.previous;
            height -= 1;
        }
    }
    let accounts = AccountInventory::read(account_store.account_entries(&txn).map_err(invalid)?)
        .map_err(invalid)?;
    Ok(IndexedTailPlan {
        plan,
        accounts,
        rows,
    })
}
