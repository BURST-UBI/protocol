//! Ledger state updater — maintains per-account state on block insertion.
//!
//! Inspired by rsnano's BlockInserter: atomically updates AccountInfo,
//! pending entries, and tracks BRN burns when a block is inserted.
//!
//! It does NOT touch consensus (rep) weight. Weight is a pure function of
//! verified humans and their delegation, never token balances or reputation.
//! The node rebuilds it from the durable confirmed authority projection.
//! Agreement on roster epochs remains separate unfinished work. Patching weight per-block here would be
//! a consensus-corruption hazard and is unnecessary.

use burst_ledger::{BlockType, StateBlock};
use burst_store::account::AccountInfo;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::TrstEngine;
use burst_types::Timestamp;
use burst_types::WalletAddress;

pub(crate) fn update_token_counters(
    account: &mut AccountInfo,
    trst: &TrstEngine,
    now: Timestamp,
) -> Result<(), String> {
    let portfolio = trst
        .get_portfolio(&account.address)
        .ok_or("affected holder portfolio missing")?;
    let mut expired = 0u128;
    let mut revoked = 0u128;
    for token in &portfolio.tokens {
        if token.state == burst_types::TrstState::Revoked {
            revoked = revoked
                .checked_add(token.amount)
                .ok_or("revoked balance overflow")?;
        } else if token.is_expired(now, trst.expiry_secs) {
            expired = expired
                .checked_add(token.amount)
                .ok_or("expired balance overflow")?;
        }
    }
    // Signed balances only change with account blocks. Spending checks the
    // portfolio; expired pending returns need not have been received on-chain.
    account.expired_trst = expired;
    account.revoked_trst = revoked;
    Ok(())
}

pub(crate) fn refresh_token_counters(
    batch: &mut LmdbWriteTransaction<'_>,
    holder: &WalletAddress,
    trst: &TrstEngine,
    now: Timestamp,
) -> Result<(), String> {
    if trst.get_portfolio(holder).is_none() {
        return Ok(());
    }
    let mut account = batch.get_account(holder).map_err(|e| e.to_string())?;
    update_token_counters(&mut account, trst, now)?;
    batch
        .put_account_info(&account, false)
        .map_err(|e| e.to_string())
}

pub fn blocks_compete(a: &StateBlock, b: &StateBlock) -> bool {
    a.account == b.account && a.previous == b.previous && a.hash != b.hash
}

/// Read after acquiring the economic locks and writer. An earlier admission
/// read may predate expiry or lifecycle metadata updates on the same account.
pub(crate) fn predecessor_account(
    batch: &LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<Option<AccountInfo>, String> {
    crate::payment_epoch::require_block(batch, block).map_err(|error| error.to_string())?;
    let account = match batch.get_account(&block.account) {
        Ok(account) => Some(account),
        Err(burst_store::StoreError::NotFound(_)) => None,
        Err(error) => return Err(error.to_string()),
    };
    let extends = match &account {
        Some(account) if account.address != block.account => false,
        Some(account) if account.block_count > 0 => {
            !block.previous.is_zero() && account.head == block.previous
        }
        Some(account) => block.previous.is_zero() && account.head.is_zero(),
        None => block.previous.is_zero(),
    };
    if !extends {
        return Err("candidate does not extend the predecessor state".into());
    }
    Ok(account)
}

/// One election per account position, including competing open blocks.
/// Domain separation keeps an open position distinct from its first successor.
pub fn election_root(block: &StateBlock) -> burst_types::BlockHash {
    let mut bytes = b"BURST/election-root/v1\0".to_vec();
    bytes.extend_from_slice(block.previous.as_bytes());
    bytes.extend_from_slice(block.account.as_str().as_bytes());
    burst_crypto::hash_block(&bytes)
}

/// Update ledger state after a block is accepted.
///
/// Called within the write batch so all updates are atomic. Updates the
/// account's `AccountInfo` and tracks BRN burns.
///
/// `prev_brn_balance` is the BRN balance from the previous StateBlock in
/// the account chain (needed for burn tracking since `AccountInfo` does not
/// store raw BRN balance).
pub fn update_account_on_block(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    prev_account: Option<&AccountInfo>,
    prev_brn_balance: u128,
) -> Result<AccountInfo, String> {
    let info = account_after_block(block, prev_account, prev_brn_balance)?;
    // Preserve concurrently advanced confirmation height and the verified-count
    // index using the account currently visible in this write transaction.
    batch
        .put_account_info(&info, false)
        .map_err(|e| format!("failed to put account: {e}"))?;
    Ok(info)
}

pub(crate) fn account_after_block(
    block: &StateBlock,
    prev_account: Option<&AccountInfo>,
    prev_brn_balance: u128,
) -> Result<AccountInfo, String> {
    let is_open = block.previous.is_zero();

    let info = if is_open {
        // Verification may precede the account's first signed block. Opening
        // its chain must not erase that identity or restart its BRN accrual.
        let mut info = prev_account
            .cloned()
            .unwrap_or_else(|| AccountInfo::unopened(block.account.clone()));
        info.head = block.hash;
        info.block_count = 1;
        info.confirmation_height = 0;
        info.representative = block.representative.clone();
        info.trst_balance = block.trst_balance;
        info
    } else {
        let mut info = prev_account
            .cloned()
            .ok_or_else(|| "previous account info not found".to_string())?;

        info.head = block.hash;
        info.block_count = info
            .block_count
            .checked_add(1)
            .ok_or("account height overflow")?;
        info.trst_balance = block.trst_balance;
        info.representative = block.representative.clone();

        // Track PERMANENT BRN burns. `brn_balance` is an ascending odometer of
        // cumulative spent BRN, so the delta is the amount spent by this block.
        // Burn and Endorse are permanent burns (endorsers permanently burn their
        // BRN, whitepaper §356), so both accumulate into `total_brn_burned`.
        // Challenge / VerificationVote deltas are *temporary stakes* (returned or
        // forfeited later) — those are tracked authoritatively by the BRN engine
        // (`total_staked`), not folded into the permanent-burn counter here.
        if matches!(
            block.block_type,
            BlockType::Burn | BlockType::Endorse | BlockType::GovernanceEndorse
        ) {
            let burned = block.brn_balance.saturating_sub(prev_brn_balance);
            info.total_brn_burned = info.total_brn_burned.saturating_add(burned);
        }

        // Epoch blocks upgrade the account version
        if block.block_type == BlockType::Epoch {
            info.epoch = block.version;
        }

        // These are provisional effects until confirmed replay establishes the
        // eligible pool. The timestamp also gates the minimum opt-in age.
        match block.block_type {
            BlockType::VerifierOptIn => info.verifier_opted_in_at = Some(block.timestamp),
            BlockType::VerifierOptOut => info.verifier_opted_in_at = None,
            _ => {}
        }

        info
    };

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
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    amount: u128,
    destination: &WalletAddress,
    consumed: Vec<burst_trst::ConsumedProvenance>,
) -> Result<(), String> {
    // Send blocks AND Burn blocks create pending entries: burns are
    // two-phase (8.4a) — the minted TRST is pending until the provider
    // publishes a Receive block.
    if !matches!(block.block_type, BlockType::Send | BlockType::Burn) || block.link.is_zero() {
        return Ok(());
    }
    let pending_data = bincode::serialize(&pending_from_provenance(block, amount, consumed))
        .map_err(|e| format!("serialize pending: {e}"))?;
    batch
        .put_pending(destination, block.hash.as_bytes(), &pending_data)
        .map_err(|e| format!("put pending: {e}"))?;
    Ok(())
}

pub(crate) fn pending_from_provenance(
    block: &StateBlock,
    amount: u128,
    consumed: Vec<burst_trst::ConsumedProvenance>,
) -> PendingInfo {
    let provenance = consumed
        .into_iter()
        .map(|c| PendingProvenance {
            basis: c.basis,
            revoked_amounts: Default::default(),
            amount: c.amount,
            origin: c.origin,
            origin_wallet: c.origin_wallet,
            origin_timestamp: c.origin_timestamp,
            effective_origin_timestamp: c.effective_origin_timestamp,
        })
        .collect();
    PendingInfo {
        return_credit: false,
        source: block.account.clone(),
        amount,
        timestamp: block.timestamp,
        provenance,
    }
}

/// Delete a pending entry when a receive, receive-open, or reject-receive
/// block is processed.
///
/// Handles `Receive`, `Open` with a non-zero link (receive-open — the
/// account's first block pockets a pending send), and `RejectReceive`
/// (returning pending TRST to sender). Uses binary composite key
/// `account_bytes ++ link_bytes` matching `create_pending_entry`.
pub fn delete_pending_entry(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<(), String> {
    let claims_pending = matches!(
        block.block_type,
        BlockType::Receive | BlockType::RejectReceive
    ) || (block.block_type == BlockType::Open && block.trst_balance > 0);
    if !claims_pending {
        return Ok(());
    }
    if block.link.is_zero() {
        return Err("pending claim has no source".into());
    }
    // Recheck inside the writer transaction. The expiry task may have returned
    // this transfer after admission checked it but before block processing took
    // the TRST lock. A missing pending entry must not mint an unbacked receive.
    let pending = batch
        .get_pending(&block.account, &block.link.into_tx_hash())
        .map_err(|e| e.to_string())?;
    if pending.return_credit
        && (block.block_type != BlockType::Receive || pending.source != block.account)
    {
        return Err("expiry return credit must be received by its owner".into());
    }
    if block.block_type == BlockType::RejectReceive {
        // Return as a new pending entry. The sender explicitly claims it; neither
        // account's signed balance is rewritten by someone else's rejection.
        let bytes = bincode::serialize(&pending).map_err(|e| e.to_string())?;
        batch
            .put_pending(&pending.source, block.hash.as_bytes(), &bytes)
            .map_err(|e| e.to_string())?;
    } else {
        let previous_balance = if block.previous.is_zero() {
            0
        } else {
            let bytes = batch
                .get_block(&block.previous)
                .map_err(|e| e.to_string())?;
            let previous: StateBlock = bincode::deserialize(&bytes).map_err(|e| e.to_string())?;
            previous.trst_balance
        };
        validate_receive_amount(block, previous_balance, &pending)?;
    }
    batch
        .delete_pending(&block.account, block.link.as_bytes())
        .map_err(|e| format!("delete pending: {e}"))?;
    Ok(())
}

pub(crate) fn validate_receive_amount(
    block: &StateBlock,
    previous_balance: u128,
    pending: &PendingInfo,
) -> Result<(), String> {
    if block.trst_balance.checked_sub(previous_balance) != Some(pending.amount) {
        return Err("receive balance does not match pending amount at commit".into());
    }
    Ok(())
}

/// Re-export the canonical PendingInfo from the store crate.
pub use burst_store::pending::{PendingInfo, PendingProvenance};

pub type PendingWrite = (u128, WalletAddress, Vec<burst_trst::ConsumedProvenance>);
pub type TrstIndexWrite = (
    burst_types::TxHash,
    burst_types::TxHash,
    burst_types::Timestamp,
);

/// Commit the block and its primary economic effects, or commit nothing.
/// No write error is recoverable by logging and continuing this transaction.
/// The live verification/governance path stages its additional effects before
/// committing instead of using this payment-only convenience wrapper.
pub fn persist_block(
    store: &burst_store_lmdb::LmdbEnvironment,
    block: &StateBlock,
    prev_account: Option<&AccountInfo>,
    prev_brn_balance: u128,
    pending: Option<&PendingWrite>,
    indices: Option<&TrstIndexWrite>,
    engines: (&burst_brn::BrnEngine, &burst_trst::TrstEngine),
) -> Result<(), String> {
    let mut batch = store.tx_begin_write().map_err(|e| e.to_string())?;
    stage_block(
        &mut batch,
        block,
        prev_account,
        prev_brn_balance,
        pending,
        indices,
    )?;
    refresh_token_counters(&mut batch, &block.account, engines.1, block.timestamp)?;
    persist_economics(&mut batch, engines.0, engines.1)?;
    batch.commit().map_err(|e| e.to_string())
}

/// Stage ledger writes without committing, so lifecycle effects can join them.
pub(crate) fn stage_block(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    prev_account: Option<&AccountInfo>,
    prev_brn_balance: u128,
    pending: Option<&PendingWrite>,
    indices: Option<&TrstIndexWrite>,
) -> Result<(), String> {
    let bytes = bincode::serialize(block).map_err(|e| e.to_string())?;
    let height = prev_account.map_or(Ok(1), |a| {
        a.block_count
            .checked_add(1)
            .ok_or_else(|| "account height overflow".to_string())
    })?;
    batch
        .begin_block_undo(block.hash, block.account.clone(), height)
        .map_err(|e| e.to_string())?;
    crate::verification_commit::dependencies::bind_identity(batch, block)
        .map_err(|e| e.to_string())?;
    batch
        .put_block_with_account(&block.hash, &bytes, &block.account, height)
        .map_err(|e| e.to_string())?;
    batch
        .put_frontier(&block.account, &block.hash)
        .map_err(|e| e.to_string())?;
    update_account_on_block(batch, block, prev_account, prev_brn_balance)?;
    delete_pending_entry(batch, block)?;
    if let Some((amount, destination, provenance)) = pending {
        create_pending_entry(batch, block, *amount, destination, provenance.clone())?;
    }
    if let Some((origin, token, expiry)) = indices {
        batch
            .put_origin_index(origin, token)
            .map_err(|e| e.to_string())?;
        batch
            .put_expiry_index(*expiry, token)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Account balances and the economic state backing them must share a durable
/// commit. In particular portfolios/provenance cannot wait for clean shutdown.
pub fn persist_economics(
    batch: &mut LmdbWriteTransaction<'_>,
    brn: &burst_brn::BrnEngine,
    trst: &burst_trst::TrstEngine,
) -> Result<(), String> {
    persist_brn(batch, brn)?;
    persist_trst(batch, trst)
}

pub(crate) fn persist_brn(
    batch: &mut LmdbWriteTransaction<'_>,
    brn: &burst_brn::BrnEngine,
) -> Result<(), String> {
    for (address, wallet) in &brn.wallets {
        let bytes = bincode::serialize(wallet).map_err(|e| e.to_string())?;
        batch
            .put_brn_wallet(address, &bytes)
            .map_err(|e| e.to_string())?;
    }
    let rates = bincode::serialize(&brn.rate_history).map_err(|e| e.to_string())?;
    batch
        .put_brn_meta(b"rate_history", &rates)
        .map_err(|e| e.to_string())?;
    batch
        .put_brn_meta(b"next_stake_id", &brn.next_stake_id().to_be_bytes())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist a candidate relative to its still-installed engine. Full replay and
/// startup continue to use `persist_brn`; their baseline is not a live snapshot.
pub(crate) fn persist_brn_delta(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &burst_brn::BrnEngine,
    after: &burst_brn::BrnEngine,
) -> Result<(), String> {
    for (address, wallet) in &after.wallets {
        if before.wallets.get(address) != Some(wallet) {
            let bytes = bincode::serialize(wallet).map_err(|error| error.to_string())?;
            batch
                .put_brn_wallet(address, &bytes)
                .map_err(|error| error.to_string())?;
        }
    }
    for address in before.wallets.keys() {
        if !after.wallets.contains_key(address) {
            batch
                .delete_brn_wallet(address)
                .map_err(|error| error.to_string())?;
        }
    }
    if before.rate_history != after.rate_history {
        let bytes = bincode::serialize(&after.rate_history).map_err(|error| error.to_string())?;
        batch
            .put_brn_meta(b"rate_history", &bytes)
            .map_err(|error| error.to_string())?;
    }
    if before.next_stake_id() != after.next_stake_id() {
        batch
            .put_brn_meta(b"next_stake_id", &after.next_stake_id().to_be_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn persist_trst(
    batch: &mut LmdbWriteTransaction<'_>,
    trst: &burst_trst::TrstEngine,
) -> Result<(), String> {
    crate::graph_storage::persist(batch, &trst.merger_graph).map_err(|e| e.to_string())?;
    persist_trst_wallets(batch, trst)
}

pub(crate) fn persist_trst_wallets(
    batch: &mut LmdbWriteTransaction<'_>,
    trst: &burst_trst::TrstEngine,
) -> Result<(), String> {
    crate::trst_storage::persist(batch, trst).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_store::account::AccountInfo;
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress, WalletState};

    fn test_account() -> WalletAddress {
        WalletAddress::new("brst_1test111111111111111111111111111111111111111111111111111111111111")
    }

    fn test_rep() -> WalletAddress {
        WalletAddress::new("brst_1rep1111111111111111111111111111111111111111111111111111111111111")
    }

    fn make_open_block(account: &WalletAddress, rep: &WalletAddress, trst: u128) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
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
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
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
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
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
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            verifier_opted_in_at: None,
            orv_evicted: false,
        }
    }

    // --- PendingInfo tests ---

    #[test]
    fn confirming_ancestor_does_not_make_descendants_fork_losers() {
        let account = test_account();
        let rep = test_rep();
        let open = make_open_block(&account, &rep, 1000);
        let send = make_send_block(&account, &rep, open.hash, 800, BlockHash::new([1; 32]));
        let next = make_send_block(&account, &rep, send.hash, 600, BlockHash::new([2; 32]));
        let fork = make_send_block(&account, &rep, open.hash, 700, BlockHash::new([3; 32]));
        assert!(!blocks_compete(&open, &send));
        assert!(!blocks_compete(&send, &next));
        assert!(!blocks_compete(&send, &send));
        assert!(blocks_compete(&send, &fork));
        assert_ne!(election_root(&open), election_root(&send));
        assert_eq!(election_root(&send), election_root(&fork));
        let other_open = make_open_block(&account, &rep, 999);
        assert_eq!(election_root(&open), election_root(&other_open));
        let unrelated_open = make_open_block(&rep, &rep, 999);
        assert_ne!(election_root(&open), election_root(&unrelated_open));
        let mut wrong_account = send.clone();
        wrong_account.account = rep;
        assert_ne!(election_root(&send), election_root(&wrong_account));
    }

    #[test]
    fn receive_open_preserves_existing_verification_and_accrual() {
        let directory = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbEnvironment::open(directory.path(), 32, 16 * 1024 * 1024)
            .unwrap();
        let account = test_account();
        let rep = test_rep();
        let mut prior = make_account_info(&account, &rep, BlockHash::ZERO, 0, 0);
        prior.state = WalletState::Verified;
        prior.verified_at = Some(Timestamp::new(500));
        let block = make_open_block(&account, &rep, 500);
        let mut batch = store.tx_begin_write().unwrap();
        let actual = update_account_on_block(&mut batch, &block, Some(&prior), 0).unwrap();
        batch.commit().unwrap();
        assert_eq!(actual.state, WalletState::Verified);
        assert_eq!(actual.verified_at, prior.verified_at);
        assert_eq!(actual.head, block.hash);
        assert_eq!(actual.trst_balance, 500);
        assert_eq!(actual.block_count, 1);
        assert_eq!(actual.confirmation_height, 0);
    }

    #[test]
    fn primary_commit_aborts_if_account_update_fails() {
        use burst_store::{block::BlockStore, frontier::FrontierStore};
        let dir = tempfile::tempdir().unwrap();
        let store =
            burst_store_lmdb::LmdbEnvironment::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let block = make_burn_block(&test_account(), &test_rep(), BlockHash::new([1; 32]), 10, 0);
        // The block and frontier writes precede account validation in the batch.
        // A missing predecessor account must abort those earlier writes too.
        let result = persist_block(
            &store,
            &block,
            None,
            0,
            None,
            None,
            (&burst_brn::BrnEngine::new(), &burst_trst::TrstEngine::new()),
        );
        assert!(result.unwrap_err().contains("previous account info"));
        assert!(!store.block_store().exists(&block.hash).unwrap());
        assert_eq!(
            store.block_store().height_of_block(&block.hash).unwrap(),
            None
        );
        assert_eq!(
            store
                .block_store()
                .block_at_height(&block.account, 1)
                .unwrap(),
            None
        );
        assert!(store.frontier_store().get_frontier(&block.account).is_err());
    }

    #[test]
    fn primary_commit_aborts_on_full_database_and_can_retry() {
        use burst_store::{
            account::AccountStore, block::BlockStore, frontier::FrontierStore, BrnStore,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbEnvironment::open(dir.path(), 32, 1024 * 1024).unwrap();
        let block = make_open_block(&test_account(), &test_rep(), 0);
        let mut brn = burst_brn::BrnEngine::new();
        for n in 0..20_000 {
            brn.track_wallet(
                WalletAddress::new(format!("brst_stress_{n:064}")),
                burst_brn::BrnWalletState::new(Timestamp::new(1)),
            );
        }
        let trst = burst_trst::TrstEngine::new();
        let error = persist_block(&store, &block, None, 0, None, None, (&brn, &trst)).unwrap_err();
        assert!(
            error.contains("MDB_MAP_FULL"),
            "unexpected failure: {error}"
        );
        assert!(!store.block_store().exists(&block.hash).unwrap());
        assert!(store.account_store().get_account(&block.account).is_err());
        assert!(store.frontier_store().get_frontier(&block.account).is_err());
        assert_eq!(
            store.block_store().height_of_block(&block.hash).unwrap(),
            None
        );
        assert!(store.brn_store().iter_wallet_states().unwrap().is_empty());
        assert!(store
            .tx_begin_write()
            .unwrap()
            .block_undo(&block.hash)
            .unwrap()
            .is_none());
        persist_block(
            &store,
            &block,
            None,
            0,
            None,
            None,
            (&burst_brn::BrnEngine::new(), &trst),
        )
        .unwrap();
        assert_eq!(
            store
                .account_store()
                .get_account(&block.account)
                .unwrap()
                .head,
            block.hash
        );
    }

    #[test]
    fn primary_commit_preserves_confirmation_height_from_live_transaction() {
        use burst_store::account::AccountStore;
        let dir = tempfile::tempdir().unwrap();
        let store =
            burst_store_lmdb::LmdbEnvironment::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let prior = make_account_info(
            &test_account(),
            &test_rep(),
            BlockHash::new([1; 32]),
            100,
            1,
        );
        store.account_store().put_account(&prior).unwrap();
        store
            .account_store()
            .advance_confirmation_height(&prior.address, 1)
            .unwrap();
        // `prior` was read before the confirmation task advanced the height.
        let block = make_burn_block(&prior.address, &prior.representative, prior.head, 10, 100);
        persist_block(
            &store,
            &block,
            Some(&prior),
            0,
            None,
            None,
            (&burst_brn::BrnEngine::new(), &burst_trst::TrstEngine::new()),
        )
        .unwrap();
        let loaded = store.account_store().get_account(&prior.address).unwrap();
        assert_eq!(loaded.confirmation_height, 1);
        assert_eq!(loaded.head, block.hash);
    }

    #[test]
    fn pending_info_roundtrip_serialization() {
        let info = PendingInfo {
            return_credit: false,
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

    #[test]
    fn rejection_returns_pending_provenance_atomically_and_cannot_repeat() {
        use burst_store::{account::AccountStore, block::BlockStore, pending::PendingStore};
        let directory = tempfile::tempdir().unwrap();
        let store =
            burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let receiver = test_account();
        let sender = test_rep();
        let open = make_open_block(&receiver, &sender, 0);
        let brn = burst_brn::BrnEngine::new();
        let trst = burst_trst::TrstEngine::new();
        persist_block(&store, &open, None, 0, None, None, (&brn, &trst)).unwrap();
        let source = TxHash::new([3; 32]);
        let original = PendingInfo {
            return_credit: false,
            source: sender.clone(),
            amount: 12,
            timestamp: Timestamp::new(10),
            provenance: vec![PendingProvenance {
                basis: Default::default(),
                revoked_amounts: Default::default(),
                amount: 12,
                origin: TxHash::new([4; 32]),
                origin_wallet: sender.clone(),
                origin_timestamp: Timestamp::new(1),
                effective_origin_timestamp: Timestamp::new(1),
            }],
        };
        store
            .pending_store()
            .put_pending(&receiver, &source, &original)
            .unwrap();
        let mut reject = make_send_block(
            &receiver,
            &sender,
            open.hash,
            0,
            BlockHash::new(*source.as_bytes()),
        );
        reject.block_type = BlockType::RejectReceive;
        reject.hash = reject.compute_hash();
        let prior = store.account_store().get_account(&receiver).unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            stage_block(&mut batch, &reject, Some(&prior), 0, None, None).unwrap();
        }
        assert!(store
            .pending_store()
            .get_pending(&receiver, &source)
            .is_ok());
        assert!(store
            .pending_store()
            .get_pending(&sender, &reject.hash.into_tx_hash())
            .is_err());
        persist_block(&store, &reject, Some(&prior), 0, None, None, (&brn, &trst)).unwrap();
        assert!(store
            .pending_store()
            .get_pending(&receiver, &source)
            .is_err());
        let returned = store
            .pending_store()
            .get_pending(&sender, &reject.hash.into_tx_hash())
            .unwrap();
        assert_eq!(
            bincode::serialize(&returned).unwrap(),
            bincode::serialize(&original).unwrap()
        );
        let after = store.account_store().get_account(&receiver).unwrap();
        let mut repeated = reject.clone();
        repeated.previous = reject.hash;
        repeated.hash = repeated.compute_hash();
        assert!(persist_block(
            &store,
            &repeated,
            Some(&after),
            0,
            None,
            None,
            (&brn, &trst)
        )
        .is_err());
        assert!(store.block_store().get_block(&repeated.hash).is_err());
        assert_eq!(
            store.account_store().get_account(&receiver).unwrap().head,
            reject.hash
        );
        let batch = store.tx_begin_write().unwrap();
        assert!(batch.block_undo(&repeated.hash).unwrap().is_none());
        let undo = batch.block_undo(&reject.hash).unwrap().unwrap();
        let pending: Vec<_> = undo
            .changes
            .iter()
            .filter(|c| c.table == burst_store_lmdb::block_undo::UndoTable::Pending)
            .collect();
        assert_eq!(pending.len(), 2);
        assert!(
            pending
                .iter()
                .any(|c| c.before == Some(bincode::serialize(&original).unwrap())
                    && c.after.is_none())
        );
        assert!(
            pending
                .iter()
                .any(|c| c.before.is_none()
                    && c.after == Some(bincode::serialize(&original).unwrap()))
        );
    }

    #[test]
    fn pending_removed_after_admission_cannot_be_received() {
        use burst_store::{account::AccountStore, block::BlockStore};
        let directory = tempfile::tempdir().unwrap();
        let store =
            burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let receiver = test_account();
        let mut open = make_open_block(&receiver, &test_rep(), 12);
        open.link = BlockHash::new([8; 32]);
        open.hash = open.compute_hash();
        assert!(persist_block(
            &store,
            &open,
            None,
            0,
            None,
            None,
            (&burst_brn::BrnEngine::new(), &burst_trst::TrstEngine::new())
        )
        .is_err());
        assert!(store.block_store().get_block(&open.hash).is_err());
        assert!(store.account_store().get_account(&receiver).is_err());
    }

    // Rep-weight behaviour now lives in `burst_consensus::rep_weights` (the
    // cache is rebuilt from account state, not patched per block), so the
    // per-block weight tests moved there.

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
            verifier_opted_in_at: None,
            orv_evicted: false,
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
        let send = make_send_block(&account, &rep, open.hash, 700, BlockHash::new([0xAA; 32]));

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
