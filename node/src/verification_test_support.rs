//! Component fixtures use actual pinned-mainnet drand proofs and independent
//! checkpoint signatures. They do not stand in for the unfinished wire service.

use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, CheckpointSignature, HumanDelegation,
    HumanRoster,
};
use burst_crypto::{derive_address, keypair_from_seed, sign_message};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp, WalletAddress};
use burst_verification::VerificationOrchestrator;
use burst_vrf::{DrandBeacon, DrandClient};

// The default fits a 1024-descriptor process. For two independent full meshes,
// raise that process's soft limit to 4096 before opting into parallel fixtures.
pub static NETWORK_FIXTURE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| {
        let value = match std::env::var("BURST_TEST_NETWORK_CONCURRENCY") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => panic!("invalid BURST_TEST_NETWORK_CONCURRENCY: {error}"),
        };
        tokio::sync::Semaphore::new(
            network_fixture_limit(value.as_deref())
                .expect("BURST_TEST_NETWORK_CONCURRENCY must be 1 or 2; use ulimit -n 4096 for 2"),
        )
    });

fn network_fixture_limit(value: Option<&str>) -> Result<usize, ()> {
    let limit = value
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| ())?
        .unwrap_or(1);
    if (1..=2).contains(&limit) {
        Ok(limit)
    } else {
        Err(())
    }
}

#[test]
fn network_fixture_concurrency_is_explicit_and_bounded() {
    assert_eq!(network_fixture_limit(None), Ok(1));
    for value in ["1", "2"] {
        assert_eq!(
            network_fixture_limit(Some(value)),
            Ok(value.parse().unwrap())
        );
    }
    for value in ["", "0", "3", "-1", "all"] {
        assert!(network_fixture_limit(Some(value)).is_err());
    }
}

pub fn time(tick: u64) -> Timestamp {
    // One logical test tick spans a beacon period, including challenge deadlines.
    Timestamp::new(1_595_431_050 + 30 * tick)
}

pub fn select(
    orchestrator: &mut VerificationOrchestrator,
    target: &WalletAddress,
    pool: &[WalletAddress],
    params: &ProtocolParams,
) -> BlockHash {
    let snapshot = orchestrator.snapshot();
    let state = orchestrator.get_state(target).unwrap();
    let cutoff = state.started_at.as_secs().max(
        snapshot
            .prior_votes
            .get(target)
            .into_iter()
            .flatten()
            .map(|vote| vote.timestamp.as_secs())
            .max()
            .unwrap_or(0),
    );
    let trigger = match (
        state.revote_count,
        orchestrator.active_challenge_context(target).unwrap(),
    ) {
        (0, Some(challenge)) => challenge,
        _ => BlockHash::new(burst_crypto::blake2b_256(&cutoff.to_be_bytes())),
    };
    let request = orchestrator
        .prepare_selection(target, trigger, Timestamp::new(cutoff), pool, params)
        .unwrap();
    let key = keypair_from_seed(&[90; 32]);
    let signer = derive_address(&key.public);
    let roster = HumanRoster::new(vec![HumanDelegation {
        human: signer.clone(),
        representative: signer.clone(),
    }])
    .unwrap();
    let checkpoint = Checkpoint {
        network: NetworkId::Dev,
        genesis: BlockHash::new([1; 32]),
        parent: BlockHash::new([1; 32]),
        height: 1,
        cutoff,
        ledger_root: BlockHash::new([2; 32]),
        params_hash: params.params_hash(),
        next_roster: roster.commitment(),
        next_quorum_bps: 6700,
        selection_root: request.commitment().unwrap(),
    };
    request
        .validate_before_signing(&checkpoint, cutoff, 0)
        .unwrap();
    let signature = sign_message(
        &burst_consensus::checkpoint_round::Ballot::precommit(&checkpoint, 0).signing_payload(),
        &key.private,
    );
    let parent =
        CheckpointAnchor::genesis(NetworkId::Dev, checkpoint.genesis, cutoff - 1, roster, 6700)
            .unwrap();
    let certificate = CheckpointCertificate {
        checkpoint,
        round: 0,
        signatures: vec![CheckpointSignature {
            representative: signer,
            signature,
        }],
    };
    // Downloaded from api.drand.sh/<pinned chain>/public/<round>; tests stay offline.
    let beacons: Vec<DrandBeacon> =
        serde_json::from_str(include_str!("../tests/fixtures/selection-beacons.json")).unwrap();
    let beacon = beacons
        .iter()
        .find(|beacon| beacon.round == request.round())
        .unwrap_or_else(|| panic!("missing real beacon fixture for round {}", request.round()));
    let output = DrandClient::beacon_to_random_output(beacon).unwrap();
    orchestrator
        .install_selection(target, &parent, &certificate, &output)
        .unwrap()
}
