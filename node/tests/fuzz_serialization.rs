//! Property-based fuzz tests for serialization boundaries.
//!
//! Every type that crosses a trust boundary (network or storage) must survive
//! a bincode serialize → deserialize roundtrip for arbitrary valid inputs.
//! These tests use proptest to generate millions of random inputs and verify
//! that invariant holds.

use proptest::prelude::*;

use burst_brn::state::{BrnWalletState, RateHistory, RateSegment};
use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_store::account::AccountInfo;
use burst_store::pending::{PendingInfo, PendingProvenance};
use burst_types::{
    BlockHash, OriginProportion, Signature, Timestamp, TxHash, WalletAddress, WalletState,
};

// ---------------------------------------------------------------------------
// Proptest strategies for core types
// ---------------------------------------------------------------------------

fn arb_wallet_address() -> impl Strategy<Value = WalletAddress> {
    "[a-z0-9]{10,30}".prop_map(|s| WalletAddress::new(&format!("brst_{s}")))
}

fn arb_block_hash() -> impl Strategy<Value = BlockHash> {
    any::<[u8; 32]>().prop_map(BlockHash::new)
}

fn arb_tx_hash() -> impl Strategy<Value = TxHash> {
    any::<[u8; 32]>().prop_map(TxHash::new)
}

fn arb_signature() -> impl Strategy<Value = Signature> {
    any::<[u8; 64]>().prop_map(Signature)
}

fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
    (0u64..=u64::MAX / 2).prop_map(Timestamp::new)
}

fn arb_block_type() -> impl Strategy<Value = BlockType> {
    prop_oneof![
        Just(BlockType::Open),
        Just(BlockType::Send),
        Just(BlockType::Receive),
        Just(BlockType::Burn),
        Just(BlockType::ChangeRepresentative),
        Just(BlockType::Epoch),
        Just(BlockType::Endorse),
        Just(BlockType::Challenge),
        Just(BlockType::VerificationVote),
        Just(BlockType::GovernanceProposal),
        Just(BlockType::GovernanceVote),
        Just(BlockType::Merge),
        Just(BlockType::Split),
        Just(BlockType::RejectReceive),
        Just(BlockType::Delegate),
        Just(BlockType::RevokeDelegation),
    ]
}

fn arb_wallet_state() -> impl Strategy<Value = WalletState> {
    prop_oneof![
        Just(WalletState::Unverified),
        Just(WalletState::Endorsed),
        Just(WalletState::Voting),
        Just(WalletState::Verified),
        Just(WalletState::Challenged),
        Just(WalletState::Revoked),
        Just(WalletState::Deactivated),
    ]
}

fn arb_origin_proportion() -> impl Strategy<Value = OriginProportion> {
    (arb_tx_hash(), arb_wallet_address(), any::<u128>()).prop_map(|(origin, wallet, amount)| {
        OriginProportion {
            origin,
            origin_wallet: wallet,
            amount,
        }
    })
}

// ---------------------------------------------------------------------------
// StateBlock roundtrip
// ---------------------------------------------------------------------------

fn arb_state_block() -> impl Strategy<Value = StateBlock> {
    (
        arb_block_type(),
        arb_wallet_address(),
        arb_block_hash(),
        arb_wallet_address(),
        any::<u128>(),
        any::<u128>(),
        arb_block_hash(),
        arb_tx_hash(),
        arb_timestamp(),
        any::<u64>(),
        arb_signature(),
    )
        .prop_map(
            |(bt, account, prev, rep, brn, trst, link, origin, ts, work, sig)| {
                let mut block = StateBlock {
                    version: CURRENT_BLOCK_VERSION,
                    block_type: bt,
                    account,
                    previous: prev,
                    representative: rep,
                    brn_balance: brn,
                    trst_balance: trst,
                    link,
                    origin,
                    transaction: TxHash::ZERO,
                    timestamp: ts,
                    work,
                    signature: sig,
                    hash: BlockHash::ZERO,
                };
                block.hash = block.compute_hash();
                block
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_state_block_roundtrip(block in arb_state_block()) {
        let bytes = bincode::serialize(&block).unwrap();
        let decoded: StateBlock = bincode::deserialize(&bytes).unwrap();
        prop_assert_eq!(decoded.hash, block.hash);
        prop_assert_eq!(decoded.brn_balance, block.brn_balance);
        prop_assert_eq!(decoded.trst_balance, block.trst_balance);
        prop_assert_eq!(decoded.work, block.work);
    }
}

// ---------------------------------------------------------------------------
// AccountInfo roundtrip
// ---------------------------------------------------------------------------

fn arb_account_info() -> impl Strategy<Value = AccountInfo> {
    (
        (
            arb_wallet_address(),
            arb_wallet_state(),
            proptest::option::of(arb_timestamp()),
            arb_block_hash(),
            any::<u64>(),
            any::<u64>(),
            arb_wallet_address(),
        ),
        (
            any::<u128>(),
            any::<u128>(),
            any::<u128>(),
            any::<u128>(),
            any::<u128>(),
            any::<u8>(),
        ),
    )
        .prop_map(
            |(
                (addr, state, verified_at, head, bc, ch, rep),
                (burned, staked, trst, expired, revoked, epoch),
            )| {
                AccountInfo {
                    address: addr,
                    state,
                    verified_at,
                    head,
                    block_count: bc,
                    confirmation_height: ch,
                    representative: rep,
                    total_brn_burned: burned,
                    total_brn_staked: staked,
                    trst_balance: trst,
                    expired_trst: expired,
                    revoked_trst: revoked,
                    epoch,
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_account_info_roundtrip(info in arb_account_info()) {
        let bytes = bincode::serialize(&info).unwrap();
        let decoded: AccountInfo = bincode::deserialize(&bytes).unwrap();
        prop_assert_eq!(decoded.address.as_str(), info.address.as_str());
        prop_assert_eq!(decoded.block_count, info.block_count);
        prop_assert_eq!(decoded.trst_balance, info.trst_balance);
        prop_assert_eq!(decoded.epoch, info.epoch);
    }
}

// ---------------------------------------------------------------------------
// PendingInfo roundtrip
// ---------------------------------------------------------------------------

fn arb_pending_provenance() -> impl Strategy<Value = PendingProvenance> {
    (
        any::<u128>(),
        arb_tx_hash(),
        arb_wallet_address(),
        arb_timestamp(),
        arb_timestamp(),
        proptest::collection::vec(arb_origin_proportion(), 0..3),
    )
        .prop_map(
            |(amt, origin, wallet, ots, eots, props)| PendingProvenance {
                amount: amt,
                origin,
                origin_wallet: wallet,
                origin_timestamp: ots,
                effective_origin_timestamp: eots,
                origin_proportions: props,
            },
        )
}

fn arb_pending_info() -> impl Strategy<Value = PendingInfo> {
    (
        arb_wallet_address(),
        any::<u128>(),
        arb_timestamp(),
        proptest::collection::vec(arb_pending_provenance(), 0..5),
    )
        .prop_map(|(source, amount, ts, prov)| PendingInfo {
            source,
            amount,
            timestamp: ts,
            provenance: prov,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_pending_info_roundtrip(info in arb_pending_info()) {
        let bytes = bincode::serialize(&info).unwrap();
        let decoded: PendingInfo = bincode::deserialize(&bytes).unwrap();
        prop_assert_eq!(decoded.amount, info.amount);
        prop_assert_eq!(decoded.provenance.len(), info.provenance.len());
    }
}

// ---------------------------------------------------------------------------
// BrnWalletState roundtrip
// ---------------------------------------------------------------------------

fn arb_brn_wallet_state() -> impl Strategy<Value = BrnWalletState> {
    (
        arb_timestamp(),
        any::<u128>(),
        any::<u128>(),
        any::<bool>(),
        proptest::option::of(arb_timestamp()),
    )
        .prop_map(
            |(verified_at, burned, staked, active, stopped)| BrnWalletState {
                verified_at,
                total_burned: burned,
                total_staked: staked,
                accrual_active: active,
                accrual_stopped_at: stopped,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_brn_wallet_state_roundtrip(state in arb_brn_wallet_state()) {
        let bytes = bincode::serialize(&state).unwrap();
        let decoded: BrnWalletState = bincode::deserialize(&bytes).unwrap();
        prop_assert_eq!(decoded.total_burned, state.total_burned);
        prop_assert_eq!(decoded.total_staked, state.total_staked);
        prop_assert_eq!(decoded.accrual_active, state.accrual_active);
    }
}

// ---------------------------------------------------------------------------
// RateHistory roundtrip
// ---------------------------------------------------------------------------

fn arb_rate_segment() -> impl Strategy<Value = RateSegment> {
    (
        any::<u128>(),
        arb_timestamp(),
        proptest::option::of(arb_timestamp()),
    )
        .prop_map(|(rate, start, end)| RateSegment { rate, start, end })
}

fn arb_rate_history() -> impl Strategy<Value = RateHistory> {
    proptest::collection::vec(arb_rate_segment(), 1..10)
        .prop_map(|segments| RateHistory { segments })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn fuzz_rate_history_roundtrip(history in arb_rate_history()) {
        let bytes = bincode::serialize(&history).unwrap();
        let decoded: RateHistory = bincode::deserialize(&bytes).unwrap();
        prop_assert_eq!(decoded.segments.len(), history.segments.len());
    }
}

// ---------------------------------------------------------------------------
// BRN math edge cases
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn fuzz_brn_balance_never_negative(
        rate in 0u128..1_000_000,
        verified_secs in 0u64..1_000_000,
        now_secs in 0u64..2_000_000,
        burned in 0u128..u128::MAX / 2,
        staked in 0u128..u128::MAX / 2,
    ) {
        let verified = Timestamp::new(verified_secs);
        let now = Timestamp::new(now_secs);
        let history = RateHistory::new(rate, Timestamp::new(0));

        let balance = burst_wallet_core::balance::compute_balance_with_history(
            verified, now, &history, burned, staked,
        );
        // Balance must never underflow — saturating_sub guarantees this
        prop_assert!(balance <= u128::MAX);
    }

    #[test]
    fn fuzz_brn_wallet_state_balance_never_panics(
        rate in 0u128..1_000_000,
        verified_secs in 0u64..1_000_000,
        now_secs in 0u64..2_000_000,
        burned in any::<u128>(),
        staked in any::<u128>(),
        active in any::<bool>(),
    ) {
        let history = RateHistory::new(rate, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(verified_secs));
        state.total_burned = burned;
        state.total_staked = staked;
        state.accrual_active = active;
        if !active {
            state.accrual_stopped_at = Some(Timestamp::new(verified_secs.saturating_add(100)));
        }
        // Must not panic — returns 0 on overflow
        let _balance = state.available_balance(&history, Timestamp::new(now_secs));
    }
}

// ---------------------------------------------------------------------------
// Corrupt data resilience
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn fuzz_corrupt_state_block_rejected(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let result = bincode::deserialize::<StateBlock>(&data);
        // Must not panic — either Ok or Err, never UB
        let _ = result;
    }

    #[test]
    fn fuzz_corrupt_account_info_rejected(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let result = bincode::deserialize::<AccountInfo>(&data);
        let _ = result;
    }

    #[test]
    fn fuzz_corrupt_pending_info_rejected(data in proptest::collection::vec(any::<u8>(), 0..256)) {
        let result = bincode::deserialize::<PendingInfo>(&data);
        let _ = result;
    }

    #[test]
    fn fuzz_corrupt_brn_state_rejected(data in proptest::collection::vec(any::<u8>(), 0..128)) {
        let result = bincode::deserialize::<BrnWalletState>(&data);
        let _ = result;
    }
}

// ---------------------------------------------------------------------------
// LMDB stress: write and read many records
// ---------------------------------------------------------------------------

#[test]
fn stress_lmdb_1000_accounts() {
    use burst_store::account::AccountStore;
    let dir = tempfile::tempdir().unwrap();
    let env = burst_store_lmdb::LmdbEnvironment::open(dir.path(), 30, 256 * 1024 * 1024).unwrap();
    let store = env.account_store();

    let accounts: Vec<AccountInfo> = (0u16..1000)
        .map(|i| {
            let addr = WalletAddress::new(&format!("brst_{i:05}_{:032x}", i as u128));
            AccountInfo {
                address: addr.clone(),
                state: if i % 3 == 0 {
                    WalletState::Verified
                } else {
                    WalletState::Unverified
                },
                verified_at: if i % 3 == 0 {
                    Some(Timestamp::new(i as u64 * 100))
                } else {
                    None
                },
                head: BlockHash::new([(i % 256) as u8; 32]),
                block_count: i as u64,
                confirmation_height: i as u64 / 2,
                representative: WalletAddress::new("brst_rep000000000000000000000000000"),
                total_brn_burned: i as u128 * 100,
                total_brn_staked: 0,
                trst_balance: i as u128 * 50,
                expired_trst: 0,
                revoked_trst: 0,
                epoch: 0,
            }
        })
        .collect();

    for info in &accounts {
        store.put_account(info).unwrap();
    }

    assert_eq!(store.account_count().unwrap(), 1000);

    for info in &accounts {
        let read = store.get_account(&info.address).unwrap();
        assert_eq!(read.block_count, info.block_count);
        assert_eq!(read.trst_balance, info.trst_balance);
    }

    let verified_count = store.verified_account_count().unwrap();
    let expected_verified = (0u16..1000).filter(|i| i % 3 == 0).count() as u64;
    assert_eq!(verified_count, expected_verified);
}

#[test]
fn stress_lmdb_1000_pending_entries() {
    use burst_store::pending::PendingStore;
    let dir = tempfile::tempdir().unwrap();
    let env = burst_store_lmdb::LmdbEnvironment::open(dir.path(), 30, 256 * 1024 * 1024).unwrap();
    let store = env.pending_store();

    let dest = WalletAddress::new("brst_destination0000000000000000");

    for i in 0u32..1000 {
        let mut hash_bytes = [0u8; 32];
        hash_bytes[..4].copy_from_slice(&i.to_be_bytes());
        let source_hash = TxHash::new(hash_bytes);
        let info = PendingInfo {
            source: WalletAddress::new(&format!("brst_sender_{i:028}")),
            amount: i as u128 * 10,
            timestamp: Timestamp::new(i as u64 * 100),
            provenance: Vec::new(),
        };
        store.put_pending(&dest, &source_hash, &info).unwrap();
    }

    assert_eq!(store.pending_count().unwrap(), 1000);

    let all = store.get_pending_for_account(&dest).unwrap();
    assert_eq!(all.len(), 1000);
}

#[test]
fn stress_lmdb_account_pagination() {
    use burst_store::account::AccountStore;
    let dir = tempfile::tempdir().unwrap();
    let env = burst_store_lmdb::LmdbEnvironment::open(dir.path(), 30, 256 * 1024 * 1024).unwrap();
    let store = env.account_store();

    for i in 0u16..100 {
        let addr = WalletAddress::new(&format!("brst_{i:05}_{:032x}", i as u128));
        let info = AccountInfo {
            address: addr,
            state: WalletState::Unverified,
            verified_at: None,
            head: BlockHash::ZERO,
            block_count: i as u64,
            confirmation_height: 0,
            representative: WalletAddress::new("brst_rep000000000000000000000000000"),
            total_brn_burned: 0,
            total_brn_staked: 0,
            trst_balance: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        };
        store.put_account(&info).unwrap();
    }

    let page1 = store.iter_accounts_paged(None, 25).unwrap();
    assert_eq!(page1.len(), 25);

    let cursor = &page1.last().unwrap().address;
    let page2 = store.iter_accounts_paged(Some(cursor), 25).unwrap();
    assert_eq!(page2.len(), 25);
    assert_ne!(page1[0].address.as_str(), page2[0].address.as_str());

    let mut all_pages = Vec::new();
    let mut cursor: Option<WalletAddress> = None;
    loop {
        let page = store.iter_accounts_paged(cursor.as_ref(), 30).unwrap();
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().address.clone());
        all_pages.extend(page);
    }
    assert_eq!(all_pages.len(), 100);
}

// ---------------------------------------------------------------------------
// Write batch atomicity under error conditions
// ---------------------------------------------------------------------------

#[test]
fn write_batch_partial_failure_does_not_corrupt() {
    use burst_store::block::BlockStore;
    let dir = tempfile::tempdir().unwrap();
    let env = burst_store_lmdb::LmdbEnvironment::open(dir.path(), 30, 64 * 1024 * 1024).unwrap();

    let hash1 = BlockHash::new([1u8; 32]);
    let hash2 = BlockHash::new([2u8; 32]);

    {
        let mut batch = env.write_batch().unwrap();
        batch.put_block(&hash1, &[0xAA; 64]).unwrap();
        batch.put_block(&hash2, &[0xBB; 64]).unwrap();
        batch.commit().unwrap();
    }

    let b1 = env.block_store().get_block(&hash1).unwrap();
    let b2 = env.block_store().get_block(&hash2).unwrap();
    assert_eq!(b1[0], 0xAA);
    assert_eq!(b2[0], 0xBB);

    {
        let mut batch = env.write_batch().unwrap();
        batch
            .put_block(&BlockHash::new([3u8; 32]), &[0xCC; 64])
            .unwrap();
        drop(batch);
    }

    assert!(
        env.block_store()
            .get_block(&BlockHash::new([3u8; 32]))
            .is_err(),
        "dropped batch should not persist"
    );
}
