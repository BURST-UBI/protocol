use super::*;
use crate::checkpoint_store::{chain, CertifiedCheckpoint};
use crate::confirmed_replay::snapshot::PreparedSnapshot;
use burst_consensus::snapshot::SnapshotCertificate;

fn encoded<K: serde::Serialize, V: serde::Serialize>(
    map: &HashMap<K, V>,
) -> std::collections::BTreeMap<Vec<u8>, Vec<u8>> {
    map.iter()
        .map(|(key, value)| {
            (
                bincode::serialize(key).unwrap(),
                bincode::serialize(value).unwrap(),
            )
        })
        .collect()
}

#[test]
fn authenticated_economic_snapshot_matches_independent_replays_and_restores_all_state() {
    let mut fixture = Fixture::selection();
    let grant = fixture.push(0, BlockType::ChangeSigner, 1_595_434_049, |block| {
        block.link = BlockHash::new([77; 32]);
    });
    fixture.push(0, BlockType::Epoch, 1_595_434_049, |_| {});
    fixture.push(1, BlockType::Epoch, 1_595_434_049, |_| {});
    sign(0, fixture.blocks.last_mut().unwrap());
    let foreign_marker = fixture.blocks.last().unwrap().clone();
    let (_dir, store, heads) = fixture.store();
    let cutoff = 1_595_434_050;
    let candidate = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        cutoff,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
    let record = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate,
        heads,
        address(20),
    )
    .unwrap();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let anchor = chain::next_anchor(&events, NetworkId::Dev, &fixture.params).unwrap();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let authority = burst_consensus::snapshot::SnapshotAuthorityStep {
        certificate: record.certificate().clone(),
        roster: anchor.roster().humans().to_vec(),
    };
    let imported_anchor = authority.authenticate(&genesis).unwrap();
    assert_eq!(imported_anchor.hash(), anchor.hash());
    assert_eq!(imported_anchor.roster(), anchor.roster());
    let mut forged = authority.clone();
    forged.roster.pop();
    assert!(forged.authenticate(&genesis).is_err());
    forged = authority.clone();
    forged.certificate.signatures.clear();
    assert!(forged.authenticate(&genesis).is_err());
    let snapshot = PreparedSnapshot::at_tip(&store, NetworkId::Dev, &fixture.params).unwrap();
    let certificate = SnapshotCertificate {
        manifest: snapshot.manifest().clone(),
        signatures: (0..7)
            .map(|seed| {
                snapshot
                    .sign(
                        &anchor,
                        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )
                    .unwrap()
            })
            .collect(),
    };
    use crate::confirmed_replay::snapshot::archive::AuthenticatedSnapshot;
    let mut archive = Vec::new();
    snapshot
        .write_archive(
            &genesis,
            std::slice::from_ref(&authority),
            &certificate,
            &mut archive,
        )
        .unwrap();
    let imported = AuthenticatedSnapshot::read(&genesis, &mut archive.as_slice()).unwrap();
    let staging = tempfile::tempdir().unwrap();
    let staged =
        AuthenticatedSnapshot::read_in(&genesis, &mut archive.as_slice(), staging.path()).unwrap();
    assert_eq!(staged.anchor().hash(), imported.anchor().hash());
    assert_eq!(staged.certificate(), imported.certificate());
    assert_eq!(
        staged
            .into_state()
            .prepare_snapshot(&anchor)
            .unwrap()
            .bytes(),
        snapshot.bytes()
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    assert_eq!(imported.anchor().hash(), anchor.hash());
    assert_eq!(imported.certificate(), &certificate);
    let mut cursor = imported.history_cursor(&address(1)).unwrap().unwrap();
    let requested = cursor.next().unwrap().hash;
    assert_eq!(requested, foreign_marker.hash);
    let mut wrong_signer = foreign_marker.clone();
    wrong_signer.signature = burst_crypto::sign_message(
        wrong_signer.hash.as_bytes(), &burst_crypto::keypair_from_seed(&[1; 32]).private,
    );
    assert!(cursor.accept(&[bincode::serialize(&wrong_signer).unwrap()]).is_err());
    assert_eq!(cursor.next().unwrap().hash, requested);
    let proof = cursor.accept(&[bincode::serialize(&foreign_marker).unwrap()]).unwrap();
    assert_eq!(proof[0].block().hash, foreign_marker.hash);
    assert_eq!(
        imported
            .into_state()
            .prepare_snapshot(&anchor)
            .unwrap()
            .bytes(),
        snapshot.bytes()
    );
    // A previously authenticated tip can be used directly, without trusting a
    // peer's advertised genesis or accepting a rollback behind the local base.
    let mut at_tip = Vec::new();
    snapshot
        .write_archive(&anchor, &[], &certificate, &mut at_tip)
        .unwrap();
    assert!(AuthenticatedSnapshot::read(&anchor, &mut at_tip.as_slice()).is_ok());
    assert!(
        AuthenticatedSnapshot::read_in(&anchor, &mut at_tip.as_slice(), staging.path()).is_ok()
    );
    assert!(
        AuthenticatedSnapshot::read_in(&genesis, &mut at_tip.as_slice(), staging.path()).is_err()
    );
    assert!(
        AuthenticatedSnapshot::read_in(&anchor, &mut archive.as_slice(), staging.path()).is_err()
    );
    assert!(AuthenticatedSnapshot::read(&genesis, &mut at_tip.as_slice()).is_err());
    assert!(AuthenticatedSnapshot::read(&anchor, &mut archive.as_slice()).is_err());
    let header_end = 24 + u64::from_be_bytes(archive[16..24].try_into().unwrap()) as usize;
    let mut invalid_header = archive.clone();
    invalid_header[header_end - 1] ^= 1;
    let mut reader = std::io::Cursor::new(invalid_header);
    let mut staged_reader = reader.clone();
    assert!(AuthenticatedSnapshot::read_in(&genesis, &mut staged_reader, staging.path()).is_err());
    assert!(
        staged_reader.position() <= header_end as u64,
        "invalid header must not consume state"
    );
    assert!(AuthenticatedSnapshot::read(&genesis, &mut reader).is_err());
    assert_eq!(
        reader.position(),
        header_end as u64,
        "reject bad signatures before consuming the state body"
    );
    for length in [0, 15, 23, header_end - 1, header_end, archive.len() - 1] {
        assert!(AuthenticatedSnapshot::read(&genesis, &mut &archive[..length]).is_err());
        assert!(
            AuthenticatedSnapshot::read_in(&genesis, &mut &archive[..length], staging.path())
                .is_err()
        );
    }
    let mut altered = archive.clone();
    altered[header_end + 10] ^= 1;
    assert!(AuthenticatedSnapshot::read(&genesis, &mut altered.as_slice()).is_err());
    assert!(
        AuthenticatedSnapshot::read_in(&genesis, &mut altered.as_slice(), staging.path()).is_err()
    );
    let mut appended = archive.clone();
    appended.push(0);
    assert!(AuthenticatedSnapshot::read(&genesis, &mut appended.as_slice()).is_err());
    assert!(
        AuthenticatedSnapshot::read_in(&genesis, &mut appended.as_slice(), staging.path()).is_err()
    );
    assert!(AuthenticatedSnapshot::read_in(
        &genesis,
        &mut archive.as_slice(),
        &staging.path().join("missing")
    )
    .is_err());
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    let mut minority_export = certificate.clone();
    minority_export.signatures.pop();
    let mut output = Vec::new();
    assert!(snapshot
        .write_archive(&genesis, &[authority], &minority_export, &mut output)
        .is_err());
    assert!(
        output.is_empty(),
        "reject invalid export before writing any bytes"
    );
    let restored =
        ConfirmedReplay::from_authenticated_snapshot(&anchor, &certificate, snapshot.bytes())
            .unwrap();
    assert_eq!(restored.account(&address(0)).unwrap().epoch, burst_ledger::CURRENT_BLOCK_VERSION);
    let legacy = restored.legacy_snapshot_bytes().unwrap();
    let mut manifest = burst_consensus::snapshot::SnapshotManifest::new(&anchor, &legacy).unwrap();
    manifest.state_format = burst_consensus::snapshot::LEGACY_STATE_FORMAT;
    let signatures = (0..7)
        .map(|seed| burst_consensus::checkpoint::CheckpointSignature {
            representative: address(seed),
            signature: burst_crypto::sign_message(
                &manifest.signing_payload(),
                &burst_crypto::keypair_from_seed(&[seed; 32]).private,
            ),
        })
        .collect();
    let legacy_certificate = SnapshotCertificate {
        manifest,
        signatures,
    };
    let legacy_restored =
        ConfirmedReplay::from_authenticated_snapshot(&anchor, &legacy_certificate, &legacy)
            .unwrap();
    assert_eq!(
        legacy_restored.snapshot_bytes().unwrap(),
        restored.snapshot_bytes().unwrap()
    );
    restored.assert_snapshot_encoding_compatibility();
    let disk = restored
        .prepare_snapshot_in(&anchor, staging.path())
        .unwrap();
    assert_eq!(disk.manifest(), snapshot.manifest());
    assert_eq!(
        disk.sign(&anchor, &burst_crypto::keypair_from_seed(&[0; 32]).private)
            .unwrap(),
        snapshot
            .sign(&anchor, &burst_crypto::keypair_from_seed(&[0; 32]).private)
            .unwrap()
    );
    for _ in 0..2 {
        let mut output = Vec::new();
        disk.write_archive_with_beacons(&anchor, &[][..], &certificate, &[], &mut output)
            .unwrap();
        assert_eq!(output, at_tip);
    }
    // A failed destination must not consume the private prepared body.
    assert!(disk
        .write_archive_with_beacons(&anchor, &[][..], &certificate, &[], &mut &mut [0u8; 24][..])
        .is_err());
    let mut output = Vec::new();
    disk.write_archive_with_beacons(&anchor, &[][..], &certificate, &[], &mut output)
        .unwrap();
    assert_eq!(output, at_tip);
    drop(disk);
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    assert!(restored
        .prepare_snapshot_in(&anchor, &staging.path().join("missing"))
        .is_err());
    assert_eq!(
        &restored.snapshot_manifest(&anchor).unwrap(),
        snapshot.manifest()
    );
    assert_eq!(
        restored.prepare_snapshot(&anchor).unwrap().bytes(),
        snapshot.bytes()
    );
    let base = fixture.history(cutoff);
    fixture.push(0, BlockType::Burn, cutoff + 31, |block| {
        block.payment_epoch = anchor.hash();
        block.link = destination(21);
        block.brn_balance += 1;
    });
    assert_eq!(fixture.blocks.last().unwrap().signing_grant, grant);
    let tail = fixture.history(cutoff + 31);
    let continued = restored.clone()
        .replay_snapshot_tail(
            &anchor,
            &record.certificate().checkpoint,
            &base,
            &tail,
            std::slice::from_ref(&record),
        )
        .unwrap();
    let valid_successor = fixture.blocks.last().unwrap().clone();
    let stale = fixture.blocks.last_mut().unwrap();
    stale.signing_grant = BlockHash::ZERO;
    sign(0, stale);
    let stale_tail = fixture.history(cutoff + 31);
    assert!(restored
        .replay_snapshot_tail(
            &anchor,
            &record.certificate().checkpoint,
            &base,
            &stale_tail,
            std::slice::from_ref(&record),
        )
        .is_err());
    *fixture.blocks.last_mut().unwrap() = valid_successor;
    let full = ConfirmedReplay::replay_with_certificates(
        &tail,
        NetworkId::Dev,
        &fixture.params,
        &[record],
    )
    .unwrap();
    assert_eq!(encoded(&continued.accounts), encoded(&full.accounts));
    assert_eq!(encoded(&continued.pending), encoded(&full.pending));
    assert_eq!(continued.brn.wallets, full.brn.wallets);
    assert_eq!(continued.brn.rate_history, full.brn.rate_history);
    assert_eq!(continued.brn.next_stake_id(), full.brn.next_stake_id());
    assert_eq!(continued.trst.save_wallets(), full.trst.save_wallets());
    assert_eq!(
        continued.trst.merger_graph.to_bytes(),
        full.trst.merger_graph.to_bytes()
    );
    assert_eq!(continued.lifecycle_metadata, full.lifecycle_metadata);
    for _ in 0..4 {
        let independent =
            PreparedSnapshot::at_tip(&store, NetworkId::Dev, &fixture.params).unwrap();
        assert_eq!(independent.manifest(), snapshot.manifest());
        assert_eq!(independent.bytes(), snapshot.bytes());
    }
    let mut minority = certificate.clone();
    minority.signatures.pop();
    assert!(
        ConfirmedReplay::from_authenticated_snapshot(&anchor, &minority, snapshot.bytes()).is_err()
    );
    let mut duplicate = certificate.clone();
    duplicate.signatures[6] = duplicate.signatures[0].clone();
    assert!(
        ConfirmedReplay::from_authenticated_snapshot(&anchor, &duplicate, snapshot.bytes())
            .is_err()
    );
    let mut bytes = snapshot.bytes().to_vec();
    let index = bytes.len() / 2;
    bytes[index] ^= 1;
    assert!(ConfirmedReplay::from_authenticated_snapshot(&anchor, &certificate, &bytes).is_err());
    for version in [0u8, 1, 2, 3, 4, 5, 6, 255] {
        use ciborium::value::Value;
        let mut body: Value = ciborium::from_reader(snapshot.bytes()).unwrap();
        let Value::Map(fields) = &mut body else {
            panic!("snapshot is not a map");
        };
        let (_, value) = fields
            .iter_mut()
            .find(|(key, _)| key.as_text() == Some("version"))
            .unwrap();
        *value = Value::Integer(version.into());
        let mut bytes = Vec::new();
        ciborium::into_writer(&body, &mut bytes).unwrap();
        let manifest = burst_consensus::snapshot::SnapshotManifest::new(&anchor, &bytes).unwrap();
        let signatures = (0..7)
            .map(|seed| burst_consensus::checkpoint::CheckpointSignature {
                representative: address(seed),
                signature: burst_crypto::sign_message(
                    &manifest.signing_payload(),
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                ),
            })
            .collect();
        let old = SnapshotCertificate {
            manifest,
            signatures,
        };
        old.verify_state(&anchor, &bytes).unwrap();
        assert!(
            ConfirmedReplay::from_authenticated_snapshot(&anchor, &old, &bytes)
                .err()
                .unwrap()
                .to_string()
                .contains("unsupported")
        );
    }
    assert!(snapshot
        .sign(&anchor, &burst_crypto::keypair_from_seed(&[99; 32]).private)
        .is_err());
    let mut substituted = certificate;
    substituted.manifest.roster = BlockHash::new([9; 32]);
    assert!(
        ConfirmedReplay::from_authenticated_snapshot(&anchor, &substituted, snapshot.bytes())
            .is_err()
    );
}
