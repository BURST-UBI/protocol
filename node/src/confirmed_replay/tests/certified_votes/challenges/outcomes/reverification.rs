use super::*;
use crate::checkpoint_store::chain as events;

#[test]
fn a_settled_identity_starts_a_new_certified_lifecycle_from_new_endorsements() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, first, _) = pending_with(reason, |params| {
            params.trst_expiry_secs = u64::MAX;
        });
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        for voter in panel(&second) {
            vote_choice(&mut fixture, &second, voter, CUTOFF + 60, 2);
        }
        merged_payments(&mut fixture);
        let (_dir, store, heads) = restore_chain(&fixture, &[first, second]);
        let outcome = PreparedChallengeOutcome::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70,
            &heads,
            &address(20),
        )
        .unwrap();
        let certificate = certify(&outcome.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
        let outcome = outcome
            .certify(certificate)
            .unwrap()
            .persist(&store, NetworkId::Dev, &fixture.params)
            .unwrap();
        let epoch = outcome.certificate().checkpoint.hash();
        let prefix = events::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let beacon_time = burst_vrf::DrandClient::new()
            .chain_info()
            .unwrap()
            .time_of_round(1005)
            .unwrap();
        let cutoff = beacon_time - 1;
        let original_state = outcome.state().account(&address(20)).unwrap().state;
        fixture.push(1, BlockType::Endorse, cutoff - 1, |block| {
            block.payment_epoch = epoch;
            block.link = destination(20);
            block.brn_balance += 1;
        });
        let partial = ConfirmedReplay::replay_with_events(
            &fixture.history(cutoff - 1),
            NetworkId::Dev,
            &fixture.params,
            &prefix,
        )
        .unwrap();
        assert_eq!(
            partial
                .verification(&address(20))
                .unwrap()
                .endorsements
                .len(),
            1
        );
        assert!(!partial.selection_targets().contains(&address(20)));
        assert_eq!(partial.account(&address(20)).unwrap().state, original_state);
        for holder in [1, 8] {
            assert_eq!(
                active_trst(&partial, holder),
                if reason == ChallengeReason::Fraud {
                    5_001
                } else {
                    10_001
                }
            );
        }
        let trigger = fixture.push(2, BlockType::Endorse, cutoff, |block| {
            block.payment_epoch = epoch;
            block.link = destination(20);
            block.brn_balance += 1;
        });
        fixture.append_confirmed_prefix(&store, cutoff);
        let candidate =
            CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, cutoff)
                .unwrap()
                .unwrap();
        assert_eq!(candidate.request().attempt(), 0);
        assert_eq!(candidate.request().trigger(), trigger);
        assert_eq!(candidate.request().round(), 1005);
        assert_eq!(
            candidate.lifecycle(),
            SelectionLifecycle::Verification(trigger)
        );
        let mut selection = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(candidate.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
            candidate.manifest().heads,
            address(20),
        )
        .unwrap();
        let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/selection-beacons.json"
        )))
        .unwrap();
        selection
            .attach_beacon(
                &store,
                &fixture.params,
                burst_vrf::DrandClient::beacon_to_random_output(&beacons[1]).unwrap(),
            )
            .unwrap();
        let mut all = events::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let voters = panel(&selection);
        assert_eq!(voters.len(), 2);
        vote_choice(&mut fixture, &selection, voters[0], beacon_time + 1, 1);
        let incomplete = ConfirmedReplay::replay_with_events(
            &fixture.history(beacon_time + 1),
            NetworkId::Dev,
            &fixture.params,
            &all,
        )
        .unwrap();
        assert_eq!(
            incomplete.account(&address(20)).unwrap().state,
            original_state
        );
        assert_eq!(incomplete.human_roster().unwrap().weight(&address(20)), 0);
        let prefix_len = fixture.blocks.len();
        fixture.push(20, BlockType::Burn, beacon_time + 2, |block| {
            block.payment_epoch = selection.certificate().checkpoint.hash();
            block.link = destination(8);
            block.brn_balance += 1;
        });
        assert!(
            ConfirmedReplay::replay_with_events(
                &fixture.history(beacon_time + 2),
                NetworkId::Dev,
                &fixture.params,
                &all,
            )
            .is_err(),
            "an incomplete fresh panel cannot restore spending rights"
        );
        fixture.blocks.truncate(prefix_len);
        for voter in voters.into_iter().skip(1) {
            vote_choice(&mut fixture, &selection, voter, beacon_time + 1, 1);
        }
        let records: Vec<_> = all
            .iter()
            .filter_map(events::CertifiedEvent::selection)
            .collect();
        let bare = selections::Selections::new(&records, &fixture.blocks);
        assert!(matches!(bare, Err(error) if error.to_string().contains(
            "re-verification requires agreed lifecycle replay"
        )));
        let verified = ConfirmedReplay::replay_with_events(
            &fixture.history(beacon_time + 1),
            NetworkId::Dev,
            &fixture.params,
            &all,
        )
        .unwrap();
        assert!(verified.account(&address(20)).unwrap().state.is_verified());
        assert_eq!(verified.human_roster().unwrap().weight(&address(20)), 1);
        assert_eq!(
            verified
                .verification(&address(20))
                .unwrap()
                .endorsements
                .len(),
            2
        );
        assert!(!verified.verification.has_active_challenge(&address(20)));
        for holder in [1, 8] {
            assert_eq!(
                active_trst(&verified, holder),
                10_001,
                "restored holder {holder}"
            );
        }
        let new_payment = fixture.push(20, BlockType::Burn, beacon_time + 2, |block| {
            block.payment_epoch = selection.certificate().checkpoint.hash();
            block.link = destination(8);
            block.brn_balance += 1;
        });
        let spent = ConfirmedReplay::replay_with_events(
            &fixture.history(beacon_time + 2),
            NetworkId::Dev,
            &fixture.params,
            &all,
        )
        .unwrap();
        assert_eq!(
            spent
                .pending(&address(8), new_payment.into_tx_hash())
                .unwrap()
                .amount,
            1
        );
        all.swap(2, 3);
        assert!(
            ConfirmedReplay::replay_with_events(
                &fixture.history(beacon_time + 2),
                NetworkId::Dev,
                &fixture.params,
                &all,
            )
            .is_err(),
            "a fresh selection cannot precede the certified revocation"
        );
        all.retain(|event| !matches!(event, events::CertifiedEvent::Outcome(_)));
        assert!(
            ConfirmedReplay::replay_with_events(
                &fixture.history(beacon_time + 2),
                NetworkId::Dev,
                &fixture.params,
                &all,
            )
            .is_err(),
            "a missing revocation must not authorize a fresh lifecycle"
        );
    }
}

fn active_trst(state: &ConfirmedReplay, holder: u8) -> u128 {
    state
        .portfolio(&address(holder))
        .unwrap()
        .tokens
        .iter()
        .filter(|token| token.state == burst_types::TrstState::Active)
        .map(|token| token.amount)
        .sum()
}
