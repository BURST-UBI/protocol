use super::*;
use crate::confirmed_replay::scratch_records::ScratchRecords;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize)]
struct Head {
    account: WalletAddress,
    hash: BlockHash,
    height: u64,
}

/// Private root indexes populated from authenticated cursors. There is no
/// deserializer or reopen API: disk rows require the in-memory index roots and
/// checked context. Discard the collection if insertion fails between index writes.
pub(crate) struct StoredHistoryRoots {
    context: HistoryContext,
    cutoff: u64,
    accounts: ScratchRecords<Head>,
    heads: ScratchRecords<WalletAddress>,
    count: usize,
}

impl StoredHistoryRoots {
    pub(in crate::confirmed_replay::snapshot) fn new(
        directory: &Path,
        context: HistoryContext,
        cutoff: u64,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            context,
            cutoff,
            accounts: ScratchRecords::new(directory)?,
            heads: ScratchRecords::new(directory)?,
            count: 0,
        })
    }

    fn key(account: &WalletAddress) -> [u8; 32] {
        burst_crypto::blake2b_256(account.as_str().as_bytes())
    }

    pub(in crate::confirmed_replay::snapshot) fn insert(
        &mut self,
        cursor: HistoryCursor,
    ) -> Result<(), StoreError> {
        let error = || StoreError::Corruption("invalid stored history root".into());
        if cursor.context != self.context || cursor.cutoff != self.cutoff {
            return Err(error());
        }
        let head = cursor.next().ok_or_else(error)?;
        let key = Self::key(&head.account);
        if self.accounts.get(&key)?.is_some() || self.heads.get(head.hash.as_bytes())?.is_some() {
            return Err(error());
        }
        self.heads.put(*head.hash.as_bytes(), &head.account)?;
        self.accounts.put(
            key,
            &Head {
                account: head.account,
                hash: head.hash,
                height: head.height,
            },
        )?;
        self.count += 1;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn truncate_for_test(&self) {
        self.accounts.truncate_for_test();
    }

    pub(crate) fn context(&self) -> HistoryContext {
        self.context
    }
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn cursor(
        &self,
        account: &WalletAddress,
    ) -> Result<Option<HistoryCursor>, StoreError> {
        let Some(head) = self.accounts.get(&Self::key(account))? else {
            return Ok(None);
        };
        if head.account != *account || head.height == 0 || head.hash.is_zero() {
            return Err(StoreError::Corruption(
                "stored history account differs from its key".into(),
            ));
        }
        Ok(Some(HistoryCursor {
            context: self.context,
            cutoff: self.cutoff,
            account: head.account,
            next: head.hash,
            height: head.height,
        }))
    }

    pub(crate) fn account_for_head(
        &self,
        hash: BlockHash,
    ) -> Result<Option<WalletAddress>, StoreError> {
        let Some(account) = self.heads.get(hash.as_bytes())? else {
            return Ok(None);
        };
        if self
            .cursor(&account)?
            .and_then(|cursor| cursor.next())
            .is_none_or(|head| head.hash != hash)
        {
            return Err(StoreError::Corruption(
                "stored history head index differs".into(),
            ));
        }
        Ok(Some(account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_roots_bind_context_accounts_heads_and_reject_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let base = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let mut roots = base.stored_history_roots(directory.path()).unwrap();
        let genesis = base.anchor().genesis_hash();
        let account = roots.account_for_head(genesis).unwrap().unwrap();
        let cursor = roots.cursor(&account).unwrap().unwrap();
        assert_eq!(cursor.context(), base.history_context());
        assert_eq!(cursor.next().unwrap().hash, genesis);
        assert!(roots.insert(cursor.clone()).is_err());
        // Synthetic cursor positions exercise the private index, not finality.
        let mut expected = Vec::new();
        for number in 1u64..258 {
            let seed = burst_crypto::blake2b_256(&number.to_be_bytes());
            let key = burst_crypto::keypair_from_seed(&seed);
            let mut next = cursor.clone();
            next.account = burst_crypto::derive_address(&key.public);
            next.next = BlockHash::new(seed);
            next.height = number;
            expected.push(next.next().unwrap());
            roots.insert(next).unwrap();
        }
        assert_eq!(roots.len(), 258);
        for head in expected {
            assert_eq!(
                roots
                    .cursor(&head.account)
                    .unwrap()
                    .unwrap()
                    .next()
                    .unwrap(),
                head
            );
            assert_eq!(
                roots.account_for_head(head.hash).unwrap().unwrap(),
                head.account
            );
        }
        assert!(roots.account_for_head(BlockHash::ZERO).unwrap().is_none());
        let unknown =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[99; 32]).public);
        assert!(roots.cursor(&unknown).unwrap().is_none());
        let mut foreign = cursor.clone();
        foreign.context.state_hash = BlockHash::ZERO;
        assert!(roots.insert(foreign).is_err());
        let mut exhausted = cursor.clone();
        exhausted.height = 0;
        assert!(roots.insert(exhausted).is_err());
        let mut changed_cutoff = cursor;
        changed_cutoff.cutoff += 1;
        assert!(roots.insert(changed_cutoff).is_err());
        roots.accounts.truncate_for_test();
        assert!(roots.cursor(&account).is_err());
        assert!(roots.cursor(&unknown).is_err());
        assert!(roots.account_for_head(genesis).is_err());
        let roots = base.stored_history_roots(directory.path()).unwrap();
        roots.heads.truncate_for_test();
        assert!(roots.account_for_head(genesis).is_err());
        assert!(roots.account_for_head(BlockHash::ZERO).is_err());
    }
}
