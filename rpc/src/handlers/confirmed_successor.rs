use crate::{error::RpcError, server::RpcState};
use burst_store::{block::BlockStore, StoreError};
use burst_types::{BlockHash, WalletAddress};

/// Check that the stored account position matches the hash and is within the
/// supplied confirmation height. This uses indexes without loading block bodies.
pub(super) fn confirmed(
    store: &dyn BlockStore,
    hash: &BlockHash,
    account: &WalletAddress,
    confirmed_height: u64,
) -> Result<bool, StoreError> {
    let Some(height) = store.height_of_block(hash)? else {
        return Ok(false);
    };
    Ok(height > 0
        && height <= confirmed_height
        && store.block_at_height(account, height)? == Some(*hash))
}

/// Return the next confirmed hash only when both positions exist in local indexes.
/// A missing sparse-history position returns `None`; no historical body is loaded.
pub(super) fn lookup(
    hash: &BlockHash,
    account: &WalletAddress,
    state: &RpcState,
) -> Result<Option<String>, RpcError> {
    let info = match state.account_store.get_account(account) {
        Ok(info) => info,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(at_position(
        state.block_store.as_ref(),
        hash,
        account,
        info.confirmation_height,
    )?
    .map(|hash| hash.to_string()))
}

fn at_position(
    store: &dyn BlockStore,
    hash: &BlockHash,
    account: &WalletAddress,
    confirmed_height: u64,
) -> Result<Option<BlockHash>, StoreError> {
    let Some(height) = store.height_of_block(hash)? else {
        return Ok(None);
    };
    let Some(next) = height.checked_add(1) else {
        return Ok(None);
    };
    if height == 0
        || next > confirmed_height
        || store.block_at_height(account, height)? != Some(*hash)
    {
        return Ok(None);
    }
    store.block_at_height(account, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Positions {
        anchor: BlockHash,
        successor: BlockHash,
        height: u64,
        canonical: bool,
    }
    impl BlockStore for Positions {
        fn height_of_block(&self, hash: &BlockHash) -> Result<Option<u64>, StoreError> {
            Ok((*hash == self.anchor).then_some(self.height))
        }
        fn block_at_height(
            &self,
            _: &WalletAddress,
            height: u64,
        ) -> Result<Option<BlockHash>, StoreError> {
            Ok(if height == self.height {
                self.canonical.then_some(self.anchor)
            } else if self.height.checked_add(1) == Some(height) {
                Some(self.successor)
            } else {
                None
            })
        }
        fn put_block(&self, _: &BlockHash, _: &[u8]) -> Result<(), StoreError> {
            unreachable!()
        }
        fn get_block(&self, _: &BlockHash) -> Result<Vec<u8>, StoreError> {
            unreachable!("lookup must not load historical bodies")
        }
        fn exists(&self, _: &BlockHash) -> Result<bool, StoreError> {
            unreachable!()
        }
        fn delete_block(&self, _: &BlockHash) -> Result<(), StoreError> {
            unreachable!()
        }
        fn get_account_blocks(&self, _: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> {
            unreachable!("lookup must not scan account history")
        }
        fn block_count(&self) -> Result<u64, StoreError> {
            unreachable!()
        }
    }

    #[test]
    fn confirmation_requires_the_canonical_account_position() {
        let mut store = Positions {
            anchor: BlockHash::new([1; 32]),
            successor: BlockHash::new([2; 32]),
            height: 1,
            canonical: true,
        };
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
        assert!(confirmed(&store, &store.anchor, &account, 1).unwrap());
        assert!(!confirmed(&store, &store.anchor, &account, 0).unwrap());
        assert!(!confirmed(&store, &BlockHash::new([9; 32]), &account, 1).unwrap());
        store.canonical = false;
        assert!(!confirmed(&store, &store.anchor, &account, 100).unwrap());
        store.canonical = true;
        store.height = 0;
        assert!(!confirmed(&store, &store.anchor, &account, 100).unwrap());
    }

    #[test]
    fn confirmed_successor_requires_canonical_anchor_and_confirmed_next_position() {
        let mut store = Positions {
            anchor: BlockHash::new([1; 32]),
            successor: BlockHash::new([2; 32]),
            height: 1_000_000,
            canonical: true,
        };
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
        assert_eq!(
            at_position(&store, &store.anchor, &account, 1_000_001).unwrap(),
            Some(store.successor)
        );
        assert_eq!(
            at_position(&store, &store.anchor, &account, 1_000_000).unwrap(),
            None
        );
        assert_eq!(
            at_position(&store, &BlockHash::new([9; 32]), &account, u64::MAX).unwrap(),
            None
        );
        store.canonical = false;
        assert_eq!(
            at_position(&store, &store.anchor, &account, u64::MAX).unwrap(),
            None
        );
        store.canonical = true;
        store.height = u64::MAX;
        assert_eq!(
            at_position(&store, &store.anchor, &account, u64::MAX).unwrap(),
            None
        );
        store.height = 0;
        assert_eq!(
            at_position(&store, &store.anchor, &account, u64::MAX).unwrap(),
            None
        );
    }
}
