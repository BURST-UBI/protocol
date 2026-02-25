//! Genesis block creation — the first block on each network.
//!
//! The genesis block is a special open block that bootstraps a network.
//! It has `previous: BlockHash::ZERO` (no predecessor), embeds the initial
//! `ProtocolParams`, and differs per `NetworkId` (live, test, dev) so that
//! each network has a unique, deterministic genesis hash.

use crate::state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_crypto::blake2b_256;
use burst_types::{
    BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash, WalletAddress,
};

/// Configuration for creating a genesis block.
pub struct GenesisConfig {
    /// Which network this genesis is for.
    pub network: NetworkId,
    /// The wallet address of the genesis account (creator).
    pub creator: WalletAddress,
}

/// Create the genesis block for a given configuration.
///
/// The genesis block is an open block with:
/// - `previous: BlockHash::ZERO` (no predecessor)
/// - `account`: the creator's wallet
/// - `link`: encodes the network-specific genesis seed
/// - Balances set to zero (BRN is computed, not stored)
/// - Timestamp set to the network's epoch start
///
/// The initial `ProtocolParams` are embedded into the block's link field
/// via a deterministic hash of the params + network id.
pub fn create_genesis_block(config: &GenesisConfig) -> StateBlock {
    let params = initial_params(&config.network);
    let link = params_link(&params, &config.network);
    let timestamp = genesis_timestamp(&config.network);

    let mut block = StateBlock {
        version: CURRENT_BLOCK_VERSION,
        block_type: BlockType::Open,
        account: config.creator.clone(),
        previous: BlockHash::ZERO,
        representative: config.creator.clone(),
        brn_balance: 0,
        trst_balance: 0,
        link,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        timestamp,
        params_hash: BlockHash::ZERO,
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block
}

/// Return the deterministic genesis block hash for a network.
///
/// Uses canonical genesis creator addresses so the hash is fully deterministic
/// per network. Useful for hardcoding known genesis hashes for bootstrapping.
pub fn genesis_hash(network: NetworkId) -> BlockHash {
    let config = GenesisConfig {
        network,
        creator: genesis_creator(&network),
    };
    let block = create_genesis_block(&config);
    block.hash
}

/// Return the initial protocol params for a network.
pub fn initial_params(network: &NetworkId) -> ProtocolParams {
    match network {
        NetworkId::Live => ProtocolParams::burst_defaults(),
        NetworkId::Test => {
            let mut params = ProtocolParams::burst_defaults();
            // Test network uses lower thresholds for faster iteration.
            params.endorsement_threshold = 1;
            params.num_verifiers = 3;
            params.bootstrap_exit_threshold = 5;
            params.min_work_difficulty = 0;
            params
        }
        NetworkId::Dev => {
            let mut params = ProtocolParams::burst_defaults();
            // Dev network disables anti-spam and lowers all thresholds.
            params.endorsement_threshold = 1;
            params.num_verifiers = 1;
            params.verification_threshold_bps = 5000;
            params.bootstrap_exit_threshold = 2;
            params.min_work_difficulty = 0;
            params.new_wallet_tx_limit_per_day = 1000;
            params
        }
    }
}

/// Canonical genesis creator address per network.
///
/// These are well-known addresses whose private keys are:
/// - Live: unknown (generated and discarded at launch)
/// - Test: published for public testing
/// - Dev: published for local development
fn genesis_creator(network: &NetworkId) -> WalletAddress {
    match network {
        NetworkId::Live => WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        ),
        NetworkId::Test => WalletAddress::new(
            "brst_1genesistest111111111111111111111111111111111111111111111111111111",
        ),
        NetworkId::Dev => WalletAddress::new(
            "brst_1genesisdev1111111111111111111111111111111111111111111111111111111",
        ),
    }
}

/// Genesis timestamp per network.
fn genesis_timestamp(network: &NetworkId) -> Timestamp {
    match network {
        // Live: 2026-01-01 00:00:00 UTC
        NetworkId::Live => Timestamp::new(1_767_225_600),
        // Test: 2025-06-01 00:00:00 UTC
        NetworkId::Test => Timestamp::new(1_748_736_000),
        // Dev: epoch 0
        NetworkId::Dev => Timestamp::new(0),
    }
}

/// Derive a deterministic link hash from protocol params + network id.
///
/// This embeds the initial params into the genesis block so that any node
/// can verify it was created with the expected configuration.
fn params_link(params: &ProtocolParams, network: &NetworkId) -> BlockHash {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(network.as_str().as_bytes());
    buffer.extend_from_slice(&params.brn_rate.to_be_bytes());
    buffer.extend_from_slice(&params.trst_expiry_secs.to_be_bytes());
    buffer.extend_from_slice(&params.endorsement_threshold.to_be_bytes());
    buffer.extend_from_slice(&params.num_verifiers.to_be_bytes());
    buffer.extend_from_slice(&params.min_work_difficulty.to_be_bytes());
    buffer.extend_from_slice(&params.bootstrap_exit_threshold.to_be_bytes());
    let hash = blake2b_256(&buffer);
    BlockHash::new(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_hash_is_deterministic() {
        let h1 = genesis_hash(NetworkId::Dev);
        let h2 = genesis_hash(NetworkId::Dev);
        assert_eq!(h1, h2);
    }

    #[test]
    fn genesis_hashes_differ_per_network() {
        let live = genesis_hash(NetworkId::Live);
        let test = genesis_hash(NetworkId::Test);
        let dev = genesis_hash(NetworkId::Dev);

        assert_ne!(live, test);
        assert_ne!(live, dev);
        assert_ne!(test, dev);
    }

    #[test]
    fn genesis_block_is_open() {
        let config = GenesisConfig {
            network: NetworkId::Dev,
            creator: genesis_creator(&NetworkId::Dev),
        };
        let block = create_genesis_block(&config);

        assert!(block.is_open());
        assert!(block.previous.is_zero());
        assert_eq!(block.block_type, BlockType::Open);
        assert_eq!(block.brn_balance, 0);
        assert_eq!(block.trst_balance, 0);
        assert_eq!(block.version, CURRENT_BLOCK_VERSION);
    }

    #[test]
    fn genesis_hash_not_zero() {
        let h = genesis_hash(NetworkId::Live);
        assert!(!h.is_zero());
    }

    #[test]
    fn create_genesis_with_custom_creator() {
        let creator = WalletAddress::new(
            "brst_1custom111111111111111111111111111111111111111111111111111111111111",
        );
        let config = GenesisConfig {
            network: NetworkId::Dev,
            creator: creator.clone(),
        };
        let block = create_genesis_block(&config);

        assert_eq!(block.account, creator);
        assert_eq!(block.representative, creator);
    }

    #[test]
    fn initial_params_dev_has_zero_work() {
        let params = initial_params(&NetworkId::Dev);
        assert_eq!(params.min_work_difficulty, 0);
        assert_eq!(params.endorsement_threshold, 1);
    }

    #[test]
    fn initial_params_live_has_burst_defaults() {
        let params = initial_params(&NetworkId::Live);
        let defaults = ProtocolParams::burst_defaults();
        assert_eq!(params.brn_rate, defaults.brn_rate);
        assert_eq!(params.trst_expiry_secs, defaults.trst_expiry_secs);
    }
}
