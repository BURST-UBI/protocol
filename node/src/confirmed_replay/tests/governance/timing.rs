use super::*;

fn install(store: &LmdbStore, params: &ProtocolParams, cutoff: u64, signers: &[u8]) {
    let prepared = PreparedGovernance::prepare_next(store, NetworkId::Dev, params, cutoff)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.manifest().checkpoint.cutoff, cutoff);
    let manifest = ReplayManifest::Governance(prepared.manifest().clone());
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        params,
        certified_votes::certify(manifest.checkpoint(), signers),
        &manifest,
    )
    .unwrap();
    PreparedInstallation::next(store, NetworkId::Dev, params)
        .unwrap()
        .unwrap()
        .commit(store)
        .unwrap();
}

#[test]
fn shortening_voting_does_not_move_an_existing_count_behind_activation() {
    let (mut fixture, _) = fixture_with_settings(
        10,
        1,
        |_| {},
        vec![ProposalContent::ParameterChange {
            param: GovernableParam::GovernanceExplorationDurationSecs,
            new_value: 0,
        }],
    );
    let later = fixture.push(1, BlockType::GovernanceProposal, 140, |block| {
        block.link = BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 9,
            }
            .encode_compact()
            .unwrap(),
        );
    });
    fixture.push(0, BlockType::GovernanceEndorse, 140, |block| {
        block.link = later;
        block.brn_balance += 1;
    });
    let (_dir, store, _) = fixture.store();
    install(&store, &fixture.params, CUT, &[0]);
    let epoch = store.meta_store().payment_epoch().unwrap();
    fixture.push(0, BlockType::GovernanceVote, 151, |block| {
        block.payment_epoch = epoch;
        block.link = later;
        block.transaction = TxHash::ZERO;
    });
    fixture.append_confirmed_prefix(&store, 151);
    install(&store, &fixture.params, CUT + 11, &[0, 1, 2]);
    let authority =
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    let limit = burst_store::meta::decode_payment_vote_limit(
        &store
            .meta_store()
            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
            .unwrap(),
        authority.epoch,
    )
    .unwrap();
    assert_eq!(limit, Some(171));
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(160),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(replay.params().governance_exploration_duration_secs, 0);
    install(&store, &fixture.params, 171, &[0, 1, 2]);
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
}

#[test]
fn active_proposal_windows_survive_duration_and_buffer_changes() {
    for (param, value) in [
        (GovernableParam::GovernanceProposalDurationSecs, 0),
        (GovernableParam::GovernancePropagationBufferSecs, 0),
        (GovernableParam::GovernanceExplorationDurationSecs, 100),
    ] {
        let (mut fixture, _) = fixture_with_settings(
            10,
            1,
            |_| {},
            vec![ProposalContent::ParameterChange {
                param,
                new_value: value,
            }],
        );
        let (_dir, store, _) = fixture.store();
        install(&store, &fixture.params, CUT, &[0]);
        let epoch = store.meta_store().payment_epoch().unwrap();
        let later = fixture.push(1, BlockType::GovernanceProposal, 145, |block| {
            block.payment_epoch = epoch;
            block.link = BlockHash::new(
                ProposalContent::ParameterChange {
                    param: GovernableParam::BrnRate,
                    new_value: 9,
                }
                .encode_compact()
                .unwrap(),
            );
        });
        fixture.push(0, BlockType::GovernanceEndorse, 146, |block| {
            block.link = later;
            block.brn_balance += 1;
        });
        fixture.append_confirmed_prefix(&store, 151);
        install(&store, &fixture.params, 152, &[0, 1, 2]);
        let authority =
            crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
        assert_eq!(
            burst_store::meta::decode_payment_vote_limit(
                &store
                    .meta_store()
                    .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
                    .unwrap(),
                authority.epoch,
            )
            .unwrap(),
            Some(176)
        );
        let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let replay = ConfirmedReplay::replay_with_events(
            &fixture.history(153),
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        let params_hash = replay.params().params_hash();
        let governance = &replay.governance.as_ref().unwrap().governance;
        let proposal = governance.get_proposal(&later.into_tx_hash()).unwrap();
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(
            governance
                .counting_time(proposal, replay.params())
                .unwrap()
                .as_secs(),
            176
        );
        fixture.push(0, BlockType::GovernanceVote, 156, |block| {
            block.payment_epoch = authority.epoch;
            block.params_hash = params_hash;
            block.link = later;
            block.transaction = TxHash::ZERO;
        });
        fixture.append_confirmed_prefix(&store, 175);
        install(&store, &fixture.params, 176, &[0, 1, 2]);
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    }
}

#[test]
fn lowering_support_threshold_cannot_reopen_an_expired_proposal() {
    let (mut fixture, _) = fixture_with_settings(
        10,
        1,
        |_| {},
        vec![ProposalContent::ParameterChange {
            param: GovernableParam::GovernanceProposalEndorsements,
            new_value: 0,
        }],
    );
    let unsupported = fixture.push(1, BlockType::GovernanceProposal, 130, |block| {
        block.link = BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 9,
            }
            .encode_compact()
            .unwrap(),
        );
    });
    let (_dir, store, _) = fixture.store();
    install(&store, &fixture.params, CUT, &[0]);
    install(&store, &fixture.params, 152, &[0, 1, 2]);
    let authority =
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(
        burst_store::meta::decode_payment_vote_limit(
            &store
                .meta_store()
                .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
                .unwrap(),
            authority.epoch,
        )
        .unwrap(),
        None
    );
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(180),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(replay.params().governance_proposal_endorsements, 0);
    assert_eq!(
        replay
            .governance
            .as_ref()
            .unwrap()
            .governance
            .get_proposal(&unsupported.into_tx_hash())
            .unwrap()
            .phase,
        GovernancePhase::Rejected
    );
}
