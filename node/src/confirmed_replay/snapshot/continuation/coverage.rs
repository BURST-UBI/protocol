use super::*;
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use std::collections::HashMap;

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl CompactSelectionCoverage {
    pub(crate) fn check_signing_parent(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        self.replay.validate_parent_commitments(anchor)?;
        self.cut.check_signing_parent(batch, anchor)
    }

    pub fn check_installed_cut(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        self.cut.check_installed_cut(batch)
    }

    pub(crate) fn seal_local_votes(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.replay.validate_parent_commitments(anchor)?;
        self.cut.seal_local_votes(batch, anchor, representative)
    }
}

impl CompactCutCoverage {
    pub(super) fn check_signing_parent(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        if self.parent != anchor.hash() || self.cutoff != anchor.cutoff() {
            return Err(invalid("compact coverage differs from its parent"));
        }
        self.check_installed_cut(batch)?;
        let key = format!(
            "checkpoint_chain/v1/{}/{}/head",
            anchor.network().as_str(),
            anchor.genesis_hash()
        );
        let head = batch.get_meta(&key)?;
        if head.as_deref() != Some(anchor.height().to_be_bytes().as_slice())
            && !(anchor.height() == 0 && head.is_none())
        {
            return Err(invalid("compact signing parent is not the accepted tip"));
        }
        Ok(())
    }

    pub(crate) fn seal_local_votes(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.check_signing_parent(batch, anchor)?;
        batch.seal_consensus_votes(representative, &self.parent, &self.parent, &self.included)
    }

    /// Read-only finality preflight in the caller's writer. Success is not a
    /// durable reservation: signing must repeat it alongside promise checks.
    pub fn check_installed_cut(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        use crate::confirmed_replay::installation::{CUTOFF_KEY, INSTALLED_KEY};
        if batch.payment_epoch()? != self.parent
            || batch.get_meta(INSTALLED_KEY)?.as_deref() != Some(self.authority.as_slice())
            || batch.get_meta(CUTOFF_KEY)?.as_deref() != Some(self.cutoff.to_be_bytes().as_slice())
            || batch
                .get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
                .as_deref()
                != Some(self.vote_limit.as_slice())
            || batch
                .get_meta(&burst_store::meta::closed_payment_epoch_key(&self.parent))?
                .is_some()
        {
            return Err(invalid(
                "compact candidate differs from installed authority",
            ));
        }
        for (address, floor) in &self.floors {
            if batch.get_account(address)?.confirmation_height < *floor {
                return Err(invalid(
                    "compact candidate base is not installed in account finality",
                ));
            }
        }
        for account in batch.account_entries()? {
            let account = account?;
            let height = account.confirmation_height;
            if height > account.block_count {
                return Err(invalid("invalid compact signing confirmation height"));
            }
            if height == 0 {
                continue;
            }
            let hash = batch
                .block_at_height(&account.address, height)?
                .ok_or_else(|| invalid("compact signing cemented head is missing"))?;
            if batch.block_height(&account.address, &hash)? != height
                || (height == account.block_count && hash != account.head)
                || self.positions.get(&(account.address, height)) != Some(&hash)
            {
                return Err(invalid(
                    "compact candidate does not preserve local finality",
                ));
            }
        }
        let prefix = b"pending_confirmation/v1/";
        for (key, value) in batch.checkpoint_metadata(prefix)? {
            let hash = BlockHash::new(key[prefix.len()..].try_into().map_err(invalid)?);
            if hash.is_zero() || value != [1] || !self.included.contains(&hash) {
                return Err(invalid(
                    "compact candidate omits a durable confirmation target",
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn positions(
    base: &SnapshotContinuation,
    tail: &SnapshotTail,
) -> Result<HashMap<(WalletAddress, u64), BlockHash>, ReplayError> {
    let invalid = |message: &str| super::super::invalid(message);
    let mut heads: HashMap<_, _> = base
        .state
        .accounts
        .values()
        .map(|account| (account.address.clone(), (account.block_count, account.head)))
        .collect();
    let mut result: HashMap<_, _> = heads
        .iter()
        .filter(|(_, (height, _))| *height > 0)
        .map(|(address, (height, hash))| ((address.clone(), *height), *hash))
        .collect();
    for witness in tail.retained().iter() {
        let witness = witness.map_err(|error| invalid(&error.to_string()))?;
        let key = (witness.block().account.clone(), witness.height());
        if result
            .insert(key, witness.block().hash)
            .is_some_and(|old| old != witness.block().hash)
        {
            return Err(invalid("conflicting compact membership positions"));
        }
    }
    for block in tail.blocks() {
        let block = block.map_err(|error| invalid(&error.to_string()))?;
        let head = heads
            .entry(block.account.clone())
            .or_insert((0, BlockHash::ZERO));
        if block.previous != head.1 {
            return Err(invalid("compact coverage suffix is not account ordered"));
        }
        head.0 = head
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("compact coverage height overflow"))?;
        head.1 = block.hash;
        if result
            .insert((block.account.clone(), head.0), block.hash)
            .is_some()
        {
            return Err(invalid("duplicate compact membership position"));
        }
    }
    if result.len() > 131_072 {
        return Err(invalid("compact membership positions exceed their bound"));
    }
    Ok(result)
}
