use super::*;

#[test]
fn withdrawal_uses_signed_ancestry_for_same_second_support_only() {
    for (endorser, support_first) in [(0, true), (1, true), (0, false)] {
        let (mut fixture, proposals) = fixture_with_settings(
            10,
            1,
            |_| {},
            vec![ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 2,
            }],
        );
        fixture.blocks.retain(|block| {
            !matches!(
                block.block_type,
                BlockType::GovernanceEndorse | BlockType::GovernanceVote
            )
        });
        let link = BlockHash::new(*proposals[0].as_bytes());
        if !support_first {
            fixture.push(0, BlockType::GovernanceWithdraw, 111, |block| {
                block.link = link
            });
        }
        fixture.push(endorser, BlockType::GovernanceEndorse, 111, |block| {
            block.link = link;
            block.brn_spent += 1;
        });
        if support_first {
            fixture.push(0, BlockType::GovernanceWithdraw, 111, |block| {
                block.link = link
            });
        }
        let replay = fixture.replay(CUT - 1);
        if endorser == 0 && support_first {
            assert!(
                replay.is_ok(),
                "own-chain support before withdrawal failed: {:?}",
                replay.err()
            );
        } else {
            assert!(
                replay.is_err(),
                "ambiguous or later support cannot precede withdrawal"
            );
        }
    }
}

#[test]
fn final_support_and_later_payment_cannot_both_be_promised_in_either_order() {
    for (support_first, grouped) in [(false, false), (true, false), (false, true), (true, true)] {
        let (mut fixture, _) = fixture_with_cooldown(10);
        let support = fixture
            .blocks
            .iter()
            .find(|block| block.block_type == BlockType::GovernanceEndorse)
            .unwrap()
            .clone();
        fixture.blocks.retain(|block| {
            !matches!(
                block.block_type,
                BlockType::GovernanceEndorse | BlockType::GovernanceVote
            )
        });
        let prefix = fixture.blocks.clone();
        fixture.blocks.truncate(1);
        let (dir, store, _) = fixture.store();
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
        fixture.blocks = prefix;
        fixture.append_confirmed_prefix(&store, CUT - 1);
        let mut batch = store.tx_begin_write().unwrap();
        let mut schedule = crate::confirmed_governance::ConfirmedGovernance::load(&batch)
            .unwrap()
            .unwrap();
        for block in &fixture.blocks {
            schedule.promote(&batch, block).unwrap();
        }
        schedule.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let late = fixture.push(1, BlockType::Burn, CUT + 1, |block| {
            block.brn_spent += 1;
            block.link = destination(0);
        });
        for block in [&support, fixture.blocks.last().unwrap()] {
            store
                .meta_store()
                .put_fork_candidate(&block.hash, &bincode::serialize(block).unwrap())
                .unwrap();
        }
        let keys = burst_crypto::keypair_from_seed(&[0; 32]);
        let mut generator = burst_consensus::VoteGenerator::new(address(0), keys.private.0);
        if !support_first {
            crate::durable_votes::generate_vote(&store, &mut generator, support.hash, false)
                .unwrap();
        }
        let (first, second) = if support_first {
            (support.hash, late)
        } else {
            (late, support.hash)
        };
        let (first_vote, error) = if grouped {
            let mut results = crate::durable_votes::generate_final_votes(&store, &mut generator, &[first, second]).into_iter();
            (results.next().unwrap().unwrap(), results.next().unwrap().unwrap_err())
        } else {
            (crate::durable_votes::generate_vote(&store, &mut generator, first, true).unwrap(),
             crate::durable_votes::generate_vote(&store, &mut generator, second, true).unwrap_err())
        };
        assert!(
            error.to_string().contains(if support_first {
                "crosses a promised governance deadline"
            } else {
                "precedes an earlier final promise"
            }),
            "{error}"
        );
        let first_sequence = if support_first { 1 } else { 2 };
        assert_eq!(generator.sequence(), first_sequence);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
        assert!(crate::durable_votes::generate_vote(&store, &mut generator, second, true).is_err());
        if !support_first {
            let cached = crate::durable_votes::generate_vote(&store, &mut generator, first, true).unwrap();
            assert_eq!(cached.signature, first_vote.signature);
            assert_eq!(cached.sequence, first_sequence);
            assert_eq!(generator.sequence(), first_sequence);
        }
        // Restart the signer too: its sequence must recover from durable state.
        let mut generator = burst_consensus::VoteGenerator::new(address(0), keys.private.0);
        assert!(crate::durable_votes::generate_vote(&store, &mut generator, second, true).is_err());
        assert_eq!(generator.sequence(), 0);
        crate::durable_votes::generate_vote(&store, &mut generator, first, true).unwrap();
        assert_eq!(generator.sequence(), first_sequence + 1);
    }
}

#[test]
fn paid_but_unvoted_proposals_close_without_activation_and_recover() {
    for withdraw in [false, true] {
        let (mut fixture, proposals) = fixture_with_settings(
            10,
            1,
            |params| params.governance_proposal_endorsements = 2,
            vec![ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 2,
            }],
        );
        fixture
            .blocks
            .retain(|block| block.block_type != BlockType::GovernanceVote);
        if withdraw {
            fixture.push(0, BlockType::GovernanceWithdraw, 115, |block| {
                block.link = BlockHash::new(*proposals[0].as_bytes())
            });
        }
        let (dir, store, _) = fixture.store();
        if withdraw {
            let (_other_dir, other, _) = fixture.store();
            let roster =
                PreparedRoster::prepare_next(&other, NetworkId::Dev, &fixture.params, CUT - 1)
                    .unwrap()
                    .unwrap();
            let manifest = ReplayManifest::Roster(roster.manifest().clone());
            chain::accept_manifest(
                &other,
                NetworkId::Dev,
                &fixture.params,
                certified_votes::certify(manifest.checkpoint(), &[0]),
                &manifest,
            )
            .unwrap();
            PreparedInstallation::next(&other, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap()
                .commit(&other)
                .unwrap();
            assert!(PreparedGovernance::prepare_next(
                &other,
                NetworkId::Dev,
                &fixture.params,
                CUT + 10
            )
            .unwrap()
            .is_none());
        }
        let prepared =
            PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
                .unwrap()
                .unwrap();
        assert_eq!(prepared.manifest().checkpoint.cutoff, CUT);
        let manifest = ReplayManifest::Governance(prepared.manifest().clone());
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(manifest.checkpoint(), &[0]),
            &manifest,
        )
        .unwrap();
        let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
        let proposal = installed
            .governance
            .governance
            .get_proposal(&proposals[0])
            .unwrap();
        assert_eq!(
            proposal.phase,
            if withdraw {
                GovernancePhase::Withdrawn
            } else {
                GovernancePhase::Rejected
            }
        );
        assert!(proposal.activation_at.is_none());
        assert_eq!(
            installed.governance.params.params_hash(),
            fixture.params.params_hash()
        );
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
        assert!(PreparedGovernance::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUT + 10
        )
        .unwrap()
        .is_none());
    }
}
