//! Configured network anchors and historical genesis fixtures.
//!
//! The running node uses `configured_genesis_block`, which commits the supplied
//! parameter hash and creator address at Unix epoch zero. Fixture helpers retain
//! their separate encoding and timestamps; their hashes are not launch anchors.

use crate::state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_crypto::blake2b_256;
use burst_types::{
    BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash, WalletAddress,
};

/// Network and creator used by the historical genesis fixture.
pub struct GenesisConfig {
    /// Which network this genesis is for.
    pub network: NetworkId,
    /// The wallet address of the genesis account (creator).
    pub creator: WalletAddress,
}

/// Historical fixture layout, not the configured node launch anchor.
/// Uses the current block version and fixture parameters; its hash can change
/// when those inputs change.
pub fn legacy_genesis_fixture(config: &GenesisConfig) -> StateBlock {
    let params = initial_params(&config.network);
    let link = params_link(&params, &config.network);
    let timestamp = genesis_timestamp(&config.network);

    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: BlockHash::ZERO,
        block_type: BlockType::Open,
        account: config.creator.clone(),
        previous: BlockHash::ZERO,
        representative: config.creator.clone(),
        brn_spent: 0,
        trst_balance: 0,
        link,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        timestamp,
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

/// Fixture hash using the network's placeholder creator address.
/// Deterministic for the current fixture inputs, not a configured launch hash.
pub fn legacy_genesis_fixture_hash(network: NetworkId) -> BlockHash {
    let config = GenesisConfig {
        network,
        creator: genesis_creator(&network),
    };
    let block = legacy_genesis_fixture(&config);
    block.hash
}

/// Parameter overrides for historical fixtures. Running nodes use their
/// configured parameters, not these Test/Dev overrides.
pub fn initial_params(network: &NetworkId) -> ProtocolParams {
    match network {
        NetworkId::Live => ProtocolParams::burst_defaults(),
        NetworkId::Test => {
            let mut params = ProtocolParams::burst_defaults();
            // Fixture overrides; not the running Test network defaults.
            params.endorsement_threshold = 1;
            params.num_verifiers = 3;
            params.bootstrap_exit_threshold = 5;
            params.min_work_difficulty = 0;
            params
        }
        NetworkId::Dev => {
            let mut params = ProtocolParams::burst_defaults();
            // Zero-work fixture with a one-person panel and early bootstrap exit.
            params.endorsement_threshold = 1;
            params.num_verifiers = 1;
            params.verification_threshold_bps = 5000;
            params.bootstrap_exit_threshold = 2;
            params.min_work_difficulty = 0;
            params
        }
    }
}

/// Placeholder addresses used only by historical fixtures.
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

/// Fixed fixture timestamp selected by network.
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

/// Fixture link hash over the network name and the six fields below.
/// This is not a commitment to the complete protocol parameter set.
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
    fn fixture_hash_is_deterministic() {
        let h1 = legacy_genesis_fixture_hash(NetworkId::Dev);
        let h2 = legacy_genesis_fixture_hash(NetworkId::Dev);
        assert_eq!(h1, h2);
    }

    #[test]
    fn fixture_hashes_differ_per_network() {
        let live = legacy_genesis_fixture_hash(NetworkId::Live);
        let test = legacy_genesis_fixture_hash(NetworkId::Test);
        let dev = legacy_genesis_fixture_hash(NetworkId::Dev);

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
        let block = legacy_genesis_fixture(&config);

        assert!(block.is_open());
        assert!(block.previous.is_zero());
        assert_eq!(block.block_type, BlockType::Open);
        assert_eq!(block.brn_spent, 0);
        assert_eq!(block.trst_balance, 0);
        assert_eq!(block.version, CURRENT_BLOCK_VERSION);
    }

    #[test]
    fn fixture_hash_not_zero() {
        let h = legacy_genesis_fixture_hash(NetworkId::Live);
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
        let block = legacy_genesis_fixture(&config);

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

/// Canonical launch anchor from explicit configured identity and parameters.
/// Network selection belongs to the caller; no placeholder identity is inferred.
pub fn configured_genesis_block(address: WalletAddress, params: &ProtocolParams) -> StateBlock {
    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: BlockHash::ZERO,
        block_type: BlockType::Open,
        account: address.clone(),
        representative: address,
        previous: BlockHash::ZERO,
        brn_spent: 0,
        trst_balance: 0,
        link: BlockHash::ZERO,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        target_head: BlockHash::ZERO,
        merge_sources: Vec::new(),
        release_sources: Vec::new(),
        timestamp: Timestamp::EPOCH,
        params_hash: params.params_hash(),
        work: 0,
        signature: Signature([0; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block
}
