use super::*;

fn hash(n: u8) -> BlockHash { BlockHash::new([n; 32]) }
fn address(n: u8) -> WalletAddress { WalletAddress::new(format!("brst_retention_{n}")) }

#[test]
fn derived_empty_deadline_outcome_is_not_a_signed_block_reference() {
    let source = BlockHash::new(burst_crypto::blake2b_256_multi(&[
        b"BURST/empty-deadline-outcome/v1\0", hash(1).as_bytes(), &9u64.to_be_bytes(),
    ]));
    let receipt = receipts::Receipt { source, target: address(2), completed_at: Timestamp::new(9),
        previous: None, verdict: receipts::Verdict::Failed, rewards: vec![] };
    let mut outcome = false;
    visit_outcome(&receipts::key(source), &bincode::serialize(&receipt).unwrap(), |r| {
        assert_ne!(r, Reference::Block(source));
        outcome |= r == Reference::Outcome(source);
        Ok(())
    }).unwrap();
    assert!(outcome);
}

#[test]
fn lifecycle_links_keep_source_sets_identity_bindings_and_checkpoint_namespace() {
    let owner = address(1);
    let b = |n| format!("{:?}", Reference::Block(hash(n)));
    let a = format!("{:?}", Reference::Account(&owner));
    let cases = [
        (format!("verification_outcome_sources/v1/{}", hash(2)), bincode::serialize(&vec![hash(1), hash(2)]).unwrap(), vec![format!("{:?}", Reference::Outcome(hash(2))), b(1), b(2)]),
        (format!("verification_transition_sources/v1/{}", hash(1)), bincode::serialize(&vec![hash(1), hash(2)]).unwrap(), vec![b(1), b(1), b(2)]),
        (format!("verification_identity_source/v1/{owner}"), hash(3).as_bytes().to_vec(), vec![a.clone(), b(3)]),
        (format!("verification_identity_bound/v1/{owner}"), hash(3).as_bytes().to_vec(), vec![a.clone(), b(3)]),
        (format!("verification_latest_outcome/v1/{owner}"), hash(4).as_bytes().to_vec(), vec![a, format!("{:?}", Reference::Outcome(hash(4)))]),
        (format!("verification_identity_binding/v1/{}", hash(2)), hash(3).as_bytes().to_vec(), vec![b(2), b(3)]),
        (format!("verification_certified_outcome/v1/{}", hash(4)), hash(9).as_bytes().to_vec(), vec![format!("{:?}", Reference::Outcome(hash(4))), format!("{:?}", Reference::Checkpoint(hash(9)))]),
    ];
    for (key, bytes, expected) in cases {
        let mut actual = Vec::new();
        visit_row(&key, &bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
        assert_eq!(actual, expected);
        for stop in 1..=expected.len() {
            let mut calls = 0;
            assert!(visit_row(&key, &bytes, |_| {
                calls += 1;
                if calls == stop { Err(invalid("stop")) } else { Ok(()) }
            }).is_err());
            assert_eq!(calls, stop);
        }
        for bad_key in [key.replace("/v1/", "/v9/"), format!("{key}/extra")] {
            assert!(visit_row(&bad_key, &bytes, |_| panic!("bad key emitted roots")).is_err());
        }
        let mut trailing = bytes.clone(); trailing.push(0);
        assert!(visit_row(&key, &trailing, |_| panic!("trailing row emitted roots")).is_err());
    }
    let key = format!("verification_transition_sources/v1/{}", hash(2));
    for sources in [vec![], vec![hash(1)], vec![hash(2), hash(2)], vec![hash(2), hash(1)], vec![BlockHash::ZERO, hash(2)]] {
        assert!(visit_link(&key, &bincode::serialize(&sources).unwrap(), |_| panic!("invalid sources emitted roots")).is_err());
    }
    for key in [format!("verification_identity_binding/v1/{}", hash(1)),
        format!("verification_certified_outcome/v1/{}", hash(1)),
        format!("verification_identity_source/v1/{owner}")] {
        assert!(visit_link(&key, BlockHash::ZERO.as_bytes(), |_| panic!("zero emitted roots")).is_err());
    }
}

#[test]
fn stake_roots_include_attempt_source_and_settlement_without_status_filtering() {
    let target = address(1);
    let staker = address(2);
    for challenge in [false, true] {
        for settled_by in [None, Some(hash(4))] {
            let key = stake_key(&target, &staker, challenge, hash(0xab));
            let receipt = StakeReceipt { block: hash(3), amount: 5, settled_by };
            let bytes = bincode::serialize(&receipt).unwrap();
            let mut expected = vec![format!("{:?}", Reference::Account(&target)),
                format!("{:?}", Reference::Account(&staker)), format!("{:?}", if challenge { Reference::Block(hash(0xab)) } else { Reference::CommitteeContext(hash(0xab)) }),
                format!("{:?}", Reference::Block(hash(3)))];
            if let Some(hash) = settled_by { expected.push(format!("{:?}", Reference::Outcome(hash))); }
            let mut actual = Vec::new();
            visit_stake(&key, &bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
            assert_eq!(actual, expected);
            for stop in 1..=expected.len() {
                let mut calls = 0;
                let result = visit_stake(&key, &bytes, |_| {
                    calls += 1;
                    if calls == stop { Err(StoreError::Backend("stop".into())) } else { Ok(()) }
                });
                assert!(matches!(result, Err(StoreError::Backend(reason)) if reason == "stop"));
                assert_eq!(calls, stop);
            }
            for invalid_key in [key.replace("/v2/", "/v1/"), format!("{key}/extra"),
                key.replace(&hash(0xab).to_string(), &hash(0xab).to_string().to_uppercase()),
                stake_key(&target, &staker, challenge, BlockHash::ZERO),
                key.replace(target.as_str(), "invalid")]
            {
                assert!(visit_stake(&invalid_key, &bytes, |_| panic!("invalid key emitted roots")).is_err());
            }
            let mut trailing = bytes.clone(); trailing.push(0);
            assert!(visit_stake(&key, &trailing, |_| panic!("trailing data emitted roots")).is_err());
            for bad in [StakeReceipt { block: BlockHash::ZERO, amount: 5, settled_by },
                StakeReceipt { block: hash(3), amount: 5, settled_by: Some(BlockHash::ZERO) }]
            {
                assert!(visit_stake(&key, &bincode::serialize(&bad).unwrap(), |_| panic!("zero dependency emitted roots")).is_err());
            }
        }
    }
}

#[test]
fn outcome_roots_preserve_all_verdicts_appeal_links_and_derived_rewards() {
    use receipts::{Verdict, ChallengeVerdict, Receipt, Reward};
    let target = address(1);
    let recipient = address(2);
    let source = hash(3);
    let origin = receipts::reward_origin(source, &recipient);
    let mut verdicts = vec![Verdict::Verified, Verdict::Failed];
    for reason in [burst_types::ChallengeReason::Fraud, burst_types::ChallengeReason::Inactivity] {
        for result in [ChallengeVerdict::Upheld, ChallengeVerdict::Rejected, ChallengeVerdict::LegacyExpired] {
            verdicts.push(Verdict::Challenge { admission: hash(4), reason, result: result.clone() });
            if result != ChallengeVerdict::LegacyExpired {
                verdicts.push(Verdict::Appeal { admission: hash(4), reason, result, reviewed: hash(5) });
            }
        }
    }
    for verdict in verdicts {
        let receipt = Receipt { source, target: target.clone(), completed_at: Timestamp::new(9),
            previous: Some(hash(5)), verdict, rewards: vec![Reward { recipient: recipient.clone(), origin, amount: 7 }] };
        let key = receipts::key(source);
        let bytes = bincode::serialize(&receipt).unwrap();
        let mut expected = vec![format!("{:?}", Reference::Outcome(source)), format!("{:?}", Reference::Account(&target)),
            format!("{:?}", Reference::Boundary(Timestamp::new(9))), format!("{:?}", Reference::Outcome(hash(5)))];
        match &receipt.verdict {
            Verdict::Verified | Verdict::Failed => {},
            Verdict::Challenge { .. } => expected.push(format!("{:?}", Reference::Block(hash(4)))),
            Verdict::Appeal { .. } => { expected.push(format!("{:?}", Reference::Block(hash(4)))); expected.push(format!("{:?}", Reference::Outcome(hash(5)))); }
        }
        expected.push(format!("{:?}", Reference::Account(&recipient)));
        expected.push(format!("{:?}", Reference::RewardOrigin(origin)));
        let mut actual = Vec::new();
        visit_outcome(&key, &bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
        assert_eq!(actual, expected);
        for stop in 1..=expected.len() {
            let mut calls = 0;
            assert!(visit_outcome(&key, &bytes, |_| {
                calls += 1; if calls == stop { Err(invalid("stop")) } else { Ok(()) }
            }).is_err());
            assert_eq!(calls, stop);
        }
        assert!(visit_outcome(&receipts::key(hash(9)), &bytes, |_| panic!("wrong key emitted roots")).is_err());
        let mut trailing = bytes.clone(); trailing.push(0);
        assert!(visit_outcome(&key, &trailing, |_| panic!("trailing data emitted roots")).is_err());
        let mut forged = receipt.clone(); forged.rewards[0].origin = TxHash::ZERO;
        assert!(visit_outcome(&key, &bincode::serialize(&forged).unwrap(), |_| panic!("forged reward emitted roots")).is_err());
    }
}

#[test]
fn deadline_roots_preserve_typed_activation_defaults_and_reject_corruption() {
    let rules = burst_types::ProtocolParams::default();
    let activation = crate::verification_activation::Activation {
        target: address(1), selection: hash(2), context: hash(3),
        params_hash: rules.params_hash(), rules_at_activation: rules,
        started_at: 7, deadline: 9,
    };
    let completion = crate::verification_deadline::Completion {
        activation: activation.clone(), lifecycle: hash(4), completed_at: 10,
        absent: vec![address(5), address(6)],
    };
    let activation_expected = vec![
        format!("{:?}", Reference::Account(&address(1))),
        format!("{:?}", Reference::CommitteeContext(hash(3))),
        format!("{:?}", Reference::Checkpoint(hash(2))),
        format!("{:?}", Reference::Parameters(activation.params_hash)),
        format!("{:?}", Reference::Boundary(Timestamp::new(7))),
        format!("{:?}", Reference::Boundary(Timestamp::new(9))),
    ];
    let mut completion_expected = activation_expected.clone();
    completion_expected.extend([
        format!("{:?}", Reference::Block(hash(4))),
        format!("{:?}", Reference::Boundary(Timestamp::new(10))),
        format!("{:?}", Reference::Account(&address(5))),
        format!("{:?}", Reference::Account(&address(6))),
    ]);
    for (key, bytes, expected) in [
        (crate::verification_activation::key(hash(3)), bincode::serialize(&activation).unwrap(), activation_expected),
        (crate::verification_deadline::key(hash(3)), bincode::serialize(&completion).unwrap(), completion_expected),
        (crate::verification_deadline::voter_key(&address(1), &address(5)), hash(3).as_bytes().to_vec(), vec![
            format!("{:?}", Reference::Account(&address(1))), format!("{:?}", Reference::Account(&address(5))),
            format!("{:?}", Reference::CommitteeContext(hash(3))),
        ]),
    ] {
        let mut actual = Vec::new();
        visit_row(&key, &bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
        assert_eq!(actual, expected);
        for stop in 1..=expected.len() {
            let mut calls = 0;
            assert!(visit_row(&key, &bytes, |_| {
                calls += 1; if calls == stop { Err(invalid("stop")) } else { Ok(()) }
            }).is_err());
            assert_eq!(calls, stop);
        }
        let mut trailing = bytes.clone(); trailing.push(0);
        for (bad_key, bad_bytes) in [(key.replace("/v1/", "/v2/"), bytes.clone()), (key.clone(), trailing)] {
            assert!(visit_row(&bad_key, &bad_bytes, |_| panic!("invalid row emitted roots")).is_err());
        }
    }
    let mut bad_activation = activation.clone(); bad_activation.params_hash = hash(8);
    let mut bad_completion = completion.clone(); bad_completion.absent.reverse();
    let mut early_completion = completion.clone(); early_completion.completed_at = 8;
    for (key, bytes) in [
        (crate::verification_activation::key(hash(8)), bincode::serialize(&activation).unwrap()),
        (crate::verification_activation::key(hash(3)), bincode::serialize(&bad_activation).unwrap()),
        (crate::verification_deadline::key(hash(8)), bincode::serialize(&completion).unwrap()),
        (crate::verification_deadline::key(hash(3)), bincode::serialize(&bad_completion).unwrap()),
        (crate::verification_deadline::key(hash(3)), bincode::serialize(&early_completion).unwrap()),
        ("verification_default/v1/brst_target/brst_voter/extra".into(), hash(3).as_bytes().to_vec()),
        (crate::verification_deadline::voter_key(&address(1), &address(5)), vec![0; 32]),
    ] { assert!(visit_row(&key, &bytes, |_| panic!("invalid row emitted roots")).is_err()); }
}
