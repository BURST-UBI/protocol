use super::*;
use crate::block_processor::ProcessResult;
use crate::checkpoint_store::{historical::HistoricalAdmissions, CertifiedCheckpoint};

const CUTOFF: u64 = 1_595_434_050;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u8,
    record: Vec<u8>,
    blocks: Vec<StateBlock>,
    contents: Vec<crate::confirmed_history::ProposalWitness>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    version: u8,
    certificate: burst_consensus::checkpoint::CheckpointCertificate,
    heads: Vec<HistoryHead>,
    target: WalletAddress,
    beacon: Option<burst_vrf::RandomOutput>,
}

#[test]
fn historical_transfer_authenticates_before_admission_and_does_not_install_peer_state() {
    let fixture = Fixture::selection();
    let (_source_dir, source, heads) = fixture.store();
    let prepared = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(prepared.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    let transfer =
        CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 1)
            .unwrap()
            .unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = LmdbStore::open(dest_dir.path(), 32, 32 << 20).unwrap();
    let admissions = std::sync::Arc::new(HistoricalAdmissions::default());
    let candidate = &fixture.blocks[1];

    // Arrival within the live window and authenticated historical admission
    // are different paths. A peer response alone cannot bridge this boundary.
    for age in [60, 61] {
        let mut live = BlockProcessor::new(0, fixture.blocks[0].hash);
        live.test_time = Some(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
            candidate.timestamp.as_secs() + age,
        )));
        let result = live.validate_candidate(candidate, &burst_ledger::DagFrontier::new());
        if age == 60 {
            assert_eq!(result, ProcessResult::Gap);
        } else {
            assert!(matches!(result, ProcessResult::Rejected(reason) if reason.contains("past")));
        }
    }

    let mut processor = BlockProcessor::new(0, fixture.blocks[0].hash);
    processor.test_time = Some(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
        CUTOFF + 86_400,
    )));
    processor.historical_admissions = admissions.clone();
    let frontier = burst_ledger::DagFrontier::new();
    assert!(matches!(processor.validate_candidate(candidate, &frontier),
        ProcessResult::Rejected(reason) if reason.contains("past")));
    for bytes in [&transfer[..transfer.len() - 1], &[0u8; 64][..]] {
        assert!(CertifiedCheckpoint::import_transfer(
            &dest,
            NetworkId::Dev,
            &fixture.params,
            1,
            bytes,
            &admissions
        )
        .is_err());
        assert!(!admissions.contains(&candidate.hash));
    }
    assert!(CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Test,
        &fixture.params,
        1,
        &transfer,
        &admissions
    )
    .is_err());
    assert!(CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Dev,
        &fixture.params,
        2,
        &transfer,
        &admissions
    )
    .is_err());
    assert!(!admissions.contains(&candidate.hash));

    let original: Envelope = bincode::deserialize(&transfer).unwrap();
    let candidate_index = original
        .blocks
        .iter()
        .position(|block| block.hash == candidate.hash)
        .unwrap();
    assert_ne!(
        original.blocks[candidate_index].signature,
        Signature([0; 64])
    );
    for mutation in 0..10 {
        let mut altered = original.clone();
        match mutation {
            0 => altered.blocks[candidate_index].signature = Signature([0; 64]),
            1 => {
                altered.blocks.pop();
            }
            2 => altered.blocks.push(altered.blocks[candidate_index].clone()),
            3 => altered.blocks[candidate_index].brn_balance += 1,
            4 => altered.version = 3,
            8 => altered
                .contents
                .push(crate::confirmed_history::ProposalWitness {
                    block: candidate.hash,
                    bytes: vec![0; 32],
                }),
            9 => altered.version = 1,
            _ => {
                let mut record: Record = bincode::deserialize(&altered.record).unwrap();
                match mutation {
                    5 => record.certificate.signatures.clear(),
                    6 => record.heads[0].height += 1,
                    _ => record.target = address(99),
                }
                altered.record = bincode::serialize(&record).unwrap();
            }
        }
        let bytes = bincode::serialize(&altered).unwrap();
        assert!(
            CertifiedCheckpoint::import_transfer(
                &dest,
                NetworkId::Dev,
                &fixture.params,
                1,
                &bytes,
                &admissions
            )
            .is_err(),
            "mutation {mutation}"
        );
        assert!(
            !admissions.contains(&candidate.hash),
            "mutation {mutation} authorized a block"
        );
        assert!(!dest.block_store().exists(&candidate.hash).unwrap());
    }

    // Certificate and signed history persist outside the applied ledger.
    // No account snapshot or economic state is installed by transport.
    CertifiedCheckpoint::import_transfer(
        &dest,
        NetworkId::Dev,
        &fixture.params,
        1,
        &transfer,
        &admissions,
    )
    .unwrap();
    assert!(admissions.contains(&candidate.hash));
    assert!(!dest.block_store().exists(&candidate.hash).unwrap());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&dest, NetworkId::Dev, &fixture.params).unwrap(),
        1
    );
    assert_eq!(
        processor.validate_candidate(candidate, &frontier),
        ProcessResult::Gap
    );
    CertifiedCheckpoint::import_transfer(
        &source,
        NetworkId::Dev,
        &fixture.params,
        1,
        &transfer,
        &admissions,
    )
    .unwrap();
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&source, NetworkId::Dev, &fixture.params).unwrap(),
        1
    );

    let mut demanding_work = BlockProcessor::new(u64::MAX, fixture.blocks[0].hash);
    demanding_work.historical_admissions = admissions.clone();
    assert!(
        matches!(demanding_work.validate_candidate(candidate, &frontier),
        ProcessResult::Rejected(reason) if reason.contains("proof-of-work"))
    );

    let mut forged = candidate.clone();
    forged.signature = Signature([0; 64]);
    assert!(matches!(processor.validate_candidate(&forged, &frontier),
        ProcessResult::Rejected(reason) if reason.contains("unsigned")));
    let mut backdated = candidate.clone();
    backdated.timestamp = Timestamp::new(candidate.timestamp.as_secs() + 1);
    sign(0, &mut backdated);
    assert!(
        matches!(processor.validate_candidate(&backdated, &frontier),
        ProcessResult::Rejected(reason) if reason.contains("past"))
    );

    processor
        .test_time
        .as_ref()
        .unwrap()
        .store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(processor.validate_candidate(candidate, &frontier),
        ProcessResult::Rejected(reason) if reason.contains("future")));

    assert!(dest
        .account_store()
        .get_account(&candidate.account)
        .is_err());
    drop(dest);
    let dest = LmdbStore::open(dest_dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(
        CertifiedCheckpoint::export_transfer(&dest, NetworkId::Dev, &fixture.params, 1)
            .unwrap()
            .unwrap(),
        transfer,
    );
    assert!(!dest.block_store().exists(&candidate.hash).unwrap());
    let key = format!("checkpoint_history/v1/{}", candidate.hash);
    let mut corrupt = candidate.clone();
    corrupt.signature = Signature([0; 64]);
    dest.meta_store()
        .put_meta(&key, &bincode::serialize(&corrupt).unwrap())
        .unwrap();
    assert!(CertifiedCheckpoint::load_chain(&dest, NetworkId::Dev, &fixture.params).is_err());
    assert!(!dest.block_store().exists(&candidate.hash).unwrap());
}
