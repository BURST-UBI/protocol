use super::*;
use crate::confirmed_replay::snapshot::history::HistoricalBlock;

#[test]
fn compact_snapshot_preserves_device_grant_without_retaining_the_handoff_block() {
    let mut fixture = Fixture::selection();
    let grant = fixture.push(0, BlockType::ChangeSigner, 1_595_434_048, |block| {
        block.link = BlockHash::new([77; 32]);
    });
    fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_049, |_| {});
    let (_dir, source, snapshot) = snapshot(&fixture);
    let witnesses = retained(&snapshot, &fixture, &source, &[]);
    assert!(!witnesses.iter().any(|proof| proof.block().hash == grant));
    let certificates = vec![
        CertifiedCheckpoint::load(&source, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap(),
    ];
    let base = snapshot.into_continuation();
    let (_sparse_dir, sparse) = cemented::sparse(&base, &source);
    use burst_store::block::BlockStore;
    assert!(!sparse.block_store().exists(&grant).unwrap());
    let head = base.state().account(&address(0)).unwrap().head;
    let retained_head: StateBlock =
        bincode::deserialize(&sparse.block_store().get_block(&head).unwrap()).unwrap();
    assert_eq!(retained_head.active_signing_grant(), grant);
    let cutoff = base.anchor().cutoff() + 1;
    fixture.push(0, BlockType::Burn, cutoff, |block| {
        block.payment_epoch = base.anchor().hash();
        block.link = destination(21);
        block.brn_balance += 1;
    });
    let valid = fixture.blocks.last().unwrap().clone();
    assert_eq!(valid.signing_grant, grant);
    let full_history = fixture.history(cutoff);
    let (_tail_dir, tail_store) = suffix_store(std::slice::from_ref(&valid));
    let tail = base
        .read_tail(&tail_store, cutoff, full_history.heads(), witnesses)
        .unwrap();
    let compact = base.clone().replay_tail(tail, &certificates).unwrap();
    let full = ConfirmedReplay::replay_with_certificates(
        &full_history,
        NetworkId::Dev,
        &fixture.params,
        &certificates,
    )
    .unwrap();
    assert_same(&compact, &full);

    let stale = fixture.blocks.last_mut().unwrap();
    stale.signing_grant = BlockHash::ZERO;
    sign(0, stale);
    let stale = stale.clone();
    let stale_history = fixture.history(cutoff);
    let snapshot = authenticate(&source, &fixture.params, base.anchor().hash());
    let witnesses = retained(&snapshot, &fixture, &source, &[]);
    let (_tail_dir, tail_store) = suffix_store(std::slice::from_ref(&stale));
    let tail = base
        .read_tail(&tail_store, cutoff, stale_history.heads(), witnesses)
        .unwrap();
    assert!(base.replay_tail(tail, &certificates).is_err());
    assert!(!sparse.block_store().exists(&valid.hash).unwrap());
    assert!(!sparse.block_store().exists(&stale.hash).unwrap());
}

pub(in crate::confirmed_replay::tests) fn assert_same(
    compact: &ConfirmedReplay,
    full: &ConfirmedReplay,
) {
    let a = compact.snapshot_bytes().unwrap();
    let b = full.snapshot_bytes().unwrap();
    if a != b {
        let a: ciborium::Value = ciborium::from_reader(a.as_slice()).unwrap();
        let b: ciborium::Value = ciborium::from_reader(b.as_slice()).unwrap();
        fn difference(a: &ciborium::Value, b: &ciborium::Value, path: String) -> String {
            if let (Some(a), Some(b)) = (a.as_map(), b.as_map()) {
                for (key, value) in a {
                    match b.iter().find(|(other, _)| other == key) {
                        Some((_, other)) if value != other => {
                            return difference(value, other, format!("{path}/{key:?}"))
                        }
                        None => return format!("{path}/{key:?}: missing"),
                        _ => {}
                    }
                }
            }
            if let (Some(a), Some(b)) = (a.as_array(), b.as_array()) {
                for (index, (value, other)) in a.iter().zip(b).enumerate() {
                    if value != other {
                        return difference(value, other, format!("{path}/{index}"));
                    }
                }
            }
            match (a.as_bytes(), b.as_bytes()) {
                (Some(a), Some(b)) => format!(
                    "{path}: byte strings differ ({} and {} bytes)",
                    a.len(),
                    b.len()
                ),
                _ => format!("{path}: {a:?} != {b:?}"),
            }
        }
        panic!(
            "compact replay differs: {}",
            difference(&a, &b, String::new())
        );
    }
}

pub(in crate::confirmed_replay::tests) fn retained(
    snapshot: &AuthenticatedSnapshot,
    fixture: &Fixture,
    store: &LmdbStore,
    extra: &[BlockHash],
) -> Vec<HistoricalBlock> {
    let mut needed: HashSet<_> = snapshot
        .ordering_dependencies()
        .into_iter()
        .chain(extra.iter().copied())
        .collect();
    let accounts: HashSet<_> = fixture
        .blocks
        .iter()
        .map(|block| block.account.clone())
        .collect();
    let mut result = Vec::new();
    for account in accounts {
        let Some(mut cursor) = snapshot.history_cursor(&account).unwrap() else {
            continue;
        };
        needed.insert(cursor.next().unwrap().hash);
        while let Some(next) = cursor.next() {
            let chunk = read_chunk(store, next.hash, MAX_WITNESS_BLOCKS).unwrap();
            for witness in cursor.accept(&chunk).unwrap() {
                if needed.remove(&witness.block().hash) {
                    result.push(witness);
                }
            }
        }
    }
    assert!(needed.is_empty());
    result
}

#[test]
fn compact_snapshot_replays_payments_and_governance_with_identical_complete_state() {
    let mut fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let certificates =
        CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
    let retained = retained(&snapshot, &fixture, &store, &[]);
    assert!(retained.len() < fixture.blocks.len());
    let start = fixture.blocks.len();
    let cutoff = snapshot.anchor().cutoff() + 50;
    let epoch = snapshot.anchor().hash();
    let burn = fixture.push(0, BlockType::Burn, cutoff - 10, |block| {
        block.payment_epoch = epoch;
        block.brn_balance += 5;
        block.link = destination(30);
    });
    fixture.push(30, BlockType::Open, cutoff - 9, |block| {
        block.link = burn;
        block.trst_balance = 5;
    });
    let content = burst_governance::ProposalContent::ParameterChange {
        param: burst_governance::GovernableParam::BrnRate,
        new_value: 42,
    };
    let cost = fixture.params.governance_proposal_cost;
    let proposal = fixture.push(0, BlockType::GovernanceProposal, cutoff - 8, |block| {
        block.link = BlockHash::new(content.encode_compact().unwrap());
    });
    fixture.push(1, BlockType::GovernanceEndorse, cutoff - 7, |block| {
        block.link = proposal;
        block.brn_balance += cost;
    });
    let full_history = fixture.history(cutoff);
    let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
    let tail = snapshot
        .read_tail(&tail_store, cutoff, full_history.heads(), retained)
        .unwrap();
    let compact = snapshot.replay_tail(tail, &certificates).unwrap();
    let full = ConfirmedReplay::replay_with_certificates(
        &full_history,
        NetworkId::Dev,
        &fixture.params,
        &certificates,
    )
    .unwrap();
    assert_same(&compact, &full);
    assert_eq!(
        compact.replay_dependencies,
        HashSet::from([fixture.blocks.last().unwrap().hash])
    );
}

#[test]
fn compact_snapshot_replays_certified_votes_then_newly_verified_spending() {
    let mut fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let mut certificate = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    certificate
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
    let retained = retained(&snapshot, &fixture, &store, &[]);
    let start = fixture.blocks.len();
    let cutoff = snapshot.anchor().cutoff() + 50;
    for (index, seed) in certified_votes::panel(&certificate).iter().enumerate() {
        certified_votes::vote_choice(
            &mut fixture,
            &certificate,
            *seed,
            cutoff - 19 + index as u64,
            1,
        );
    }
    fixture.push(20, BlockType::Open, cutoff - 10, |_| {});
    let burn = fixture.push(20, BlockType::Burn, cutoff - 5, |block| {
        block.brn_balance = 1;
        block.link = destination(30);
    });
    fixture.push(30, BlockType::Open, cutoff, |block| {
        block.link = burn;
        block.trst_balance = 1;
    });
    let full_history = fixture.history(cutoff);
    let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
    let tail = snapshot
        .read_tail(&tail_store, cutoff, full_history.heads(), retained)
        .unwrap();
    let certificates = [certificate];
    let compact = snapshot.replay_tail(tail, &certificates).unwrap();
    let full = ConfirmedReplay::replay_with_certificates(
        &full_history,
        NetworkId::Dev,
        &fixture.params,
        &certificates,
    )
    .unwrap();
    assert!(compact.account(&address(20)).unwrap().state.is_verified());
    assert_same(&compact, &full);
}

#[test]
fn compact_snapshot_rejects_missing_certificates_witnesses_and_semantic_double_claim() {
    for case in 0..4 {
        let mut fixture = Fixture::selection();
        let (_dir, store, snapshot) = snapshot(&fixture);
        let certificates =
            CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
        let mut retained = retained(&snapshot, &fixture, &store, &[]);
        let start = fixture.blocks.len();
        let cutoff = snapshot.anchor().cutoff() + 10;
        let epoch = snapshot.anchor().hash();
        let burn = fixture.push(0, BlockType::Burn, cutoff, |block| {
            block.payment_epoch = epoch;
            block.brn_balance += 1;
            block.link = destination(0);
        });
        fixture.push(0, BlockType::Receive, cutoff, |block| {
            block.link = burn;
            block.trst_balance += 1;
        });
        if case == 3 {
            fixture.push(0, BlockType::Receive, cutoff, |block| {
                block.link = burn;
                block.trst_balance += 1;
            });
        }
        if case == 1 {
            retained.clear();
        }
        let full = fixture.history(cutoff);
        let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
        let tail = snapshot
            .read_tail(&tail_store, cutoff, full.heads(), retained)
            .unwrap();
        let certificates = if case == 0 {
            vec![]
        } else if case == 2 {
            let different = Fixture::selection_with(|params| params.brn_rate += 1);
            let (_other_dir, other_store, _) = self::snapshot(&different);
            CertifiedCheckpoint::load_chain(&other_store, NetworkId::Dev, &different.params)
                .unwrap()
        } else {
            certificates
        };
        assert!(
            snapshot.replay_tail(tail, &certificates).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn compact_snapshot_requires_earlier_governance_ordering_witnesses() {
    for omit in [true, false] {
        let mut fixture = Fixture::selection();
        let content = burst_governance::ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 42,
        };
        let proposal = fixture.push(0, BlockType::GovernanceProposal, 1_595_434_040, |block| {
            block.link = BlockHash::new(content.encode_compact().unwrap());
        });
        let cost = fixture.params.governance_proposal_cost;
        let support = fixture.push(1, BlockType::GovernanceEndorse, 1_595_434_041, |block| {
            block.link = proposal;
            block.brn_balance += cost;
        });
        let (_dir, store, snapshot) = snapshot(&fixture);
        assert_eq!(snapshot.ordering_dependencies(), vec![support]);
        let certificates =
            CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
        let mut witnesses = retained(&snapshot, &fixture, &store, &[proposal]);
        if omit {
            witnesses.retain(|witness| witness.block().hash != support);
        }
        let start = fixture.blocks.len();
        let cutoff = snapshot.anchor().cutoff() + 10;
        fixture.push(0, BlockType::GovernanceWithdraw, cutoff, |block| {
            block.payment_epoch = snapshot.anchor().hash();
            block.link = proposal;
        });
        let full = fixture.history(cutoff);
        let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
        let tail = snapshot
            .read_tail(&tail_store, cutoff, full.heads(), witnesses)
            .unwrap();
        let compact = snapshot.replay_tail(tail, &certificates);
        if omit {
            assert!(compact
                .err()
                .unwrap()
                .to_string()
                .contains("ordering history"));
        } else {
            let compact = compact.unwrap();
            let full = ConfirmedReplay::replay_with_certificates(
                &full,
                NetworkId::Dev,
                &fixture.params,
                &certificates,
            )
            .unwrap();
            assert_same(&compact, &full);
        }
    }
}

#[test]
fn compact_snapshot_authenticates_full_proposal_body_before_execution() {
    let mut fixture = Fixture::selection();
    let (_dir, store, snapshot) = snapshot(&fixture);
    let certificates =
        CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
    let body = burst_governance::content::EncodedContent::new(
        &burst_governance::ProposalContent::ConstitutionalDiff {
            title: "Voting rights".into(),
            operations: vec![burst_governance::proposal::AmendmentOp::AddArticle {
                title: "Voting".into(),
                text: "Each verified human has one vote.".into(),
            }],
        },
    )
    .unwrap();
    let start = fixture.blocks.len();
    let cutoff = snapshot.anchor().cutoff() + 10;
    let proposal = fixture.push(0, BlockType::GovernanceProposal, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        (block.link, block.origin, block.transaction) = body.manifest().block_fields().unwrap();
    });
    let (_full_dir, full_store, heads) = fixture.store();
    let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
    let key = crate::governance_content::key(proposal);
    for bytes in [None, Some(&b"bad body"[..]), Some(body.bytes())] {
        if let Some(bytes) = bytes {
            tail_store.meta_store().put_meta(&key, bytes).unwrap();
        }
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let result = snapshot.read_tail(&tail_store, cutoff, &heads, witnesses);
        if bytes != Some(body.bytes()) {
            assert!(result.is_err());
        } else {
            assert!(result.is_ok());
        }
    }
    full_store
        .meta_store()
        .put_meta(&key, body.bytes())
        .unwrap();
    let history = ConfirmedHistory::read_certified(
        &full_store,
        NetworkId::Dev,
        fixture.blocks[0].hash,
        cutoff,
        &heads,
        &[],
    )
    .unwrap();
    let witnesses = retained(&snapshot, &fixture, &store, &[]);
    let tail = snapshot
        .read_tail(&tail_store, cutoff, &heads, witnesses)
        .unwrap();
    let compact = snapshot.replay_tail(tail, &certificates).unwrap();
    let full = ConfirmedReplay::replay_with_certificates(
        &history,
        NetworkId::Dev,
        &fixture.params,
        &certificates,
    )
    .unwrap();
    assert_same(&compact, &full);
    assert_eq!(compact.proposal_contents[&proposal], body.bytes());
}
