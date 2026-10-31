//! Fail closed when a ledger directory is reused for a different network.
use bincode::Options;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{block::BlockStore, meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams, Timestamp};

use crate::{genesis_key, NodeError};

const NETWORK_KEY: &str = "ledger_network";
const GENESIS_PARAMS_KEY: &str = "genesis_params/v1";

/// Load or pin the original parameters used to derive genesis. On a nonempty
/// ledger they must reproduce its indexed genesis hash; active governance
/// parameters may differ. An empty ledger must match the supplied configuration.
pub(crate) fn genesis_params(
    store: &LmdbStore,
    network: NetworkId,
    configured: &ProtocolParams,
) -> Result<ProtocolParams, NodeError> {
    let fail =
        |error: String| NodeError::Other(format!("cannot establish genesis parameters: {error}"));
    let meta = store.meta_store();
    let existing = match meta.get_meta(GENESIS_PARAMS_KEY) {
        Ok(bytes) => Some(bytes),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(fail(error.to_string())),
    };
    let params = match &existing {
        Some(bytes) => bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(64 * 1024)
            .deserialize::<ProtocolParams>(bytes)
            .map_err(|e| fail(e.to_string()))?,
        None => configured.clone(),
    };
    let expected = crate::genesis_state::block(network, &params);
    let actual = store
        .block_store()
        .block_at_height(&expected.account, 1)
        .map_err(|e| fail(e.to_string()))?;
    if actual.is_none()
        && store
            .block_store()
            .block_count()
            .map_err(|e| fail(e.to_string()))?
            > 0
    {
        return Err(fail("nonempty ledger has no indexed genesis".into()));
    }
    match actual {
        Some(hash) if hash != expected.hash => {
            return Err(fail(
                "parameters do not match the stored genesis; provide its original configuration"
                    .into(),
            ))
        }
        None if params.params_hash() != configured.params_hash() => {
            return Err(fail(
                "empty ledger already has different pinned genesis parameters".into(),
            ))
        }
        _ => (),
    }
    if existing.is_none() {
        let bytes = bincode::serialize(&params).map_err(|e| fail(e.to_string()))?;
        let mut batch = store.tx_begin_write().map_err(|e| fail(e.to_string()))?;
        if batch
            .get_meta(GENESIS_PARAMS_KEY)
            .map_err(|e| fail(e.to_string()))?
            .is_some()
        {
            return Err(fail(
                "genesis parameters changed during initialization; retry".into(),
            ));
        }
        batch
            .put_meta(GENESIS_PARAMS_KEY, &bytes)
            .map_err(|e| fail(e.to_string()))?;
        batch.commit().map_err(|e| fail(e.to_string()))?;
        store.force_sync().map_err(|e| fail(e.to_string()))?;
    }
    Ok(params)
}

pub(crate) fn verify_network(store: &LmdbStore, network: NetworkId) -> Result<(), NodeError> {
    let meta = store.meta_store();
    let existing = match meta.get_meta(NETWORK_KEY) {
        Ok(value) => Some(value),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => {
            return Err(NodeError::Other(format!(
                "cannot read ledger network: {error}"
            )))
        }
    };
    if existing
        .as_deref()
        .is_some_and(|value| value != network.as_str().as_bytes())
    {
        return Err(NodeError::Config(format!(
            "data directory belongs to another network, not {}; use a separate data directory",
            network.as_str(),
        )));
    }

    let blocks = store.block_store();
    let count = blocks
        .block_count()
        .map_err(|e| NodeError::Other(format!("cannot read ledger block count: {e}")))?;
    if count > 0 {
        // Validate genesis even when a network marker exists. Its original
        // parameter hash is checked separately by genesis_params.
        let address = genesis_key::genesis_address(network);
        let hash = blocks
            .block_at_height(&address, 1)
            .map_err(|e| NodeError::Other(format!("cannot read genesis index: {e}")))?
            .ok_or_else(|| {
                NodeError::Config(format!(
                "data directory has no {} genesis; refusing mixed-network or unidentified ledger",
                network.as_str(),
            ))
            })?;
        let bytes = blocks
            .get_block(&hash)
            .map_err(|e| NodeError::Other(format!("cannot read genesis block: {e}")))?;
        let version = *bytes
            .first()
            .ok_or_else(|| NodeError::Config("empty genesis encoding".into()))?;
        if version != burst_ledger::CURRENT_BLOCK_VERSION {
            return Err(NodeError::Config(format!(
                "ledger uses block version {}; this build requires version {}; use a separate fresh data directory; existing data was not migrated",
                version, burst_ledger::CURRENT_BLOCK_VERSION,
            )));
        }
        let block: StateBlock = bincode::deserialize(&bytes)
            .map_err(|e| NodeError::Other(format!("invalid genesis encoding: {e}")))?;
        if block.account != address
            || !block.payment_epoch.is_zero()
            || block.representative != address
            || block.block_type != BlockType::Open
            || !block.previous.is_zero()
            || block.timestamp != Timestamp::EPOCH
            || !block.link.is_zero()
            || !block.origin.is_zero()
            || !block.transaction.is_zero()
            || !block.target_head.is_zero()
            || block.brn_spent != 0
            || block.trst_balance != 0
            || !block.merge_sources.is_empty()
            || block.hash != hash
            || block.compute_hash() != hash
        {
            return Err(NodeError::Config(
                "data directory has an invalid network genesis".into(),
            ));
        }
    }
    if existing.is_none() {
        meta.put_meta(NETWORK_KEY, network.as_str().as_bytes())
            .map_err(|e| NodeError::Other(format!("cannot pin ledger network: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::BlockHash;

    #[test]
    fn genesis_parameters_survive_governed_rule_changes_and_are_bound_to_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let params = ProtocolParams::default();
        let initial = genesis_params(&store, NetworkId::Dev, &params).unwrap();
        let block = crate::genesis_state::block(NetworkId::Dev, &params);
        store
            .block_store()
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(&block).unwrap(),
                &block.account,
            )
            .unwrap();
        let mut governed = params.clone();
        governed.endorsement_threshold += 1;
        let loaded = genesis_params(&store, NetworkId::Dev, &governed).unwrap();
        assert_eq!(loaded.params_hash(), initial.params_hash());
        assert_ne!(loaded.params_hash(), governed.params_hash());
        assert!(genesis_params(&store, NetworkId::Test, &params).is_err());
        store
            .meta_store()
            .put_meta(GENESIS_PARAMS_KEY, &bincode::serialize(&governed).unwrap())
            .unwrap();
        assert!(genesis_params(&store, NetworkId::Dev, &params).is_err());
    }

    #[test]
    fn legacy_genesis_parameter_pin_requires_exact_inputs_without_rewriting_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let params = ProtocolParams::default();
        let block = crate::genesis_state::block(NetworkId::Dev, &params);
        let bytes = bincode::serialize(&block).unwrap();
        store
            .block_store()
            .put_block_with_account(&block.hash, &bytes, &block.account)
            .unwrap();
        let mut wrong = params.clone();
        wrong.brn_rate += 1;
        assert!(genesis_params(&store, NetworkId::Dev, &wrong).is_err());
        assert!(store.meta_store().get_meta(GENESIS_PARAMS_KEY).is_err());
        genesis_params(&store, NetworkId::Dev, &params).unwrap();
        assert_eq!(store.block_store().get_block(&block.hash).unwrap(), bytes);
        let mut invalid = store.meta_store().get_meta(GENESIS_PARAMS_KEY).unwrap();
        invalid.push(0);
        store
            .meta_store()
            .put_meta(GENESIS_PARAMS_KEY, &invalid)
            .unwrap();
        assert!(genesis_params(&store, NetworkId::Dev, &params).is_err());
    }

    #[test]
    fn empty_ledger_cannot_switch_pinned_genesis_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let params = ProtocolParams::default();
        genesis_params(&store, NetworkId::Dev, &params).unwrap();
        let mut other = params.clone();
        other.num_verifiers += 1;
        assert!(genesis_params(&store, NetworkId::Dev, &other).is_err());
        assert_eq!(
            genesis_params(&store, NetworkId::Dev, &params)
                .unwrap()
                .params_hash(),
            params.params_hash()
        );
    }

    fn legacy_genesis(store: &LmdbStore, network: NetworkId) -> StateBlock {
        let mut block =
            burst_ledger::genesis::legacy_genesis_fixture(&burst_ledger::genesis::GenesisConfig {
                network,
                creator: genesis_key::genesis_address(network),
            });
        // Give the legacy fixture the node's epoch timestamp and empty link.
        block.timestamp = Timestamp::EPOCH;
        block.link = BlockHash::ZERO;
        block.hash = block.compute_hash();
        store
            .block_store()
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(&block).unwrap(),
                &block.account,
            )
            .unwrap();
        block
    }

    #[test]
    fn fresh_directory_is_pinned_and_cannot_switch_networks() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        verify_network(&store, NetworkId::Dev).unwrap();
        verify_network(&store, NetworkId::Dev).unwrap();
        assert!(verify_network(&store, NetworkId::Live).is_err());
        assert!(verify_network(&store, NetworkId::Test).is_err());
    }

    #[test]
    fn old_work_rules_require_a_separate_ledger_without_deleting_data() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let mut block =
            burst_ledger::genesis::legacy_genesis_fixture(&burst_ledger::genesis::GenesisConfig {
                network: NetworkId::Dev,
                creator: genesis_key::genesis_address(NetworkId::Dev),
            });
        block.version = 1;
        block.timestamp = Timestamp::EPOCH;
        block.link = BlockHash::ZERO;
        block.hash = block.compute_hash();
        let bytes = bincode::serialize(&block).unwrap();
        store
            .block_store()
            .put_block_with_account(&block.hash, &bytes, &block.account)
            .unwrap();
        let error = verify_network(&store, NetworkId::Dev)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!(
            "requires version {}",
            burst_ledger::CURRENT_BLOCK_VERSION
        )));
        assert_eq!(store.block_store().get_block(&block.hash).unwrap(), bytes);
        assert_eq!(store.block_store().block_count().unwrap(), 1);
        assert!(store.meta_store().get_meta(NETWORK_KEY).is_err());
    }

    #[test]
    fn version_two_encoding_is_rejected_before_decoding_or_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let mut block =
            burst_ledger::genesis::legacy_genesis_fixture(&burst_ledger::genesis::GenesisConfig {
                network: NetworkId::Dev,
                creator: genesis_key::genesis_address(NetworkId::Dev),
            });
        block.version = 2;
        let prefix = bincode::serialized_size(&(
            block.version,
            &block.block_type,
            &block.account,
            block.previous,
            &block.representative,
            block.brn_spent,
            block.trst_balance,
            block.link,
            block.origin,
            block.transaction,
        ))
        .unwrap() as usize;
        let mut bytes = bincode::serialize(&block).unwrap();
        bytes.drain(1..33);
        bytes.drain(prefix..prefix + 32);
        store
            .block_store()
            .put_block_with_account(&block.hash, &bytes, &block.account)
            .unwrap();
        let error = verify_network(&store, NetworkId::Dev)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ledger uses block version 2"), "{error}");
        assert!(error.contains("existing data was not migrated"), "{error}");
        assert_eq!(store.block_store().get_block(&block.hash).unwrap(), bytes);
        assert!(store.meta_store().get_meta(NETWORK_KEY).is_err());
    }

    #[test]
    fn version_four_ledger_is_rejected_without_migration_or_relabelling() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        let mut bytes = bincode::serialize(&block).unwrap();
        bytes[0] = 4;
        bytes.drain(1..33); // Version 4 had no signed payment epoch.
        store
            .block_store()
            .put_block_with_account(&block.hash, &bytes, &block.account)
            .unwrap();
        let error = verify_network(&store, NetworkId::Dev)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ledger uses block version 4"), "{error}");
        assert!(error.contains("existing data was not migrated"), "{error}");
        assert_eq!(store.block_store().get_block(&block.hash).unwrap(), bytes);
        assert!(store.meta_store().get_meta(NETWORK_KEY).is_err());
        assert!(store
            .meta_store()
            .get_meta(burst_store::meta::PAYMENT_EPOCH_KEY)
            .is_err());
    }

    #[test]
    fn legacy_test_genesis_cannot_be_opened_as_live_and_is_not_relabelled() {
        for network in [NetworkId::Dev, NetworkId::Test] {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
            let block = legacy_genesis(&store, network);
            assert!(verify_network(&store, NetworkId::Live).is_err());
            assert!(store.meta_store().get_meta(NETWORK_KEY).is_err());
            verify_network(&store, network).unwrap();
            assert_eq!(store.block_store().block_count().unwrap(), 1);
            assert!(store.block_store().exists(&block.hash).unwrap());
        }
    }

    #[test]
    fn metadata_does_not_override_a_foreign_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        legacy_genesis(&store, NetworkId::Dev);
        store.meta_store().put_meta(NETWORK_KEY, b"live").unwrap();
        assert!(verify_network(&store, NetworkId::Live).is_err());
    }

    #[test]
    fn invalid_genesis_is_not_adopted_as_a_legacy_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), burst_store_lmdb::REQUIRED_DATABASES, 64 << 20).unwrap();
        let block = legacy_genesis(&store, NetworkId::Live);
        let mut corrupt = block.clone();
        corrupt.trst_balance = 1;
        store
            .block_store()
            .put_block(&block.hash, &bincode::serialize(&corrupt).unwrap())
            .unwrap();
        assert!(verify_network(&store, NetworkId::Live).is_err());
        assert!(store.meta_store().get_meta(NETWORK_KEY).is_err());
    }
}
