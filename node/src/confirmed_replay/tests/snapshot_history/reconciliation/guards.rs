use super::*;
use crate::confirmed_replay::installation::{CUTOFF_KEY, INSTALLED_KEY};

#[test]
fn snapshot_reconciliation_rejects_inconsistent_local_authority_and_journals() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let genesis = fixture.blocks[0].hash;
    for case in 0..13 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local, _) = fixture.store();
        let meta = local.meta_store();
        let mut params = fixture.params.clone();
        let mut network = NetworkId::Dev;
        match case {
            0 => network = NetworkId::Test,
            1 => params.brn_rate += 1,
            2 => meta.put_meta("ledger_network", b"live").unwrap(),
            3 => {
                let mut wrong = params.clone();
                wrong.brn_rate += 1;
                meta.put_meta("genesis_params/v1", &bincode::serialize(&wrong).unwrap())
                    .unwrap();
            }
            4 => meta
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
                .unwrap(),
            5 => meta
                .put_meta(
                    &burst_store::meta::closed_payment_epoch_key(&genesis),
                    &[1; 64],
                )
                .unwrap(),
            6 => meta
                .put_meta(
                    &format!("checkpoint_chain/v1/dev/{genesis}/head"),
                    &1u64.to_be_bytes(),
                )
                .unwrap(),
            7 => {
                let events = chain::load(&source, NetworkId::Dev, &params).unwrap();
                chain::accept_manifest(
                    &local,
                    NetworkId::Dev,
                    &params,
                    events[0].certificate().clone(),
                    &events[0].manifest(),
                )
                .unwrap();
                meta.put_meta(&format!("checkpoint_chain/v1/dev/{genesis}/1"), b"damaged")
                    .unwrap();
            }
            8 => meta.put_meta(INSTALLED_KEY, b"unexpected").unwrap(),
            9 => meta.put_meta(CUTOFF_KEY, &1u64.to_be_bytes()).unwrap(),
            10 => meta
                .put_meta(
                    &format!("consensus_fence/v2/{genesis}/{}", address(0)),
                    &[99; 32],
                )
                .unwrap(),
            11 => meta
                .put_meta("consensus_final/v1/legacy", &[98; 32])
                .unwrap(),
            _ => meta
                .put_meta(
                    &format!("checkpoint_round/v1/dev/{genesis}/{}/1", address(0)),
                    b"damaged",
                )
                .unwrap(),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            snapshot
                .reconcile_installation(local.tx_begin_write().unwrap(), network, &params, &[])
                .is_err(),
            "case {case}"
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn snapshot_reconciliation_cannot_replace_a_different_accepted_checkpoint() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let (_dir, local, _) = fixture.store();
    let roster = PreparedRoster::prepare_next(
        &local,
        NetworkId::Dev,
        &fixture.params,
        certificate.checkpoint.cutoff - 2,
    )
    .unwrap()
    .unwrap();
    chain::accept_manifest(
        &local,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&roster.manifest().checkpoint, &[0]),
        &checkpoint_agreement::ReplayManifest::Roster(roster.manifest().clone()),
    )
    .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_chain/")
        .unwrap();
    let error = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[],
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("conflicts with an accepted"));
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"checkpoint_chain/")
            .unwrap(),
        before
    );
}

#[test]
fn snapshot_reconciliation_never_unlocks_or_rewrites_a_prepared_journal() {
    use crate::checkpoint_agreement::Outgoing;
    use burst_consensus::checkpoint_round::Quorum;
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let (_dir, local, _) = fixture.store();
    let roster = PreparedRoster::prepare_next(
        &local,
        NetworkId::Dev,
        &fixture.params,
        certificate.checkpoint.cutoff - 1,
    )
    .unwrap()
    .unwrap();
    let parent = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    let clock = Some(crate::checkpoint_replay::SigningClock {
        now: certificate.checkpoint.cutoff,
        error_bound: 0,
    });
    let outputs = checkpoint_agreement::apply_candidates(
        &local,
        &parent,
        &key.private,
        Command::Propose(roster.manifest().checkpoint.clone()),
        &[ReplayInput::Roster(&roster)],
        clock,
    )
    .unwrap();
    let proposal = outputs
        .iter()
        .find_map(|output| match output {
            Outgoing::Proposal(proposal) => Some(proposal.as_ref()),
            _ => None,
        })
        .unwrap();
    checkpoint_agreement::apply_candidates(
        &local,
        &parent,
        &key.private,
        Command::Proposal(proposal),
        &[ReplayInput::Roster(&roster)],
        clock,
    )
    .unwrap();
    let state = checkpoint_agreement::read_state(&local, &parent, &address(0))
        .unwrap()
        .unwrap();
    let ballot = state.prevote().unwrap().clone();
    let proof = Quorum {
        signatures: vec![burst_consensus::checkpoint::CheckpointSignature {
            representative: address(0),
            signature: burst_crypto::sign_message(&ballot.signing_payload(), &key.private),
        }],
        ballot,
    };
    checkpoint_agreement::apply_candidates(
        &local,
        &parent,
        &key.private,
        Command::Prepared(proposal, &proof),
        &[ReplayInput::Roster(&roster)],
        None,
    )
    .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_round/")
        .unwrap();
    assert!(
        checkpoint_agreement::read_state(&local, &parent, &address(0))
            .unwrap()
            .unwrap()
            .precommit()
            .is_some()
    );
    let checked = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[],
        )
        .unwrap();
    drop(checked);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"checkpoint_round/")
            .unwrap(),
        before
    );
    assert!(
        checkpoint_agreement::read_state(&local, &parent, &address(0))
            .unwrap()
            .unwrap()
            .precommit()
            .is_some()
    );
}

#[test]
fn snapshot_reconciliation_cannot_reinstall_or_rewind_an_installed_epoch() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    while let Some(next) =
        PreparedInstallation::next(&source, NetworkId::Dev, &fixture.params).unwrap()
    {
        next.commit(&source).unwrap();
    }
    let before = source.meta_store().payment_epoch().unwrap();
    let error = snapshot
        .reconcile_installation(
            source.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[],
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("does not advance"));
    assert_eq!(source.meta_store().payment_epoch().unwrap(), before);
    let old = authenticate(&source, &fixture.params, certificate.checkpoint.parent);
    assert!(old
        .reconcile_installation(
            source.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[]
        )
        .is_err());
    assert_eq!(source.meta_store().payment_epoch().unwrap(), before);
}
