use super::*;
use burst_consensus::{ActiveElections, VoteGenerator};
use tokio::sync::{Mutex, RwLock};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_human_verification_forks_require_quorum_and_survive_certified_recovery() {
    governance_journey(false, false, Scenario::VerificationFork).await;
}

type ElectionControls<'a> = (
    &'a [Arc<Mutex<VoteGenerator>>],
    &'a [Arc<RwLock<ActiveElections>>],
);

pub(super) async fn replace(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    registries: &[super::super::partition::Registry],
    (signers, elections): ElectionControls<'_>,
    keys: &[KeyPair],
    params: &ProtocolParams,
    at: u64,
) {
    let owner = burst_crypto::derive_address(&keys[0].public);
    for store in stores {
        let authority = PaymentAuthority::read(store).unwrap().unwrap();
        for key in keys {
            assert_eq!(
                authority
                    .roster
                    .weight(&burst_crypto::derive_address(&key.public)),
                1
            );
        }
    }
    let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[9; 32]).public);
    for endorsement_wins in [false, true] {
        eprintln!("verification fork: endorsement_wins={endorsement_wins}");
        let tx =
            Transaction::Endorse(build_endorse_tx(&owner, &target, 1, Timestamp::new(at)).unwrap());
        let endorse = signed_block(&clients[0], &keys[0], tx, params).await;
        let tx = Transaction::ChangeRepresentative(
            build_change_rep_tx(&owner, &owner, Timestamp::new(at)).unwrap(),
        );
        let preference = signed_block(&clients[0], &keys[0], tx, params).await;
        let (loser, winner) = if endorsement_wins {
            (preference, endorse)
        } else {
            (endorse, preference)
        };
        forks::replace(
            clients,
            stores,
            &forks::Network {
                registries,
                signers,
                elections,
            },
            keys,
            (&loser, &winner),
        )
        .await;
        for store in stores {
            let snapshot = burst_verification::OrchestratorSnapshot::decode(
                &store
                    .meta_store()
                    .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                    .unwrap(),
            )
            .unwrap();
            let verification = burst_verification::VerificationOrchestrator::restore(snapshot);
            assert_eq!(verification.get_state(&target).is_some(), endorsement_wins);
        }
        eprintln!("verification fork: replacement confirmed on every node");
    }
}
