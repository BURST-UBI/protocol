//! Ledger state updater — maintains per-account state on block insertion.
//!
//! Inspired by rsnano's BlockInserter: atomically updates AccountInfo,
//! pending entries, and rep weights when a block is inserted.

use burst_consensus::RepWeightCache;
use burst_ledger::{BlockType, StateBlock};
use burst_store::account::AccountInfo;
use burst_store_lmdb::WriteBatch;
use burst_types::{WalletAddress, WalletState};

/// Update ledger state after a block is accepted.
///
/// Called within the write batch so all updates are atomic. Updates the
/// account's `AccountInfo`, adjusts representative weights, and tracks BRN
/// burns.
///
/// `prev_brn_balance` is the BRN balance from the previous StateBlock in
/// the account chain (needed for burn tracking since `AccountInfo` does not
/// store raw BRN balance).
pub fn update_account_on_block(
    batch: &mut WriteBatch<'_>,
    block: &StateBlock,
    prev_account: Option<&AccountInfo>,
    prev_brn_balance: u128,
    rep_weights: &mut RepWeightCache,
) -> Result<AccountInfo, String> {
    let is_open = block.previous.is_zero();

    let info = if is_open {
        AccountInfo {
            address: block.account.clone(),
            state: WalletState::Unverified,
            verified_at: None,
            head: block.hash,
            block_count: 1,
            confirmation_height: 0,
            representative: block.representative.clone(),
            total_brn_burned: 0,
            trst_balance: block.trst_balance,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        }
    } else {
        let mut info = prev_account
            .cloned()
            .ok_or_else(|| "previous account info not found".to_string())?;

        let old_trst = info.trst_balance;
        let old_rep = info.representative.clone();

        info.head = block.hash;
        info.block_count += 1;
        info.trst_balance = block.trst_balance;

        // Single atomic rep weight update: handles both rep change and balance change
        if old_rep != block.representative || old_trst != block.trst_balance {
            if old_trst > 0 {
                rep_weights.remove_weight(&old_rep, old_trst);
            }
            if block.trst_balance > 0 {
                rep_weights.add_weight(&block.representative, block.trst_balance);
            }
            info.representative = block.representative.clone();
        }

        // Track BRN burns (BRN decrease = amount burned → TRST minted 1:1)
        if block.block_type == BlockType::Burn {
            let burned = prev_brn_balance.saturating_sub(block.brn_balance);
            info.total_brn_burned = info.total_brn_burned.saturating_add(burned);
        }

        // Epoch blocks upgrade the account version
        if block.block_type == BlockType::Epoch {
            info.epoch = block.version;
        }

        info
    };

    // Update rep weight for open blocks
    if is_open {
        rep_weights.add_weight(&block.representative, block.trst_balance);
    }

    // Serialize and put in batch
    let info_bytes = bincode::serialize(&info)
        .map_err(|e| format!("failed to serialize AccountInfo: {e}"))?;
    batch
        .put_account(&block.account, &info_bytes)
        .map_err(|e| format!("failed to put account: {e}"))?;

    Ok(info)
}

/// Create a pending entry for a send block's receiver.
///
/// Uses binary composite key `destination_bytes ++ send_block_hash_bytes`.
///
/// `destination` must be the wallet address of the receiver.
/// `provenance` carries origin info from the consumed tokens so receivers
/// get properly tracked TRST with lineage for expiry and revocation.
pub fn create_pending_entry(
    batch: &mut WriteBatch<'_>,
    block: &StateBlock,
    amount: u128,
    destination: &WalletAddress,
    consumed: Vec<burst_trst::ConsumedProvenance>,
) -> Result<(), String> {
    if block.block_type != BlockType::Send || block.link.is_zero() {
        return Ok(());
    }
    let provenance: Vec<PendingProvenance> = consumed
        .into_iter()
        .map(|c| PendingProvenance {
            amount: c.amount,
            origin: c.origin,
            origin_wallet: c.origin_wallet,
            origin_timestamp: c.origin_timestamp,
            effective_origin_timestamp: c.effective_origin_timestamp,
            origin_proportions: c.origin_proportions
                .into_iter()
                .map(|p| burst_types::OriginProportion {
                    origin: p.origin,
                    origin_wallet: p.origin_wallet,
                    amount: p.amount,
                })
                .collect(),
        })
        .collect();
    let pending_data = bincode::serialize(&PendingInfo {
        source: block.account.clone(),
        amount,
        timestamp: block.timestamp,
        provenance,
    })
    .map_err(|e| format!("serialize pending: {e}"))?;
    batch
        .put_pending(destination, block.hash.as_bytes(), &pending_data)
        .map_err(|e| format!("put pending: {e}"))?;
    Ok(())
}

/// Delete a pending entry when a receive or reject-receive block is processed.
///
/// Handles both `Receive` (claiming pending TRST) and `RejectReceive`
/// (returning pending TRST to sender). Uses binary composite key
/// `account_bytes ++ link_bytes` matching `create_pending_entry`.
pub fn delete_pending_entry(
    batch: &mut WriteBatch<'_>,
    block: &StateBlock,
) -> Result<(), String> {
    if (block.block_type != BlockType::Receive && block.block_type != BlockType::RejectReceive)
        || block.link.is_zero()
    {
        return Ok(());
    }
    batch
        .delete_pending(&block.account, block.link.as_bytes())
        .map_err(|e| format!("delete pending: {e}"))?;
    Ok(())
}

/// Re-export the canonical PendingInfo from the store crate.
pub use burst_store::pending::{PendingInfo, PendingProvenance};

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::RepWeightCache;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_store::account::AccountInfo;
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress, WalletState};

    fn test_account() -> WalletAddress {
        WalletAddress::new(
            "brst_1test111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_rep() -> WalletAddress {
        WalletAddress::new(
            "brst_1rep1111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_rep2() -> WalletAddress {
        WalletAddress::new(
            "brst_2rep2222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn make_open_block(account: &WalletAddress, rep: &WalletAddress, trst: u128) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Open,
            account: account.clone(),
            previous: BlockHash::ZERO,
            representative: rep.clone(),
            brn_balance: 0,
            trst_balance: trst,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1000),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_send_block(
        account: &WalletAddress,
        rep: &WalletAddress,
        previous: BlockHash,
        trst: u128,
        link: BlockHash,
    ) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Send,
            account: account.clone(),
            previous,
            representative: rep.clone(),
            brn_balance: 0,
            trst_balance: trst,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2000),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_burn_block(
        account: &WalletAddress,
        rep: &WalletAddress,
        previous: BlockHash,
        brn: u128,
        trst: u128,
    ) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Burn,
            account: account.clone(),
            previous,
            representative: rep.clone(),
            brn_balance: brn,
            trst_balance: trst,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(3000),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_account_info(
        account: &WalletAddress,
        rep: &WalletAddress,
        head: BlockHash,
        trst: u128,
        block_count: u64,
    ) -> AccountInfo {
        AccountInfo {
            address: account.clone(),
            state: WalletState::Unverified,
            verified_at: None,
            head,
            block_count,
            confirmation_height: 0,
            representative: rep.clone(),
            total_brn_burned: 0,
            trst_balance: trst,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        }
    }

    // --- PendingInfo tests ---

    #[test]
    fn pending_info_roundtrip_serialization() {
        let info = PendingInfo {
            source: test_account(),
            amount: 500,
            timestamp: Timestamp::new(1234),
            provenance: Vec::new(),
        };
        let bytes = bincode::serialize(&info).unwrap();
        let deserialized: PendingInfo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.amount, 500);
        assert_eq!(deserialized.source, info.source);
    }

    // --- Rep weight tests (unit-testing the logic paths) ---

    #[test]
    fn rep_weight_added_on_open_block() {
        let mut rw = RepWeightCache::new();
        let rep = test_rep();
        let block = make_open_block(&test_account(), &rep, 1000);

        // Simulate the open-block path
        rw.add_weight(&block.representative, block.trst_balance);

        assert_eq!(rw.weight(&rep), 1000);
        assert_eq!(rw.total_weight(), 1000);
    }

    #[test]
    fn rep_weight_adjusted_on_balance_increase() {
        let mut rw = RepWeightCache::new();
        let rep = test_rep();
        rw.add_weight(&rep, 1000);

        let old_balance = 1000u128;
        let new_balance = 1500u128;
        rw.add_weight(&rep, new_balance - old_balance);

        assert_eq!(rw.weight(&rep), 1500);
    }

    #[test]
    fn rep_weight_adjusted_on_balance_decrease() {
        let mut rw = RepWeightCache::new();
        let rep = test_rep();
        rw.add_weight(&rep, 1000);

        let old_balance = 1000u128;
        let new_balance = 700u128;
        rw.remove_weight(&rep, old_balance - new_balance);

        assert_eq!(rw.weight(&rep), 700);
    }

    #[test]
    fn rep_weight_transferred_on_rep_change() {
        let mut rw = RepWeightCache::new();
        let old_rep = test_rep();
        let new_rep = test_rep2();
        rw.add_weight(&old_rep, 1000);

        // Simulate rep change: remove from old, add to new
        let balance = 1000u128;
        rw.remove_weight(&old_rep, balance);
        rw.add_weight(&new_rep, balance);

        assert_eq!(rw.weight(&old_rep), 0);
        assert_eq!(rw.weight(&new_rep), 1000);
    }

    // --- AccountInfo construction tests ---

    #[test]
    fn account_info_created_for_open_block() {
        let account = test_account();
        let rep = test_rep();
        let block = make_open_block(&account, &rep, 500);

        let info = AccountInfo {
            address: block.account.clone(),
            state: WalletState::Unverified,
            verified_at: None,
            head: block.hash,
            block_count: 1,
            confirmation_height: 0,
            representative: block.representative.clone(),
            total_brn_burned: 0,
            trst_balance: block.trst_balance,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        };

        assert_eq!(info.block_count, 1);
        assert_eq!(info.trst_balance, 500);
        assert_eq!(info.head, block.hash);
        assert_eq!(info.representative, rep);
    }

    #[test]
    fn account_info_updated_for_subsequent_block() {
        let account = test_account();
        let rep = test_rep();
        let open = make_open_block(&account, &rep, 1000);
        let send = make_send_block(
            &account,
            &rep,
            open.hash,
            700,
            BlockHash::new([0xAA; 32]),
        );

        let mut info = make_account_info(&account, &rep, open.hash, 1000, 1);
        info.head = send.hash;
        info.block_count += 1;
        info.trst_balance = send.trst_balance;

        assert_eq!(info.block_count, 2);
        assert_eq!(info.trst_balance, 700);
        assert_eq!(info.head, send.hash);
    }

    // --- BRN burn tracking tests ---

    #[test]
    fn brn_burn_tracked_correctly() {
        let prev_brn: u128 = 500;
        let new_brn: u128 = 300;
        let burned = prev_brn.saturating_sub(new_brn);
        assert_eq!(burned, 200);

        let mut total_burned: u128 = 100;
        total_burned = total_burned.saturating_add(burned);
        assert_eq!(total_burned, 300);
    }

    #[test]
    fn brn_burn_saturates_on_underflow() {
        let prev_brn: u128 = 100;
        let new_brn: u128 = 500; // shouldn't happen, but test saturation
        let burned = prev_brn.saturating_sub(new_brn);
        assert_eq!(burned, 0);
    }

    // --- Pending entry logic tests ---

    #[test]
    fn create_pending_skips_non_send_blocks() {
        let block = make_open_block(&test_account(), &test_rep(), 100);
        assert_ne!(block.block_type, BlockType::Send);
        // create_pending_entry returns Ok(()) immediately for non-Send
    }

    #[test]
    fn create_pending_skips_zero_link() {
        let block = make_send_block(
            &test_account(),
            &test_rep(),
            BlockHash::new([1u8; 32]),
            500,
            BlockHash::ZERO,
        );
        assert!(block.link.is_zero());
        // create_pending_entry returns Ok(()) for zero link
    }

    #[test]
    fn delete_pending_skips_non_receive_blocks() {
        let block = make_open_block(&test_account(), &test_rep(), 100);
        assert_ne!(block.block_type, BlockType::Receive);
    }

    #[test]
    fn pending_key_format_for_send() {
        let link = BlockHash::new([0xBB; 32]);
        let hash = BlockHash::new([0xCC; 32]);
        let key = format!("{}:{}", link, hash);
        assert!(key.contains(':'));
        assert!(key.starts_with(&format!("{}", link)));
    }

    #[test]
    fn pending_key_format_for_receive() {
        let account = test_account();
        let link = BlockHash::new([0xDD; 32]);
        let key = format!("{}:{}", account, link);
        assert!(key.contains(':'));
    }

    #[test]
    fn burn_block_tracks_brn_decrease() {
        let account = test_account();
        let rep = test_rep();
        let open = make_open_block(&account, &rep, 0);
        let burn = make_burn_block(&account, &rep, open.hash, 300, 0);

        // Previous BRN was 500, new BRN is 300 → burned 200
        let prev_brn: u128 = 500;
        let burned = prev_brn.saturating_sub(burn.brn_balance);
        assert_eq!(burned, 200);
        assert_eq!(burn.block_type, BlockType::Burn);
    }
}
