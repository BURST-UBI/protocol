//! LMDB environment setup.

use std::path::Path;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env, EnvOpenOptions};

use crate::account::LmdbAccountStore;
use crate::block::LmdbBlockStore;
use crate::brn::LmdbBrnStore;
use crate::frontier::LmdbFrontierStore;
use crate::governance::LmdbGovernanceStore;
use crate::merger_graph::LmdbMergerGraphStore;
use crate::meta::LmdbMetaStore;
use crate::pending::LmdbPendingStore;
use crate::rep_weights::LmdbRepWeightStore;
use crate::transaction::LmdbTransactionStore;
use crate::trst_index::LmdbTrstIndexStore;
use crate::verification::LmdbVerificationStore;
use crate::write_batch::WriteBatch;
use crate::LmdbError;

/// Wraps the LMDB environment and all database handles.
pub struct LmdbEnvironment {
    env: Arc<Env>,

    // Account store
    pub(crate) accounts_db: Database<Bytes, Bytes>,

    // Block store
    pub(crate) blocks_db: Database<Bytes, Bytes>,

    // Transaction store
    pub(crate) transactions_db: Database<Bytes, Bytes>,
    pub(crate) account_txs_db: Database<Bytes, Bytes>,

    // Merger graph store
    pub(crate) merger_origins_db: Database<Bytes, Bytes>,
    pub(crate) merger_downstream_db: Database<Bytes, Bytes>,
    pub(crate) merger_nodes_db: Database<Bytes, Bytes>,

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

    // Pending store
    pub(crate) pending_db: Database<Bytes, Bytes>,

    // TRST index stores
    pub(crate) trst_origin_db: Database<Bytes, Bytes>,
    pub(crate) trst_expiry_db: Database<Bytes, Bytes>,
    /// Reverse index: tx_hash(32) → origin_hash(32) + expiry_be(8).
    /// Enables O(1) `delete_token` by providing the origin and expiry
    /// needed to construct exact keys for the forward indexes.
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
}

impl LmdbEnvironment {
    /// Open or create an LMDB environment at the given path.
    pub fn open(path: &Path, max_dbs: u32, map_size: usize) -> Result<Self, LmdbError> {
        std::fs::create_dir_all(path)
            .map_err(|e| LmdbError::Heed(format!("failed to create directory: {e}")))?;

        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(max_dbs)
                .map_size(map_size)
                .open(path)?
        };

        let mut wtxn = env.write_txn()?;

        let accounts_db = env.create_database(&mut wtxn, Some("accounts"))?;
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
        let meta_db = env.create_database(&mut wtxn, Some("meta"))?;
        let pending_db = env.create_database(&mut wtxn, Some("pending"))?;
        let trst_origin_db = env.create_database(&mut wtxn, Some("trst_origins"))?;
        let trst_expiry_db = env.create_database(&mut wtxn, Some("trst_expiry"))?;
        let trst_reverse_db = env.create_database(&mut wtxn, Some("trst_reverse"))?;
        let brn_wallets_db = env.create_database(&mut wtxn, Some("brn_wallets"))?;
        let brn_meta_db = env.create_database(&mut wtxn, Some("brn_meta"))?;
        let height_db = env.create_database(&mut wtxn, Some("block_heights"))?;
        let block_height_db = env.create_database(&mut wtxn, Some("block_height_reverse"))?;
        let rep_weights_db = env.create_database(&mut wtxn, Some("rep_weights"))?;
        let online_weight_db = env.create_database(&mut wtxn, Some("online_weights"))?;

        wtxn.commit()?;

        Ok(Self {
            env: Arc::new(env),
            accounts_db,
            blocks_db,
            transactions_db,
            account_txs_db,
            merger_origins_db,
            merger_downstream_db,
            merger_nodes_db,
            endorsements_db,
            verification_votes_db,
            challenges_db,
            proposals_db,
            votes_db,
            delegations_db,
            constitution_db,
            frontiers_db,
            meta_db,
            pending_db,
            trst_origin_db,
            trst_expiry_db,
            trst_reverse_db,
            brn_wallets_db,
            brn_meta_db,
            height_db,
            block_height_db,
            rep_weights_db,
            online_weight_db,
        })
    }

    /// Get a shared reference to the underlying heed environment.
    pub fn env(&self) -> &Arc<Env> {
        &self.env
    }

    /// Begin a write batch for grouping multiple store operations into a
    /// single LMDB write transaction, amortising the fsync cost.
    pub fn write_batch(&self) -> Result<WriteBatch<'_>, burst_store::StoreError> {
        WriteBatch::new(self)
    }

    /// Create an account store backed by this environment.
    pub fn account_store(&self) -> LmdbAccountStore {
        LmdbAccountStore {
            env: Arc::clone(&self.env),
            accounts_db: self.accounts_db,
            meta_db: self.meta_db,
        }
    }

    /// Create a block store backed by this environment.
    pub fn block_store(&self) -> LmdbBlockStore {
        LmdbBlockStore {
            env: Arc::clone(&self.env),
            blocks_db: self.blocks_db,
            height_db: self.height_db,
            block_height_db: self.block_height_db,
        }
    }

    /// Create a transaction store backed by this environment.
    pub fn transaction_store(&self) -> LmdbTransactionStore {
        LmdbTransactionStore {
            env: Arc::clone(&self.env),
            transactions_db: self.transactions_db,
            account_txs_db: self.account_txs_db,
        }
    }

    /// Create a merger graph store backed by this environment.
    pub fn merger_graph_store(&self) -> LmdbMergerGraphStore {
        LmdbMergerGraphStore {
            env: Arc::clone(&self.env),
            merger_origins_db: self.merger_origins_db,
            merger_downstream_db: self.merger_downstream_db,
            merger_nodes_db: self.merger_nodes_db,
        }
    }

    /// Create a verification store backed by this environment.
    pub fn verification_store(&self) -> LmdbVerificationStore {
        LmdbVerificationStore {
            env: Arc::clone(&self.env),
            endorsements_db: self.endorsements_db,
            verification_votes_db: self.verification_votes_db,
            challenges_db: self.challenges_db,
        }
    }

    /// Create a governance store backed by this environment.
    pub fn governance_store(&self) -> LmdbGovernanceStore {
        LmdbGovernanceStore {
            env: Arc::clone(&self.env),
            proposals_db: self.proposals_db,
            votes_db: self.votes_db,
            delegations_db: self.delegations_db,
            constitution_db: self.constitution_db,
        }
    }

    /// Create a frontier store backed by this environment.
    pub fn frontier_store(&self) -> LmdbFrontierStore {
        LmdbFrontierStore {
            env: Arc::clone(&self.env),
            frontiers_db: self.frontiers_db,
        }
    }

    /// Create a meta store backed by this environment.
    pub fn meta_store(&self) -> LmdbMetaStore {
        LmdbMetaStore {
            env: Arc::clone(&self.env),
            meta_db: self.meta_db,
        }
    }

    /// Create a pending store backed by this environment.
    pub fn pending_store(&self) -> LmdbPendingStore {
        LmdbPendingStore {
            env: Arc::clone(&self.env),
            pending_db: self.pending_db,
        }
    }

    /// Create a BRN store backed by this environment.
    pub fn brn_store(&self) -> LmdbBrnStore {
        LmdbBrnStore::new(Arc::clone(&self.env), self.brn_wallets_db, self.brn_meta_db)
    }

    /// Create a representative weight store backed by this environment.
    pub fn rep_weight_store(&self) -> LmdbRepWeightStore {
        LmdbRepWeightStore {
            env: Arc::clone(&self.env),
            rep_weights_db: self.rep_weights_db,
            online_weight_db: self.online_weight_db,
        }
    }

    /// Create a TRST index store backed by this environment.
    pub fn trst_index_store(&self) -> LmdbTrstIndexStore {
        LmdbTrstIndexStore {
            env: Arc::clone(&self.env),
            trst_origin_db: self.trst_origin_db,
            trst_expiry_db: self.trst_expiry_db,
            trst_reverse_db: self.trst_reverse_db,
        }
    }

    /// Force an `fsync` of the LMDB memory-mapped file to disk.
    ///
    /// LMDB ensures durability on every write-transaction commit. This
    /// method is an extra safety measure to call during graceful shutdown,
    /// ensuring the OS has flushed all dirty pages before the process exits.
    pub fn force_sync(&self) -> Result<(), LmdbError> {
        // A committed read-txn is effectively a no-op write; however the
        // underlying mdb_env_sync(1) (force=true) is what we want.
        // heed doesn't expose `mdb_env_sync` directly, so we open and
        // immediately commit a write-txn to flush the WAL.
        let wtxn = self.env.write_txn()?;
        wtxn.commit()?;
        Ok(())
    }
}
