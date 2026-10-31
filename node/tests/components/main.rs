//! Component integration tests for block processing, economics and LMDB storage.
//! These fixtures call components directly; they do not establish full node admission.

use burst_brn::BrnEngine;
#[path = "../support/lifecycle_simulation.rs"]
mod lifecycle_simulation;
use burst_consensus::RepWeightCache;
use burst_crypto::{derive_address, keypair_from_seed, sign_message};
use burst_ledger::{BlockType, DagFrontier, StateBlock, CURRENT_BLOCK_VERSION};
use burst_store::block::BlockStore;
use burst_store::frontier::FrontierStore;
use burst_store::pending::PendingStore;
use burst_store_lmdb::LmdbEnvironment;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, Signature, Timestamp, TrstState, TxHash, WalletAddress};


fn temp_env() -> (tempfile::TempDir, LmdbEnvironment) {
    let dir = tempfile::tempdir().expect("temp dir");
    let env = LmdbEnvironment::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 * 1024 * 1024).expect("open env");
    (dir, env)
}

fn make_address(seed: u8) -> WalletAddress {
    let kp = keypair_from_seed(&[seed; 32]);
    derive_address(&kp.public)
}

fn pubkey_bytes(addr: &WalletAddress) -> [u8; 32] {
    burst_crypto::decode_address(addr.as_str()).expect("valid address")
}

#[allow(
    clippy::too_many_arguments,
    reason = "fixture exposes the signed block fields under test"
)]
fn make_block(
    block_type: BlockType,
    account: &WalletAddress,
    previous: BlockHash,
    rep: &WalletAddress,
    brn_spent: u128,
    trst_balance: u128,
    link: BlockHash,
    origin: TxHash,
    timestamp_secs: u64,
) -> StateBlock {
    let mut dummy_sig = [0u8; 64];
    dummy_sig[0] = 0xFF;
    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: burst_types::BlockHash::new([1; 32]),
        block_type,
        account: account.clone(),
        previous,
        representative: rep.clone(),
        brn_spent,
        trst_balance,
        link,
        origin,
        transaction: TxHash::ZERO,
        timestamp: Timestamp::new(timestamp_secs),
        params_hash: BlockHash::ZERO,
        target_head: burst_types::BlockHash::ZERO,
        merge_sources: Vec::new(),
        release_sources: Vec::new(),
        work: 0,
        signature: Signature(dummy_sig),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block
}

fn test_hash(n: u8) -> TxHash {
    TxHash::new([0xB0 + n; 32])
}

mod blocks;
mod consensus;
mod economics;
mod governance;
mod lifecycle;
mod payments;
mod storage;
mod verification;
