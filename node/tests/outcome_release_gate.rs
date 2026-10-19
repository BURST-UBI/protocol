use burst_consensus::checkpoint::{Checkpoint, CheckpointCertificate};
use burst_node::checkpoint_outcome::CertifiedChallengeOutcome;
use burst_node::checkpoint_replay::ReplayedCheckpoint;
use burst_node::checkpoint_store::CertifiedCheckpoint;
use burst_node::confirmed_history::HistoryHead;
use burst_store::meta::MetaStore;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams, WalletAddress};
use serde::Serialize;

// Default artifacts must reject these records on Dev too. Experimental
// artifacts still reject the same operations on Live.
const GATED_NETWORK: NetworkId = if cfg!(feature = "experimental-testnet") {
    NetworkId::Live
} else {
    NetworkId::Dev
};

#[test]
fn ordinary_library_rejects_signed_appeals_before_queueing_or_admission() {
    use burst_ledger::{
        genesis::{create_genesis_block, GenesisConfig},
        BlockType, DagFrontier,
    };
    use burst_node::block_processor::{BlockProcessor, ProcessResult};
    let key = burst_crypto::keypair_from_seed(&[23; 32]);
    let mut block = create_genesis_block(&GenesisConfig {
        network: NetworkId::Dev,
        creator: burst_crypto::derive_address(&key.public),
    });
    block.block_type = BlockType::Challenge;
    block.previous = BlockHash::new([1; 32]);
    block.payment_epoch = BlockHash::new([2; 32]);
    block.target_head = BlockHash::new([3; 32]);
    block.brn_balance = 100;
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &key.private);
    let mut processor = BlockProcessor::new(0, block.payment_epoch);
    assert_eq!(
        processor.process(&block, &mut DagFrontier::new()),
        ProcessResult::Rejected(
            "meta-challenge admission requires certified appeal support".into()
        )
    );
}

#[derive(Serialize)]
struct OutcomeRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    target: WalletAddress,
}

#[test]
fn ordinary_library_rejects_compact_emergencies_before_queueing_or_admission() {
    use burst_ledger::{
        genesis::{create_genesis_block, GenesisConfig},
        BlockType, DagFrontier,
    };
    use burst_node::block_processor::{BlockProcessor, ProcessResult};
    let key = burst_crypto::keypair_from_seed(&[23; 32]);
    let mut block = create_genesis_block(&GenesisConfig {
        network: NetworkId::Dev,
        creator: burst_crypto::derive_address(&key.public),
    });
    block.block_type = BlockType::GovernanceProposal;
    block.previous = BlockHash::new([1; 32]);
    block.payment_epoch = BlockHash::new([2; 32]);
    block.origin = burst_types::TxHash::ZERO;
    block.link = BlockHash::new(
        burst_governance::ProposalContent::Emergency {
            description: String::new(),
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 1,
        }
        .encode_compact()
        .unwrap(),
    );
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &key.private);
    let mut processor = BlockProcessor::new(0, block.payment_epoch);
    assert_eq!(
        processor.process(&block, &mut DagFrontier::new()),
        ProcessResult::Rejected("emergency governance is behind the release gate".into())
    );
}

#[test]
fn ordinary_library_rejects_outcome_records_before_import_or_recovery() {
    let params = ProtocolParams::default();
    let anchor = ReplayedCheckpoint::genesis_anchor(GATED_NETWORK, &params).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let record = OutcomeRecord {
        version: 2,
        certificate: CheckpointCertificate {
            checkpoint: Checkpoint {
                network: GATED_NETWORK,
                genesis: anchor.genesis_hash(),
                parent: BlockHash::new([1; 32]),
                height: 2,
                cutoff: 100,
                ledger_root: BlockHash::new([2; 32]),
                params_hash: params.params_hash(),
                next_roster: anchor.roster().commitment(),
                selection_root: BlockHash::new([3; 32]),
                next_quorum_bps: anchor.quorum_bps(),
            },
            round: 0,
            signatures: Vec::new(),
        },
        heads: Vec::new(),
        target: burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[20; 32]).public),
    };
    let bytes = bincode::serialize(&record).unwrap();
    let error = CertifiedChallengeOutcome::import_record(&store, GATED_NETWORK, &params, 2, &bytes)
        .err()
        .expect("ordinary library must keep outcome imports gated");
    assert!(
        error
            .to_string()
            .contains("mixed checkpoint records are behind the release gate"),
        "{error}"
    );
    assert!(CertifiedCheckpoint::load(&store, GATED_NETWORK, &params)
        .unwrap()
        .is_none());
    let prefix = format!(
        "checkpoint_chain/v1/{}/{}",
        GATED_NETWORK.as_str(),
        anchor.genesis_hash()
    );
    assert!(store.meta_store().get_meta(&format!("{prefix}/2")).is_err());

    // A test database copied into a normal build must fail recovery too.
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&format!("{prefix}/1"), &bytes).unwrap();
    batch
        .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
        .unwrap();
    batch.commit().unwrap();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let error = CertifiedCheckpoint::load(&store, GATED_NETWORK, &params)
        .err()
        .expect("ordinary recovery must reject outcome records");
    assert!(
        error
            .to_string()
            .contains("mixed checkpoint records are behind the release gate"),
        "{error}"
    );
    assert_eq!(
        store.meta_store().get_meta(&format!("{prefix}/1")).unwrap(),
        bytes
    );
}

#[test]
fn ordinary_library_rejects_roster_activation_and_deadline_records_before_recovery_mutates_the_store(
) {
    #[derive(Serialize)]
    struct RosterRecord {
        version: u8,
        certificate: CheckpointCertificate,
        heads: Vec<HistoryHead>,
    }
    #[derive(Serialize)]
    struct ActivationRecord {
        version: u8,
        certificate: CheckpointCertificate,
        heads: Vec<HistoryHead>,
        activation: burst_node::checkpoint_activation::ActivationProof,
    }
    let params = ProtocolParams::default();
    let anchor = ReplayedCheckpoint::genesis_anchor(GATED_NETWORK, &params).unwrap();
    let record = RosterRecord {
        version: 3,
        certificate: CheckpointCertificate {
            checkpoint: Checkpoint {
                network: GATED_NETWORK,
                genesis: anchor.genesis_hash(),
                parent: anchor.hash(),
                height: 1,
                cutoff: 100,
                ledger_root: BlockHash::new([2; 32]),
                params_hash: params.params_hash(),
                next_roster: anchor.roster().commitment(),
                selection_root: BlockHash::new(burst_crypto::blake2b_256(
                    b"BURST/roster-checkpoint/v1\0",
                )),
                next_quorum_bps: anchor.quorum_bps(),
            },
            round: 0,
            signatures: Vec::new(),
        },
        heads: Vec::new(),
    };
    let mut activation_certificate = record.certificate.clone();
    activation_certificate.checkpoint.height = 2;
    let activation = ActivationRecord {
        version: 4,
        certificate: activation_certificate,
        heads: Vec::new(),
        activation: burst_node::checkpoint_activation::ActivationProof {
            selection: BlockHash::new([4; 32]),
            value: [5; 32],
            proof: vec![],
            round: 1,
        },
    };
    for bytes in [
        bincode::serialize(&record).unwrap(),
        bincode::serialize(&activation).unwrap(),
        bincode::serialize(&RosterRecord {
            version: 5,
            certificate: activation.certificate.clone(),
            heads: Vec::new(),
        })
        .unwrap(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let prefix = format!(
            "checkpoint_chain/v1/{}/{}",
            GATED_NETWORK.as_str(),
            anchor.genesis_hash()
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&format!("{prefix}/1"), &bytes).unwrap();
        batch
            .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
            .unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let error = CertifiedCheckpoint::load(&store, GATED_NETWORK, &params)
            .err()
            .expect("ordinary recovery must reject gated events before certificate replay");
        assert!(
            error
                .to_string()
                .contains("mixed checkpoint records are behind the release gate"),
            "{error}"
        );
        assert_eq!(
            store.meta_store().get_meta(&format!("{prefix}/1")).unwrap(),
            bytes
        );
        assert_eq!(
            store
                .meta_store()
                .get_meta(&format!("{prefix}/head"))
                .unwrap(),
            1u64.to_be_bytes()
        );
    }
}
