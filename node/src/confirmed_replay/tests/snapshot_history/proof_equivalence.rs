use super::*;
use crate::{
    checkpoint_activation::PreparedActivation, checkpoint_agreement::ReplayManifest,
    checkpoint_store::chain, confirmed_replay::snapshot::PreparedSnapshot,
};

#[test]
fn economic_snapshot_is_independent_of_equivalent_selection_quorum_proofs() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let mut expected: Option<(BlockHash, Vec<u8>)> = None;
    for case in 0..5 {
        let (_dir, store, _) = fixture.store();
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            events[0].certificate().clone(),
            &events[0].manifest(),
        )
        .unwrap();
        let mut certificate = certificate.clone();
        match case {
            1 => certificate.signatures.reverse(),
            2 => {
                certificate = certified_votes::certify(&manifest.checkpoint, &[2, 3, 4, 5, 6, 7, 8])
            }
            3 => {
                certificate =
                    certified_votes::certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6, 7, 8])
            }
            4 => {
                certificate.round = 3;
                for (seed, signature) in certificate.signatures.iter_mut().enumerate() {
                    signature.signature = burst_crypto::sign_message(
                        &burst_consensus::checkpoint_round::Ballot::precommit(
                            &certificate.checkpoint,
                            certificate.round,
                        )
                        .signing_payload(),
                        &burst_crypto::keypair_from_seed(&[seed as u8; 32]).private,
                    );
                }
            }
            _ => {}
        }
        let mut record = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate,
            manifest.heads.clone(),
            address(20),
        )
        .unwrap();
        record
            .attach_beacon(&store, &fixture.params, beacon.clone())
            .unwrap();
        let activation = PreparedActivation::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            manifest.checkpoint.cutoff + 31,
        )
        .unwrap()
        .unwrap();
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&activation.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
            &ReplayManifest::Activation(activation.manifest().clone()),
        )
        .unwrap();
        let snapshot = PreparedSnapshot::at_tip(&store, NetworkId::Dev, &fixture.params).unwrap();
        let epoch = activation.manifest().checkpoint.hash();
        if let Some((expected_epoch, bytes)) = &expected {
            assert_eq!(epoch, *expected_epoch);
            assert!(
                snapshot.bytes() == bytes,
                "equivalent proof changed economic state in case {case}"
            );
        } else {
            expected = Some((epoch, snapshot.bytes().to_vec()));
        }
        let restored = authenticate(&store, &fixture.params, epoch);
        assert_eq!(restored.recover_selections().unwrap().len(), 1);
    }
}
