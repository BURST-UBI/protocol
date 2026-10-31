use super::*;
use std::ops::Bound;

fn position(key: &[u8]) -> Result<(WalletAddress, u64), StoreError> {
    let split = key
        .len()
        .checked_sub(8)
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("invalid local height key"))?;
    let account = WalletAddress::new(
        std::str::from_utf8(&key[..split]).map_err(|_| invalid("invalid local height account"))?,
    );
    Ok((
        account,
        u64::from_be_bytes(key[split..].try_into().unwrap()),
    ))
}

/// Physical block inventory and logical history after staging a compact projection.
pub struct CompactProjection {
    /// Bodies moved from the block table to the orphan archive, sorted by hash.
    pub retired_blocks: Vec<BlockHash>,
    /// Bodies remaining in the block table, including existing cemented bodies.
    pub retained_blocks: u64,
    /// Sum of projected account heights, including history without local bodies.
    pub ledger_blocks: u64,
}

enum AccountTargets<'a> {
    Indexed(&'a HashMap<WalletAddress, AccountInfo>),
    Slice(HashMap<&'a WalletAddress, &'a AccountInfo>),
}

impl AccountTargets<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Indexed(accounts) => accounts.len(),
            Self::Slice(accounts) => accounts.len(),
        }
    }

    fn get(&self, address: &WalletAddress) -> Option<&AccountInfo> {
        match self {
            Self::Indexed(accounts) => accounts.get(address),
            Self::Slice(accounts) => accounts.get(address).copied(),
        }
    }

    fn entries(&self) -> Box<dyn Iterator<Item = (&WalletAddress, &AccountInfo)> + '_> {
        match self {
            Self::Indexed(accounts) => Box::new(accounts.iter()),
            Self::Slice(accounts) => Box::new(
                accounts
                    .iter()
                    .map(|(address, account)| (*address, *account)),
            ),
        }
    }

    fn values(&self) -> impl Iterator<Item = &AccountInfo> {
        self.entries().map(|(_, account)| account)
    }
}

impl LmdbWriteTransaction<'_> {
    /// Install caller-authenticated accounts and sparse block membership witnesses.
    /// The node must reconcile authority and local finality using this writer, then
    /// install economics, the authenticated base and epoch in the same transaction.
    /// Existing cemented bodies stay; unproven provisional bodies move to the orphan
    /// archive. Missing intermediate heights remain absent. Any error prevents commit.
    ///
    /// Accounts, supplied blocks and archived blocks each have a 131,072-row limit.
    /// Supplied and archived bodies each have a separate 64 MiB limit. Existing
    /// physical indexes and bodies are scanned in full, regardless of those limits.
    pub fn replace_compact_checkpoint_ledger(
        &mut self,
        expected_epoch: BlockHash,
        blocks: &[CheckpointBlock],
        accounts: &[AccountInfo],
    ) -> Result<CompactProjection, StoreError> {
        self.replace_compact_checkpoint_ledger_rows(
            expected_epoch,
            blocks
                .iter()
                .map(|block| Ok(std::borrow::Cow::Borrowed(block))),
            accounts,
        )
    }

    /// Consume caller-authenticated block rows once, retaining their positions and
    /// hashes in memory. Build an account lookup from the slice. A late source error
    /// prevents commit, including bodies already staged in this transaction.
    pub fn replace_compact_checkpoint_ledger_rows<'a>(
        &mut self,
        expected_epoch: BlockHash,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, CheckpointBlock>, StoreError>>,
        accounts: &[AccountInfo],
    ) -> Result<CompactProjection, StoreError> {
        let result = (|| {
            if accounts.len() > MAX_ROWS {
                return Err(invalid("invalid compact projection boundary"));
            }
            let mut targets = HashMap::new();
            for account in accounts {
                if targets.insert(&account.address, account).is_some() {
                    return Err(invalid("invalid compact checkpoint account"));
                }
            }
            self.replace_compact_checkpoint_ledger_inner(
                expected_epoch,
                blocks,
                AccountTargets::Slice(targets),
                None,
            )
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Borrow the replay owner's existing account index without cloning accounts
    /// or allocating a second index. Supplied authenticated BRN wallets are reconciled
    /// without rewriting identical rows; omission clears them for later publication.
    /// The same projection checks and budget apply.
    pub fn replace_compact_checkpoint_ledger_account_map<'a>(
        &mut self,
        expected_epoch: BlockHash,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, CheckpointBlock>, StoreError>>,
        accounts: &HashMap<WalletAddress, AccountInfo>,
        brn_wallets: Option<&burst_brn::wallet_states::WalletStates>,
    ) -> Result<CompactProjection, StoreError> {
        let result = self.replace_compact_checkpoint_ledger_inner(
            expected_epoch,
            blocks,
            AccountTargets::Indexed(accounts),
            brn_wallets,
        );
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn replace_compact_checkpoint_ledger_inner<'a>(
        &mut self,
        expected_epoch: BlockHash,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, CheckpointBlock>, StoreError>>,
        accounts: AccountTargets<'_>,
        brn_wallets: Option<&burst_brn::wallet_states::WalletStates>,
    ) -> Result<CompactProjection, StoreError> {
        self.require_clean_checkpoint_boundary()?;
        if self.payment_epoch()? != expected_epoch || accounts.len() > MAX_ROWS {
            return Err(invalid("invalid compact projection boundary"));
        }
        let mut ledger_blocks = 0u64;
        for (address, account) in accounts.entries() {
            if *address != account.address
                || account.confirmation_height != account.block_count
                || account.head.is_zero() != (account.block_count == 0)
            {
                return Err(invalid("invalid compact checkpoint account"));
            }
            ledger_blocks = ledger_blocks
                .checked_add(account.block_count)
                .ok_or_else(|| invalid("compact ledger count overflow"))?;
        }
        let mut positions = HashMap::new();
        let mut retained = HashMap::new();
        // Validate old positions before staging bodies, so supplied witnesses
        // cannot silently repair a missing cemented body or a damaged index.
        for row in self
            .env
            .height_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?
        {
            let (key, value) = row.map_err(LmdbError::from)?;
            let (account, height) = position(key)?;
            let hash = BlockHash::new(
                value
                    .try_into()
                    .map_err(|_| invalid("invalid local height hash"))?,
            );
            let local = self.get_account(&account)?;
            if height == 0
                || height > local.block_count
                || hash.is_zero()
                || self.block_height(&account, &hash)? != height
                || self
                    .env
                    .blocks_db
                    .get(&self.txn, hash.as_bytes())
                    .map_err(LmdbError::from)?
                    .is_none()
            {
                return Err(invalid("inconsistent local compact index"));
            }
        }
        for row in self
            .env
            .block_height_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?
        {
            let (key, value) = row.map_err(LmdbError::from)?;
            let hash = BlockHash::new(
                key.try_into()
                    .map_err(|_| invalid("invalid local reverse height hash"))?,
            );
            let (account, height) = position(value)?;
            if self.block_height(&account, &hash)? != height || height == 0 {
                return Err(invalid("inconsistent local reverse height index"));
            }
        }
        let mut byte_count = 0usize;
        let mut supplied = 0usize;
        for block in blocks {
            let block = block?;
            supplied += 1;
            if supplied > MAX_ROWS {
                return Err(invalid("invalid compact projection boundary"));
            }
            let account = accounts
                .get(&block.account)
                .ok_or_else(|| invalid("unowned compact checkpoint block"))?;
            byte_count = byte_count
                .checked_add(block.bytes.len())
                .filter(|size| *size <= MAX_BYTES)
                .ok_or_else(|| invalid("compact projection exceeds byte limit"))?;
            if block.hash.is_zero()
                || block.height == 0
                || block.height > account.block_count
                || block.bytes.is_empty()
                || positions
                    .insert((block.account.clone(), block.height), block.hash)
                    .is_some()
                || retained
                    .insert(block.hash, (block.account.clone(), block.height))
                    .is_some()
            {
                return Err(invalid("invalid compact checkpoint block index"));
            }
            match self.get_block(&block.hash) {
                Ok(existing) if existing != block.bytes => {
                    return Err(invalid("compact projection changes stored block bytes"));
                }
                Ok(_) | Err(StoreError::NotFound(_)) => (),
                Err(error) => return Err(error),
            }
            self.put_block(&block.hash, &block.bytes)?;
        }
        if supplied == 0 {
            return Err(invalid("invalid compact projection boundary"));
        }
        for account in accounts.values().filter(|a| a.block_count > 0) {
            if positions.get(&(account.address.clone(), account.block_count)) != Some(&account.head)
            {
                return Err(invalid(
                    "compact projection lacks an authenticated head body",
                ));
            }
        }
        for account in self.account_entries()? {
            let account = account?;
            if account.confirmation_height == 0 {
                continue;
            }
            let height = account.confirmation_height;
            let head = self
                .block_at_height(&account.address, height)?
                .ok_or_else(|| invalid("local cemented head is missing"))?;
            if self.block_height(&account.address, &head)? != height
                || positions.get(&(account.address.clone(), height)) != Some(&head)
                || (account.block_count == height && account.head != head)
            {
                return Err(invalid("compact projection would replace cemented history"));
            }
        }
        // Scan physical rows, not 1..account.block_count. The prefix can already
        // be sparse, and a large certified height does not authorize a large loop.
        for row in self
            .env
            .height_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?
        {
            let (key, value) = row.map_err(LmdbError::from)?;
            let (account, height) = position(key)?;
            let hash = BlockHash::new(
                value
                    .try_into()
                    .map_err(|_| invalid("invalid local height hash"))?,
            );
            let local = self.get_account(&account)?;
            if retained
                .get(&hash)
                .is_some_and(|p| *p != (account.clone(), height))
            {
                return Err(invalid("compact projection moves an indexed block"));
            }
            if height <= local.confirmation_height
                && positions
                    .get(&(account.clone(), height))
                    .is_some_and(|other| *other != hash)
            {
                return Err(invalid("compact witness conflicts with cemented index"));
            }
        }
        for (key, value) in self.checkpoint_metadata(crate::confirmation_queue::PREFIX)? {
            let hash = BlockHash::new(
                key[crate::confirmation_queue::PREFIX.len()..]
                    .try_into()
                    .map_err(|_| invalid("invalid queued confirmation hash"))?,
            );
            if hash.is_zero()
                || value != [1]
                || (!retained.contains_key(&hash) && !self.is_compact_cemented(&hash)?)
            {
                return Err(invalid(
                    "compact projection excludes durable confirmation work",
                ));
            }
        }
        for db in [
            self.env.transactions_db,
            self.env.account_txs_db,
            self.env.proposals_db,
            self.env.votes_db,
            self.env.delegations_db,
        ] {
            if db.len(&self.txn).map_err(LmdbError::from)? != 0 {
                return Err(invalid(
                    "compact projection requires unsupported table reconstruction",
                ));
            }
        }
        let mut retired_blocks = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        let mut retired_bytes = 0usize;
        loop {
            let bounds = (
                cursor.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
                Bound::Unbounded,
            );
            let keys = self
                .env
                .blocks_db
                .range(&self.txn, &bounds)
                .map_err(LmdbError::from)?
                .take(256)
                .map(|row| {
                    let (key, _) = row.map_err(LmdbError::from)?;
                    let hash = BlockHash::new(
                        key.try_into()
                            .map_err(|_| invalid("invalid stored compact block hash"))?,
                    );
                    Ok(hash)
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            if keys.is_empty() {
                break;
            }
            cursor = keys.last().map(|hash| hash.as_bytes().to_vec());
            for hash in keys {
                if retained.contains_key(&hash) || self.is_compact_cemented(&hash)? {
                    continue;
                }
                let value = self.get_block(&hash)?;
                retired_bytes = retired_bytes
                    .checked_add(value.len())
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or_else(|| invalid("compact retirement exceeds byte limit"))?;
                if retired_blocks.len() == MAX_ROWS {
                    return Err(invalid("compact retirement exceeds row limit"));
                }
                let archive = format!("checkpoint_orphan/v1/{expected_epoch}/{hash}");
                if self.get_meta(&archive)?.is_some_and(|old| old != value) {
                    return Err(invalid("compact orphan evidence changed"));
                }
                self.put_meta(&archive, &value)?;
                if let Some(key) = self
                    .env
                    .block_height_db
                    .get(&self.txn, hash.as_bytes())
                    .map_err(LmdbError::from)?
                    .map(<[u8]>::to_vec)
                {
                    self.delete_raw(UndoTable::Heights, &key)?;
                    self.delete_raw(UndoTable::BlockHeights, hash.as_bytes())?;
                }
                self.delete_raw(UndoTable::Blocks, hash.as_bytes())?;
                retired_blocks.push(hash);
            }
        }
        self.reconcile_compact_account_rows(&accounts)?;
        match brn_wallets {
            Some(wallets) => self.reconcile_checkpoint_brn_wallets(wallets)?,
            None => self.clear_checkpoint_rows(UndoTable::BrnWallets, None)?,
        }
        for table in [
            UndoTable::Pending,
            UndoTable::TrstOrigins,
            UndoTable::TrstExpiry,
            UndoTable::TrstReverse,
        ] {
            self.clear_checkpoint_rows(table, None)?;
        }
        self.clear_checkpoint_rows(UndoTable::Meta, Some(crate::block_undo::PREFIX))?;
        for (hash, (account, height)) in &retained {
            let bytes = self.get_block(hash)?;
            self.put_block_with_account(hash, &bytes, account, *height)?;
            if self.confirmation_pending(hash)? {
                self.acknowledge_confirmation(hash)?;
            }
        }
        // Old cemented indexes stay in place. Queued cemented work is resolved
        // by the authenticated replacement even if its body was not resupplied.
        for (key, _) in self.checkpoint_metadata(crate::confirmation_queue::PREFIX)? {
            let hash = BlockHash::new(
                key[crate::confirmation_queue::PREFIX.len()..]
                    .try_into()
                    .map_err(|_| invalid("invalid queued confirmation hash"))?,
            );
            self.acknowledge_confirmation(&hash)?;
        }
        let mut verified = 0u64;
        for account in accounts.values() {
            self.put_account(
                &account.address,
                &bincode::serialize(account).map_err(LmdbError::from)?,
            )?;
            if account.block_count > 0 {
                self.put_frontier(&account.address, &account.head)?;
            }
            verified += u64::from(account.state.is_verified());
        }
        self.put_meta("verified_count", &verified.to_be_bytes())?;
        retired_blocks.sort_by_key(|hash| *hash.as_bytes());
        Ok(CompactProjection {
            retired_blocks,
            retained_blocks: self.env.blocks_db.len(&self.txn).map_err(LmdbError::from)?,
            ledger_blocks,
        })
    }

    fn reconcile_compact_account_rows(&mut self, accounts: &AccountTargets<'_>) -> Result<(), StoreError> {
        // Rebuild the verified-account index and invalidate derived caches.
        // Later account writes skip byte-identical primary values.
        self.invalidate_body_changes();
        let cache = self.auxiliary_cache.get_mut();
        cache.invalidate_projection();
        cache.clear_electorate(UndoTable::Accounts);
        cache.clear_counter_accounts(UndoTable::Accounts);
        self.dirty = true;
        crate::verified_index::rebuild(
            &mut self.txn, self.env.accounts_db, self.env.verified_accounts_db,
        ).map_err(LmdbError::from)?;
        for table in [UndoTable::Accounts, UndoTable::Frontiers] {
            let db = table.database(self.env);
            let mut cursor: Option<Vec<u8>> = None;
            loop {
                let bounds = (cursor.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
                    Bound::<&[u8]>::Unbounded);
                let rows = db.range(&self.txn, &bounds).map_err(LmdbError::from)?
                    .take(256).map(|row| {
                        let (key, _) = row.map_err(LmdbError::from)?;
                        let keep = std::str::from_utf8(key).ok()
                            .filter(|address| address.starts_with(WalletAddress::PREFIX))
                            .and_then(|address| accounts.get(&WalletAddress::new(address)))
                            .is_some_and(|account| table == UndoTable::Accounts || account.block_count > 0);
                        Ok((key.to_vec(), keep))
                    }).collect::<Result<Vec<_>, StoreError>>()?;
                if rows.is_empty() { break; }
                cursor = rows.last().map(|(key, _)| key.clone());
                for (key, keep) in rows {
                    if !keep { self.delete_raw(table, &key)?; }
                }
            }
        }
        Ok(())
    }

    fn is_compact_cemented(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        let Some(key) = self
            .env
            .block_height_db
            .get(&self.txn, hash.as_bytes())
            .map_err(LmdbError::from)?
        else {
            return Ok(false);
        };
        let (account, height) = position(key)?;
        if self.block_height(&account, hash)? != height || height == 0 {
            return Err(invalid("inconsistent local compact index"));
        }
        Ok(height <= self.get_account(&account)?.confirmation_height)
    }
}

#[cfg(test)]
mod tests;
