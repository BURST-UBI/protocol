//! LMDB environment setup.

use std::path::Path;

use heed::types::Bytes;
use heed::{Database, Env, EnvOpenOptions};

use crate::account::LmdbAccountStore;
use crate::block::LmdbBlockStore;
use crate::brn::LmdbBrnStore;
use crate::frontier::LmdbFrontierStore;
use crate::governance::LmdbGovernanceStore;
use crate::merger_graph::LmdbMergerGraphStore;
use crate::meta::LmdbMetaStore;
use crate::peer::LmdbPeerStore;
use crate::pending::LmdbPendingStore;
use crate::rep_weights::LmdbRepWeightStore;
use crate::shared_env::SharedEnv;
use crate::transaction::LmdbTransactionStore;
use crate::trst_index::LmdbTrstIndexStore;
use crate::verification::LmdbVerificationStore;
use crate::write_batch::{LmdbReadTransaction, LmdbWriteTransaction};
use crate::LmdbError;

/// Wraps the LMDB environment and all database handles.
pub struct LmdbEnvironment {
    env: SharedEnv,
    pub(crate) governance_cache:
        std::sync::Arc<std::sync::Mutex<Option<(usize, burst_governance::GovernanceEngine)>>>,

    pub(crate) auxiliary_cache: std::sync::Arc<
        std::sync::Mutex<Option<(usize, crate::write_batch::auxiliary_cache::AuxiliaryCache)>>,
    >,

    pub(crate) body_changes: std::sync::Arc<crate::retention::changes::Observers>,

    pub(crate) deferred_votes: std::sync::Arc<crate::meta::vote_admission::DeferredVotes>,
    pub(crate) sequence_commits:
        std::sync::Arc<std::sync::Mutex<crate::retention::SequenceCommits>>,

    // Account store
    pub(crate) accounts_db: Database<Bytes, Bytes>,
    pub(crate) verified_accounts_db: Database<Bytes, Bytes>,

    // Block store
    pub(crate) blocks_db: Database<Bytes, Bytes>,

    // Transaction store
    pub(crate) transactions_db: Database<Bytes, Bytes>,
    pub(crate) account_txs_db: Database<Bytes, Bytes>,

    // Merger graph store
    pub(crate) merger_origins_db: Database<Bytes, Bytes>,
    pub(crate) merger_downstream_db: Database<Bytes, Bytes>,
    pub(crate) merger_nodes_db: Database<Bytes, Bytes>,
    pub(crate) graph_position_db: Database<Bytes, Bytes>,

    // Verification store
    pub(crate) endorsements_db: Database<Bytes, Bytes>,
    pub(crate) verification_votes_db: Database<Bytes, Bytes>,
    pub(crate) challenges_db: Database<Bytes, Bytes>,

    // Governance store
    pub(crate) proposals_db: Database<Bytes, Bytes>,
    pub(crate) votes_db: Database<Bytes, Bytes>,
    pub(crate) delegations_db: Database<Bytes, Bytes>,
    pub(crate) constitution_db: Database<Bytes, Bytes>,

    // Frontier store
    pub(crate) frontiers_db: Database<Bytes, Bytes>,

    // Meta store
    pub(crate) meta_db: Database<Bytes, Bytes>,
    pub(crate) checkpoint_heads_db: Database<Bytes, Bytes>,

    // Pending store
    pub(crate) pending_db: Database<Bytes, Bytes>,
    pub(crate) pending_sender_db: Database<Bytes, Bytes>,
    pub(crate) pending_amount_db: Database<Bytes, Bytes>,

    // TRST index stores
    pub(crate) trst_origin_db: Database<Bytes, Bytes>,
    pub(crate) trst_expiry_db: Database<Bytes, Bytes>,
    /// Reverse index: tx_hash(32) → origin_hash(32) + expiry_be(8).
    /// Provides the origin and expiry needed to delete exact keys from
    /// the forward indexes without scanning them.
    pub(crate) trst_reverse_db: Database<Bytes, Bytes>,

    // BRN engine stores
    pub(crate) brn_wallets_db: Database<Bytes, Bytes>,
    pub(crate) brn_meta_db: Database<Bytes, Bytes>,

    // Block height index stores
    pub(crate) height_db: Database<Bytes, Bytes>,
    pub(crate) block_height_db: Database<Bytes, Bytes>,

    // Representative weight stores
    pub(crate) rep_weights_db: Database<Bytes, Bytes>,
    pub(crate) online_weight_db: Database<Bytes, Bytes>,

    // Peer cache store
    pub(crate) peers_db: Database<Bytes, Bytes>,
}

impl LmdbEnvironment {
    /// Open or create the ledger, validating its schema and initializing or
    /// rebuilding supported indexes in one write transaction. Existing indexes
    /// are not exhaustively checked here; opening is not a ledger integrity audit.
    pub fn open(path: &Path, max_dbs: u32, map_size: usize) -> Result<Self, LmdbError> {
        std::fs::create_dir_all(path)
            .map_err(|e| LmdbError::Heed(format!("failed to create directory: {e}")))?;

        let mut options = EnvOpenOptions::new();
        options.max_dbs(max_dbs).map_size(map_size);
        let env = SharedEnv::open(path, options)?;

        let mut wtxn = env.write_txn()?;

        let existing_meta: Option<Database<Bytes, Bytes>> =
            env.open_database(&wtxn, Some("meta"))?;
        let unversioned = crate::schema::check_version(match existing_meta {
            Some(meta) => meta.get(&wtxn, b"schema_version")?,
            None => None,
        })?;

        let accounts_db: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some("accounts"))?;
        let blocks_db = env.create_database(&mut wtxn, Some("blocks"))?;
        let transactions_db = env.create_database(&mut wtxn, Some("transactions"))?;
        let account_txs_db = env.create_database(&mut wtxn, Some("account_txs"))?;
        let merger_origins_db = env.create_database(&mut wtxn, Some("merger_origins"))?;
        let merger_downstream_db = env.create_database(&mut wtxn, Some("merger_downstream"))?;
        let merger_nodes_db = env.create_database(&mut wtxn, Some("merger_nodes"))?;
        let endorsements_db = env.create_database(&mut wtxn, Some("endorsements"))?;
        let verification_votes_db = env.create_database(&mut wtxn, Some("verification_votes"))?;
        let challenges_db = env.create_database(&mut wtxn, Some("challenges"))?;
        let proposals_db = env.create_database(&mut wtxn, Some("proposals"))?;
        let votes_db = env.create_database(&mut wtxn, Some("votes"))?;
        let delegations_db = env.create_database(&mut wtxn, Some("delegations"))?;
        let constitution_db = env.create_database(&mut wtxn, Some("constitution"))?;
        let frontiers_db = env.create_database(&mut wtxn, Some("frontiers"))?;
        let meta_db: Database<Bytes, Bytes> = env.create_database(&mut wtxn, Some("meta"))?;
        let pending_db = env.create_database(&mut wtxn, Some("pending"))?;
        let pending_sender_db: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some("pending_senders"))?;
        let pending_amount_db: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some("pending_amounts"))?;
        let trst_origin_db = env.create_database(&mut wtxn, Some("trst_origins"))?;
        let trst_expiry_db = env.create_database(&mut wtxn, Some("trst_expiry"))?;
        let trst_reverse_db = env.create_database(&mut wtxn, Some("trst_reverse"))?;
        let brn_wallets_db = env.create_database(&mut wtxn, Some("brn_wallets"))?;
        let brn_meta_db = env.create_database(&mut wtxn, Some("brn_meta"))?;
        let height_db = env.create_database(&mut wtxn, Some("block_heights"))?;
        let block_height_db = env.create_database(&mut wtxn, Some("block_height_reverse"))?;
        let rep_weights_db = env.create_database(&mut wtxn, Some("rep_weights"))?;
        let online_weight_db = env.create_database(&mut wtxn, Some("online_weights"))?;
        let peers_db = env.create_database(&mut wtxn, Some("peers"))?;
        let existing_positions: Option<Database<Bytes, Bytes>> =
            env.open_database(&wtxn, Some("graph_positions"))?;
        if meta_db.get(&wtxn, b"schema_version")?.is_some_and(|bytes| {
            bytes == 5u32.to_le_bytes()
                || bytes == 6u32.to_le_bytes()
                || bytes == 7u32.to_le_bytes()
                || bytes == crate::CURRENT_SCHEMA_VERSION.to_le_bytes()
        }) && existing_positions.is_none()
        {
            return Err(LmdbError::Heed(
                "schema with graph positions is missing its graph position index".into(),
            ));
        }
        let graph_position_db = env.create_database(&mut wtxn, Some("graph_positions"))?;

        let existing_verified: Option<Database<Bytes, Bytes>> =
            env.open_database(&wtxn, Some("verified_accounts"))?;
        if meta_db.get(&wtxn, b"schema_version")?.is_some_and(|bytes| {
            bytes == 7u32.to_le_bytes() || bytes == crate::CURRENT_SCHEMA_VERSION.to_le_bytes()
        }) && existing_verified.is_none()
        {
            return Err(LmdbError::Heed(
                "schema is missing its verified account index".into(),
            ));
        }
        let verified_accounts_db = env.create_database(&mut wtxn, Some("verified_accounts"))?;
        let existing_heads: Option<Database<Bytes, Bytes>> =
            env.open_database(&wtxn, Some("checkpoint_heads"))?;
        if meta_db.get(&wtxn, b"schema_version")?
            == Some(&crate::CURRENT_SCHEMA_VERSION.to_le_bytes()[..])
            && existing_heads.is_none()
        {
            return Err(LmdbError::Heed(
                "schema is missing its checkpoint head store".into(),
            ));
        }
        let checkpoint_heads_db = env.create_database(&mut wtxn, Some("checkpoint_heads"))?;

        // Pending-layout markers do not establish the signed block encoding.
        // Never relabel an unversioned nonempty ledger as the signing-grant schema.
        if unversioned {
            for database in [
                accounts_db,
                verified_accounts_db,
                blocks_db,
                transactions_db,
                account_txs_db,
                merger_origins_db,
                merger_downstream_db,
                merger_nodes_db,
                graph_position_db,
                endorsements_db,
                verification_votes_db,
                challenges_db,
                proposals_db,
                votes_db,
                delegations_db,
                constitution_db,
                frontiers_db,
                meta_db,
                checkpoint_heads_db,
                pending_db,
                pending_sender_db,
                pending_amount_db,
                trst_origin_db,
                trst_expiry_db,
                trst_reverse_db,
                brn_wallets_db,
                brn_meta_db,
                height_db,
                block_height_db,
                rep_weights_db,
                online_weight_db,
                peers_db,
            ] {
                if database.len(&wtxn)? != 0 {
                    return Err(LmdbError::Heed(
                        "unversioned ledger has no recognized layout; existing data was not migrated".into(),
                    ));
                }
            }
        }
        for (key, supported) in [
            (b"verified_count_version".as_slice(), 2u8),
            (b"pending_sender_index_version".as_slice(), 2u8),
        ] {
            if let Some(bytes) = meta_db.get(&wtxn, key)? {
                if bytes != [supported]
                    && !(bytes == [1]
                        && (key == b"verified_count_version"
                            || key == b"pending_sender_index_version"))
                {
                    return Err(LmdbError::Heed(format!(
                        "unsupported {} marker; existing data was not migrated",
                        String::from_utf8_lossy(key)
                    )));
                }
            }
        }

        match meta_db.get(&wtxn, b"pending_format_version")? {
            Some(&[2]) => {}
            None if blocks_db.len(&wtxn)? == 0 && pending_db.len(&wtxn)? == 0
                && accounts_db.len(&wtxn)? == 0 => {
                meta_db.put(&mut wtxn, b"pending_format_version", &[2])?;
            }
            _ => return Err(LmdbError::Heed(
                "ledger predates expiry-return credits; use a separate fresh data directory; existing data was not migrated".into())),
        }

        // v2 counts challenged humans until their case is resolved. Rebuild
        // once for old databases instead of retaining a reduced electorate.
        if meta_db.get(&wtxn, b"verified_count_version")? != Some(&[2][..]) {
            let mut count = 0u64;
            for entry in accounts_db.iter(&wtxn)? {
                let (_, bytes) = entry?;
                let account: burst_store::account::AccountInfo = bincode::deserialize(bytes)?;
                if account.state.is_verified() {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| LmdbError::Heed("verified count overflow".into()))?;
                }
            }
            meta_db.put(&mut wtxn, b"verified_count", &count.to_be_bytes())?;
            meta_db.put(&mut wtxn, b"verified_count_version", &[2])?;
        }
        if meta_db.get(&wtxn, b"pending_sender_index_version")? != Some(&[2][..]) {
            crate::pending::rebuild_sender_index(&mut wtxn, pending_db, pending_sender_db)?;
            crate::pending::amount_index::rebuild(&mut wtxn, pending_db, pending_amount_db)?;
            meta_db.put(&mut wtxn, b"pending_sender_index_version", &[2])?;
        }
        if meta_db.get(&wtxn, b"schema_version")?
            != Some(&crate::CURRENT_SCHEMA_VERSION.to_le_bytes()[..])
        {
            if meta_db
                .get(&wtxn, b"schema_version")?
                .is_none_or(|bytes| bytes == 3u32.to_le_bytes() || bytes == 4u32.to_le_bytes())
            {
                crate::graph_position::rebuild(&mut wtxn, meta_db, graph_position_db)?;
            }
            if meta_db.get(&wtxn, b"schema_version")? != Some(&7u32.to_le_bytes()[..]) {
                crate::verified_index::rebuild(&mut wtxn, accounts_db, verified_accounts_db)?;
            }
            meta_db.put(
                &mut wtxn,
                b"schema_version",
                &crate::CURRENT_SCHEMA_VERSION.to_le_bytes(),
            )?;
        }
        wtxn.commit()?;

        Ok(Self {
            governance_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            auxiliary_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            body_changes: Default::default(),
            deferred_votes: Default::default(),
            sequence_commits: Default::default(),
            env,
            accounts_db,
            verified_accounts_db,
            blocks_db,
            transactions_db,
            account_txs_db,
            merger_origins_db,
            merger_downstream_db,
            merger_nodes_db,
            graph_position_db,
            endorsements_db,
            verification_votes_db,
            challenges_db,
            proposals_db,
            votes_db,
            delegations_db,
            constitution_db,
            frontiers_db,
            meta_db,
            checkpoint_heads_db,
            pending_db,
            pending_sender_db,
            pending_amount_db,
            trst_origin_db,
            trst_expiry_db,
            trst_reverse_db,
            brn_wallets_db,
            brn_meta_db,
            height_db,
            block_height_db,
            rep_weights_db,
            online_weight_db,
            peers_db,
        })
    }

    /// Get a shared reference to the underlying heed environment.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// Begin a transaction for a bounded group with a separate journal per block.
    /// Publish effects only after commit. An error aborts the entire group;
    /// process blocks separately if failures need to be isolated.
    pub fn tx_begin_block_group(
        &self,
    ) -> Result<LmdbWriteTransaction<'_>, burst_store::StoreError> {
        LmdbWriteTransaction::new_block_group(self)
    }

    /// Begin a caller-owned write transaction. Pass it through related store
    /// operations to commit them atomically. Dropping it uncommitted aborts
    /// all staged changes.
    pub fn tx_begin_write(&self) -> Result<LmdbWriteTransaction<'_>, burst_store::StoreError> {
        LmdbWriteTransaction::new(self)
    }

    /// Begin a caller-owned read transaction: a consistent snapshot for
    /// grouped reads. Thread it through store `*_txn` read methods.
    pub fn tx_begin_read(&self) -> Result<LmdbReadTransaction<'_>, burst_store::StoreError> {
        LmdbReadTransaction::new(self)
    }

    /// Bind shared read operations to a pinned reader from this environment.
    pub fn read_view<'a>(
        &'a self,
        reader: &'a LmdbReadTransaction<'_>,
    ) -> Result<crate::read_view::LedgerReadView<'a>, burst_store::StoreError> {
        if !reader.belongs_to(self) {
            return Err(burst_store::StoreError::Corruption(
                "read view belongs to another environment".into(),
            ));
        }
        Ok(crate::read_view::LedgerReadView::new(
            self,
            &reader.txn,
            reader.clean_storage_cut(),
        ))
    }

    /// Create an account store backed by this environment.
    pub fn account_store(&self) -> LmdbAccountStore {
        LmdbAccountStore {
            env: self.env.clone(),
            accounts_db: self.accounts_db,
            verified_accounts_db: self.verified_accounts_db,
            meta_db: self.meta_db,
        }
    }

    /// Create a block store backed by this environment.
    pub fn block_store(&self) -> LmdbBlockStore {
        LmdbBlockStore {
            env: self.env.clone(),
            blocks_db: self.blocks_db,
            height_db: self.height_db,
            block_height_db: self.block_height_db,
        }
    }

    /// Create a transaction store backed by this environment.
    pub fn transaction_store(&self) -> LmdbTransactionStore {
        LmdbTransactionStore {
            env: self.env.clone(),
            transactions_db: self.transactions_db,
            account_txs_db: self.account_txs_db,
        }
    }

    /// Create a merger graph store backed by this environment.
    pub fn merger_graph_store(&self) -> LmdbMergerGraphStore {
        LmdbMergerGraphStore {
            env: self.env.clone(),
            merger_origins_db: self.merger_origins_db,
            merger_downstream_db: self.merger_downstream_db,
            merger_nodes_db: self.merger_nodes_db,
        }
    }

    /// Create a verification store backed by this environment.
    pub fn verification_store(&self) -> LmdbVerificationStore {
        LmdbVerificationStore {
            env: self.env.clone(),
            endorsements_db: self.endorsements_db,
            verification_votes_db: self.verification_votes_db,
            challenges_db: self.challenges_db,
        }
    }

    /// Create a governance store backed by this environment.
    pub fn governance_store(&self) -> LmdbGovernanceStore {
        LmdbGovernanceStore {
            env: self.env.clone(),
            proposals_db: self.proposals_db,
            votes_db: self.votes_db,
            delegations_db: self.delegations_db,
            constitution_db: self.constitution_db,
        }
    }

    /// Create a frontier store backed by this environment.
    pub fn frontier_store(&self) -> LmdbFrontierStore {
        LmdbFrontierStore {
            env: self.env.clone(),
            frontiers_db: self.frontiers_db,
        }
    }

    /// Create a meta store backed by this environment.
    pub fn meta_store(&self) -> LmdbMetaStore {
        LmdbMetaStore {
            governance_cache: self.governance_cache.clone(),
            auxiliary_cache: self.auxiliary_cache.clone(),
            body_changes: self.body_changes.clone(),
            deferred_votes: self.deferred_votes.clone(),
            sequence_commits: self.sequence_commits.clone(),
            env: self.env.clone(),
            meta_db: self.meta_db,
            graph_position_db: self.graph_position_db,
            checkpoint_heads_db: self.checkpoint_heads_db,
        }
    }

    /// Create a pending store backed by this environment.
    pub fn pending_store(&self) -> LmdbPendingStore {
        LmdbPendingStore {
            env: self.env.clone(),
            pending_db: self.pending_db,
            pending_sender_db: self.pending_sender_db,
            pending_amount_db: self.pending_amount_db,
        }
    }

    /// Create a BRN store backed by this environment.
    pub fn brn_store(&self) -> LmdbBrnStore {
        LmdbBrnStore::new(self.env.clone(), self.brn_wallets_db, self.brn_meta_db)
    }

    /// Create a representative weight store backed by this environment.
    pub fn rep_weight_store(&self) -> LmdbRepWeightStore {
        LmdbRepWeightStore {
            env: self.env.clone(),
            rep_weights_db: self.rep_weights_db,
            online_weight_db: self.online_weight_db,
        }
    }

    /// Create a peer cache store backed by this environment.
    pub fn peer_store(&self) -> LmdbPeerStore {
        LmdbPeerStore {
            env: self.env.clone(),
            peers_db: self.peers_db,
        }
    }

    /// Create a TRST index store backed by this environment.
    pub fn trst_index_store(&self) -> LmdbTrstIndexStore {
        LmdbTrstIndexStore {
            env: self.env.clone(),
            trst_origin_db: self.trst_origin_db,
            trst_expiry_db: self.trst_expiry_db,
            trst_reverse_db: self.trst_reverse_db,
        }
    }

    /// Ask LMDB to flush its data buffers to disk, even if relaxed sync flags
    /// are enabled. This does not commit any open write transaction.
    pub fn force_sync(&self) -> Result<(), LmdbError> {
        self.env.force_sync()?;
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use burst_store::account::AccountStore;
    use std::time::Duration;

    #[test]
    fn checkpoint_head_schema_migrates_seven_and_rejects_missing_current_table() {
        use burst_store::MetaStore;
        for version in [7u32, crate::CURRENT_SCHEMA_VERSION] {
            let dir = tempfile::tempdir().unwrap();
            let mut options = EnvOpenOptions::new();
            options.max_dbs(crate::REQUIRED_DATABASES).map_size(2 << 20);
            let raw = SharedEnv::open(dir.path(), options).unwrap();
            let mut txn = raw.write_txn().unwrap();
            let meta: Database<Bytes, Bytes> = raw.create_database(&mut txn, Some("meta")).unwrap();
            let _: Database<Bytes, Bytes> = raw
                .create_database(&mut txn, Some("graph_positions"))
                .unwrap();
            let _: Database<Bytes, Bytes> = raw
                .create_database(&mut txn, Some("verified_accounts"))
                .unwrap();
            meta.put(&mut txn, b"schema_version", &version.to_le_bytes())
                .unwrap();
            txn.commit().unwrap();
            let opened = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 2 << 20);
            if version == 7 {
                let env = opened.unwrap();
                assert_eq!(
                    env.meta_store().get_schema_version().unwrap(),
                    crate::CURRENT_SCHEMA_VERSION
                );
                let reader = env.tx_begin_read().unwrap();
                assert!(env
                    .read_view(&reader)
                    .unwrap()
                    .checkpoint_head_node(&[1; 32])
                    .unwrap()
                    .is_none());
            } else {
                assert!(opened.is_err());
                let reader = raw.read_txn().unwrap();
                assert!(raw
                    .open_database::<Bytes, Bytes>(&reader, Some("checkpoint_heads"))
                    .unwrap()
                    .is_none());
                assert_eq!(
                    meta.get(&reader, b"schema_version").unwrap(),
                    Some(version.to_le_bytes().as_slice())
                );
            }
        }
    }

    #[test]
    fn current_schema_rejects_missing_electorate_database() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = EnvOpenOptions::new();
        options.max_dbs(crate::REQUIRED_DATABASES).map_size(1 << 20);
        let raw = SharedEnv::open(dir.path(), options).unwrap();
        let mut txn = raw.write_txn().unwrap();
        let meta: Database<Bytes, Bytes> = raw.create_database(&mut txn, Some("meta")).unwrap();
        let _: Database<Bytes, Bytes> = raw
            .create_database(&mut txn, Some("graph_positions"))
            .unwrap();
        meta.put(
            &mut txn,
            b"schema_version",
            &crate::CURRENT_SCHEMA_VERSION.to_le_bytes(),
        )
        .unwrap();
        txn.commit().unwrap();
        assert!(LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 1 << 20).is_err());
        let txn = raw.read_txn().unwrap();
        assert!(raw
            .open_database::<Bytes, Bytes>(&txn, Some("verified_accounts"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn schema_open_rejects_unknown_versions_without_metadata_writes() {
        use burst_store::MetaStore;
        for (key, values) in [
            (
                "schema_version",
                vec![
                    vec![],
                    vec![2],
                    0u32.to_le_bytes().to_vec(),
                    1u32.to_le_bytes().to_vec(),
                    2u32.to_le_bytes().to_vec(),
                    (crate::CURRENT_SCHEMA_VERSION + 1).to_le_bytes().to_vec(),
                ],
            ),
            (
                "verified_count_version",
                vec![vec![], vec![0], vec![3], vec![2, 0]],
            ),
            (
                "pending_sender_index_version",
                vec![vec![], vec![0], vec![3], vec![1, 0]],
            ),
        ] {
            for bytes in values {
                let dir = tempfile::tempdir().unwrap();
                let env = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
                env.meta_store().put_meta(key, &bytes).unwrap();
                let before = env
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap();
                assert!(
                    LmdbEnvironment::open(dir.path(), 32, 1 << 20).is_err(),
                    "{key}: {bytes:?}"
                );
                if key == "schema_version" {
                    assert!(crate::check_schema(&env.meta_store()).is_err());
                }
                assert_eq!(
                    env.tx_begin_write()
                        .unwrap()
                        .checkpoint_metadata(b"")
                        .unwrap(),
                    before
                );
            }
        }
    }

    #[test]
    fn schema_open_refuses_unversioned_nonempty_store_without_relabelling_data() {
        use burst_store::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        assert_eq!(
            env.meta_store().get_schema_version().unwrap(),
            crate::CURRENT_SCHEMA_VERSION
        );
        env.meta_store().delete_meta("schema_version").unwrap();
        env.meta_store()
            .put_meta("verified_count_version", &[1])
            .unwrap();
        env.meta_store()
            .put_meta("verified_count", &99u64.to_be_bytes())
            .unwrap();
        env.meta_store()
            .put_meta("retained-test-data", b"preserve")
            .unwrap();
        assert!(LmdbEnvironment::open(dir.path(), 32, 1 << 20).is_err());
        assert_eq!(env.meta_store().get_schema_version().unwrap(), 0);
        assert_eq!(
            env.meta_store().get_meta("verified_count").unwrap(),
            99u64.to_be_bytes()
        );
        assert_eq!(
            env.meta_store().get_meta("retained-test-data").unwrap(),
            b"preserve"
        );
        assert!(crate::check_schema(&env.meta_store()).is_err());
    }

    #[test]
    fn schema_open_preserves_version_and_count_when_index_rebuild_fails() {
        use burst_store::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        {
            let mut txn = env.env().write_txn().unwrap();
            env.meta_db
                .delete(&mut txn, b"pending_sender_index_version")
                .unwrap();
            env.meta_db
                .put(&mut txn, b"verified_count_version", &[1])
                .unwrap();
            env.meta_db
                .put(&mut txn, b"verified_count", &99u64.to_be_bytes())
                .unwrap();
            env.pending_db
                .put(&mut txn, b"malformed-key", b"malformed-entry")
                .unwrap();
            env.pending_sender_db
                .put(&mut txn, b"retained-index", b"preserve")
                .unwrap();
            txn.commit().unwrap();
        }
        let before = env
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(LmdbEnvironment::open(dir.path(), 32, 1 << 20).is_err());
        assert_eq!(
            env.meta_store().get_schema_version().unwrap(),
            crate::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            env.tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        let txn = env.env().read_txn().unwrap();
        assert_eq!(
            env.pending_sender_db.get(&txn, b"retained-index").unwrap(),
            Some(b"preserve".as_slice())
        );
        assert_eq!(
            env.pending_db.get(&txn, b"malformed-key").unwrap(),
            Some(b"malformed-entry".as_slice())
        );
    }

    #[test]
    fn old_pending_format_is_rejected_without_rewriting_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        let mut txn = env.env().write_txn().unwrap();
        env.meta_db
            .put(&mut txn, b"pending_format_version", &[1])
            .unwrap();
        txn.commit().unwrap();
        assert!(matches!(LmdbEnvironment::open(dir.path(), 32, 1 << 20),
            Err(LmdbError::Heed(message)) if message.contains("was not migrated")));
        let txn = env.env().read_txn().unwrap();
        assert_eq!(
            env.meta_db.get(&txn, b"pending_format_version").unwrap(),
            Some(&[1][..])
        );
    }

    #[test]
    fn final_environment_owner_closes_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let environment =
            LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 1 << 20).unwrap();
        let closed = heed::env_closing_event(environment.env().path()).unwrap();
        drop(environment);
        assert!(
            closed.wait_timeout(Duration::from_secs(1)),
            "dropped environment retained LMDB handles"
        );
    }

    #[test]
    fn shared_stores_and_reopened_environments_keep_the_database_alive() {
        let dir = tempfile::tempdir().unwrap();
        let first = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        let closed = heed::env_closing_event(first.env().path()).unwrap();
        let accounts = first.account_store();
        let second = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        drop(first);
        let third = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        drop(second);
        drop(third);
        assert!(!closed.wait_timeout(Duration::ZERO));
        assert!(accounts.all_accounts().unwrap().is_empty());
        let fourth = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        drop(accounts);
        assert!(!closed.wait_timeout(Duration::ZERO));
        drop(fourth);
        assert!(closed.wait_timeout(Duration::from_secs(1)));
        let reopened = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        assert!(reopened.account_store().all_accounts().unwrap().is_empty());
    }

    #[test]
    fn failed_initialization_releases_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LmdbEnvironment::open(dir.path(), 1, 1 << 20).is_err());
        assert!(heed::env_closing_event(dir.path()).is_none());
        let environment = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
        assert!(environment
            .account_store()
            .all_accounts()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn concurrent_open_and_drop_leave_no_environment_registered() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..16 {
                        let environment = LmdbEnvironment::open(dir.path(), 32, 1 << 20).unwrap();
                        let accounts = environment.account_store();
                        drop(environment);
                        assert!(accounts.all_accounts().unwrap().is_empty());
                    }
                });
            }
        });
        assert!(heed::env_closing_event(dir.path()).is_none());
    }
}
