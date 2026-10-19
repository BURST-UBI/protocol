use super::*;
use crate::{
    checkpoint_agreement::{self, Command, ReplayInput, ReplayManifest},
    checkpoint_replay::CheckpointManifest,
    checkpoint_roster::PreparedRoster,
    checkpoint_store::chain,
};
use burst_consensus::checkpoint::CheckpointCertificate;

const CUTOFF: u64 = 1_595_434_050;

mod compact_discovery;
mod compact_persistence;
mod compact_refresh;
pub(in crate::confirmed_replay::tests) mod compact_service;
mod compact_signing;

pub(super) fn setup() -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CheckpointManifest,
    CheckpointCertificate,
) {
    setup_with(|_| {})
}

pub(super) fn setup_with(
    configure: impl FnOnce(&mut ProtocolParams),
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CheckpointManifest,
    CheckpointCertificate,
) {
    let fixture = Fixture::selection_with(configure);
    let (dir, store, heads) = fixture.store();
    let roster = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF - 1)
        .unwrap()
        .unwrap();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&roster.manifest().checkpoint, &[0]),
        &ReplayManifest::Roster(roster.manifest().clone()),
    )
    .unwrap();
    let replay = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    let manifest = replay.manifest();
    let certificate = certified_votes::certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate.clone(),
        heads,
        address(20),
    )
    .unwrap();
    (fixture, dir, store, manifest, certificate)
}

#[test]
fn compact_selection_checks_installed_authority_and_finality_without_writes() {
    use crate::confirmed_replay::installation::{CUTOFF_KEY, INSTALLED_KEY};
    let (fixture, _dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let coverage = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    for case in 0..9 {
        let (_local_dir, local) = cemented::sparse(&base, &source);
        match case {
            0 => (),
            1 => local
                .meta_store()
                .put_meta(INSTALLED_KEY, b"changed")
                .unwrap(),
            2 => local
                .meta_store()
                .put_meta(CUTOFF_KEY, &0u64.to_be_bytes())
                .unwrap(),
            3 => local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY, b"changed")
                .unwrap(),
            4 => local
                .meta_store()
                .put_meta(
                    &burst_store::meta::closed_payment_epoch_key(&base.anchor().hash()),
                    b"closed",
                )
                .unwrap(),
            5 | 6 => {
                let mut batch = local.tx_begin_write().unwrap();
                let mut account = batch.get_account(&address(0)).unwrap();
                if case == 5 {
                    account.confirmation_height -= 1;
                } else {
                    account.head = BlockHash::new([99; 32]);
                }
                // Fault injection bypasses the normal writer, which correctly
                // refuses to lower an existing confirmation height.
                batch
                    .put_account(&account.address, &bincode::serialize(&account).unwrap())
                    .unwrap();
                batch.commit().unwrap();
            }
            7 => local
                .meta_store()
                .enqueue_confirmation(&BlockHash::new([99; 32]))
                .unwrap(),
            _ => local
                .meta_store()
                .enqueue_confirmation(&base.state().account(&address(0)).unwrap().head)
                .unwrap(),
        }
        let batch = local.tx_begin_write().unwrap();
        let metadata = batch.checkpoint_metadata(b"").unwrap();
        let accounts = bincode::serialize(&batch.bounded_accounts(131_072).unwrap()).unwrap();
        let result = coverage.check_installed_cut(&batch);
        assert_eq!(
            result.is_ok(),
            case == 0 || case == 8,
            "case {case}: {result:?}"
        );
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), metadata);
        assert_eq!(
            bincode::serialize(&batch.bounded_accounts(131_072).unwrap()).unwrap(),
            accounts
        );
    }
}

#[test]
fn compact_selection_vote_coverage_rechecks_promises_without_fencing() {
    let (fixture, _dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let old = &fixture.blocks[0];
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[old.hash]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let coverage = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    assert_eq!(coverage.replay().manifest(), manifest);
    assert!(coverage.replay().validate_parent(base.anchor()).is_err());
    let epoch = base.anchor().hash();
    let representative = address(0);
    suffix
        .meta_store()
        .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes())
        .unwrap();
    let head = base.state().account(&address(0)).unwrap().head;
    let block: StateBlock =
        bincode::deserialize(&source.block_store().get_block(&head).unwrap()).unwrap();
    suffix
        .meta_store()
        .reserve_consensus_vote(
            &epoch,
            &representative,
            &block.account,
            &block.previous,
            &head,
            true,
            0,
        )
        .unwrap();
    assert_ne!(head, old.hash);
    suffix
        .meta_store()
        .reserve_consensus_vote(
            &epoch,
            &representative,
            &old.account,
            &old.previous,
            &old.hash,
            true,
            0,
        )
        .unwrap();
    let before = suffix
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    coverage
        .check_local_promises(&suffix.tx_begin_write().unwrap(), &representative)
        .unwrap();
    assert_eq!(
        suffix
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    // A successful preflight has not frozen voting. A later promise invalidates
    // this cut, and checking the same coverage again must observe that promise.
    suffix
        .meta_store()
        .reserve_consensus_vote(
            &epoch,
            &representative,
            &address(99),
            &BlockHash::ZERO,
            &BlockHash::new([99; 32]),
            true,
            0,
        )
        .unwrap();
    let before = suffix
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let error = coverage
        .check_local_promises(&suffix.tx_begin_write().unwrap(), &representative)
        .unwrap_err();
    assert!(error.to_string().contains("omits a durable final vote"));
    assert_eq!(
        suffix
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    suffix
        .meta_store()
        .put_meta(
            burst_store::meta::PAYMENT_EPOCH_KEY,
            BlockHash::new([88; 32]).as_bytes(),
        )
        .unwrap();
    let error = coverage
        .check_local_promises(&suffix.tx_begin_write().unwrap(), &representative)
        .unwrap_err();
    assert!(error.to_string().contains("not installed"));
}

#[test]
fn compact_selection_proposal_replay_preserves_base_and_rejects_forged_commitments() {
    let (fixture, _dir, source, manifest, _) = setup();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    for case in 0..8 {
        let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
        let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
        let base = snapshot.into_continuation();
        let before = base.state().snapshot_bytes().unwrap();
        let tail = base
            .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
            .unwrap();
        let mut proposed = manifest.clone();
        match case {
            0 => (),
            1 => proposed.checkpoint.selection_root = BlockHash::new([99; 32]),
            2 => proposed.checkpoint.params_hash = BlockHash::new([99; 32]),
            3 => proposed.target = address(31),
            4 => proposed.checkpoint.parent = BlockHash::new([99; 32]),
            5 => proposed.heads.pop().map(|_| ()).unwrap(),
            6 => proposed.checkpoint.cutoff += 1,
            _ => proposed.checkpoint.next_roster = BlockHash::new([99; 32]),
        }
        let result = base.replay_selection_candidate(tail, &proposed, &[]);
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        if let Ok(replay) = result {
            assert_eq!(replay.manifest(), manifest);
            assert!(replay.history_hashes().is_err());
            assert!(replay.validate_parent(base.anchor()).is_err());
        }
        assert_eq!(
            base.state().snapshot_bytes().unwrap(),
            before,
            "case {case}"
        );
        assert_eq!(base.anchor().hash(), manifest.checkpoint.parent);
        assert!(suffix
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"checkpoint_round/")
            .unwrap()
            .is_empty());
    }
}

#[test]
fn compact_selection_derives_the_request_but_cannot_sign_or_claim_complete_history() {
    let (fixture, _dir, store, manifest, certificate) = setup();
    boundaries::assert_chain(&fixture, &store);
    let snapshot = authenticate(&store, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &store, &[]);
    let base = snapshot.into_continuation();
    let (_tail_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let (base, mut recovered) = base
        .replay_selection(tail, &certificate, &manifest, &[])
        .unwrap();
    assert!(recovered.replayed().history_hashes().is_err());
    let parent = recovered.parent().clone();
    let signer = (0..7)
        .find(|seed| address(*seed) == *parent.proposer(0))
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[signer; 32]);
    assert!(
        checkpoint_agreement::read_state(&suffix, &parent, &address(signer))
            .unwrap()
            .is_none()
    );
    for command in [
        Command::Propose(manifest.checkpoint.clone()),
        Command::Decide(certificate.clone()),
    ] {
        let error = checkpoint_agreement::apply_candidates(
            &suffix,
            &parent,
            &key.private,
            command,
            &[ReplayInput::Selection(recovered.replayed())],
            Some(crate::checkpoint_replay::SigningClock {
                now: CUTOFF,
                error_bound: 0,
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("complete history inventory"));
    }
    assert!(
        checkpoint_agreement::read_state(&suffix, &parent, &address(signer))
            .unwrap()
            .is_none()
    );
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    for case in 0..4 {
        let mut wrong = beacon.clone();
        match case {
            0 => wrong.round += 1,
            1 => wrong.proof[0] ^= 1,
            2 => wrong.value[0] ^= 1,
            _ => wrong.proof.resize(4097, 0),
        }
        assert!(recovered.attach_compact_beacon(wrong).is_err());
        assert!(recovered.beacon().is_none());
    }
    recovered.attach_compact_beacon(beacon.clone()).unwrap();
    recovered.attach_compact_beacon(beacon).unwrap();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let full = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    compact::assert_same(base.state(), &full);

    // A later height cannot certify the same target's frozen attempt again.
    let snapshot = authenticate(&store, &fixture.params, certificate.checkpoint.hash());
    let witnesses = compact::retained(&snapshot, &fixture, &store, &[]);
    let base = snapshot.into_continuation();
    let tail = base
        .read_tail(&suffix, CUTOFF + 1, &manifest.heads, witnesses)
        .unwrap();
    let mut duplicate = manifest;
    duplicate.checkpoint.parent = certificate.checkpoint.hash();
    duplicate.checkpoint.height += 1;
    duplicate.checkpoint.cutoff += 1;
    // The account-head commitment also binds the cutoff.
    duplicate.checkpoint.ledger_root = tail.commitment();
    let certificate = certified_votes::certify(&duplicate.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    let error = base
        .replay_selection(tail, &certificate, &duplicate, &[&recovered])
        .err()
        .unwrap();
    assert!(error.to_string().contains("already frozen"), "{error}");
}

#[test]
fn compact_selection_rejects_quorum_target_cut_and_commitment_substitution() {
    let (fixture, _dir, store, manifest, _) = setup();
    let authority = store.meta_store().payment_epoch().unwrap();
    let (_tail_dir, suffix) = suffix_store(&[]);
    for case in 0..10 {
        let snapshot = authenticate(&store, &fixture.params, manifest.checkpoint.parent);
        let witnesses = compact::retained(&snapshot, &fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let tail = base
            .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
            .unwrap();
        let mut forged = manifest.clone();
        match case {
            1 => forged.target = address(21),
            2 => forged.checkpoint.selection_root = BlockHash::new([71; 32]),
            3 => forged.checkpoint.params_hash = BlockHash::new([72; 32]),
            4 => forged.checkpoint.next_roster = BlockHash::new([73; 32]),
            5 => forged.checkpoint.cutoff += 1,
            6 => forged.checkpoint.ledger_root = BlockHash::new([74; 32]),
            7 => forged.checkpoint.next_quorum_bps = 10_000,
            8 => {
                forged.heads.pop();
            }
            _ => {}
        }
        let mut certificate = certified_votes::certify(&forged.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
        if case == 0 {
            certificate.signatures.clear();
        }
        if case == 9 {
            forged.checkpoint.parent = BlockHash::new([75; 32]);
        }
        assert!(
            base.replay_selection(tail, &certificate, &forged, &[])
                .is_err(),
            "case {case}"
        );
        assert_eq!(store.meta_store().payment_epoch().unwrap(), authority);
    }
}
