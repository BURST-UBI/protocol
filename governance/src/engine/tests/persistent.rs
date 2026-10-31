use super::*;
use std::collections::HashMap as LegacyMap;

#[test]
fn local_records_preserve_full_content_and_multiple_pending_amendments() {
    let mut content = ProposalContent::ConstitutionalDiff {
        title: "Equal rights".into(),
        operations: vec![crate::proposal::AmendmentOp::AddArticle {
            title: "Humanity".into(),
            text: "🌍".repeat(crate::content::MAX_AMENDMENT_CHARS),
        }],
    };
    let padding = crate::content::MAX_CONTENT_BYTES
        - bincode::serialized_size(&content).unwrap() as usize;
    if let ProposalContent::ConstitutionalDiff { title, .. } = &mut content {
        title.push_str(&"x".repeat(padding));
    }
    content.validate_content().unwrap();
    assert_eq!(
        bincode::serialized_size(&content).unwrap() as usize,
        crate::content::MAX_CONTENT_BYTES
    );
    let mut proposal = make_emergency_proposal(1000);
    proposal.content = content.clone();
    let records = [
        GovernanceRecord::Proposal(Box::new(proposal)),
        GovernanceRecord::PendingEffects {
            changes: vec![],
            amendments: vec![content; 3],
            evictions: vec![],
        },
    ];
    let mut decoded = Vec::new();
    for record in records {
        let (key, bytes) = record.storage_entry().unwrap();
        assert!(bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES);
        let restored = GovernanceRecord::from_storage_entry(&key, &bytes).unwrap();
        assert_eq!(restored, record);
        let mut trailing = bytes;
        trailing.push(0);
        assert!(GovernanceRecord::from_storage_entry(&key, &trailing).is_err());
        decoded.push(Ok(restored));
    }
    let engine = GovernanceEngine::from_records(decoded).unwrap();
    assert_eq!(engine.pending_amendments.len(), 3);
    assert_eq!(engine.proposals.len(), 1);
}

#[test]
fn larger_local_envelopes_do_not_relax_content_or_small_record_bounds() {
    let invalid_content = [
        ProposalContent::ConstitutionalAmendment {
            title: "Too many characters".into(),
            text: "x".repeat(crate::content::MAX_AMENDMENT_CHARS + 1),
        },
        ProposalContent::ConstitutionalAmendment {
            title: "x".repeat(crate::content::MAX_CONTENT_BYTES),
            text: "x".into(),
        },
    ];
    for content in invalid_content {
        let mut proposal = make_emergency_proposal(1000);
        proposal.content = content.clone();
        for record in [
            GovernanceRecord::Proposal(Box::new(proposal)),
            GovernanceRecord::PendingEffects {
                changes: vec![],
                amendments: vec![content.clone()],
                evictions: vec![],
            },
        ] {
            assert!(record.storage_entry().is_err());
            let key = record.storage_key().unwrap();
            let bytes = bincode::serialize(&(1u8, &record)).unwrap();
            assert!(GovernanceRecord::from_storage_entry(&key, &bytes).is_err());
        }
    }
    let bucket = GovernanceRecord::VoteBucket {
        proposal: TxHash::new([1; 32]),
        promotion: false,
    };
    assert!(GovernanceRecord::from_storage_entry(
        &bucket.storage_key().unwrap(),
        &vec![0; burst_types::MAX_P2P_MESSAGE_BYTES + 1],
    )
    .is_err());
}

// Exact field order and map representation of the pre-persistent snapshot.
#[derive(Serialize, Deserialize)]
struct LegacyEngine {
    proposals: LegacyMap<TxHash, Proposal>,
    exploration_votes: LegacyMap<TxHash, LegacyMap<WalletAddress, GovernanceVote>>,
    promotion_votes: LegacyMap<TxHash, LegacyMap<WalletAddress, GovernanceVote>>,
    endorsement_counts: LegacyMap<TxHash, (u32, u128)>,
    pending_changes: Vec<(crate::params::GovernableParam, u128)>,
    pending_amendments: Vec<ProposalContent>,
    pending_evictions: Vec<(WalletAddress, bool)>,
    timings: LegacyMap<TxHash, crate::GovernanceTiming>,
    key_ballots: LegacyMap<(TxHash, WalletAddress, burst_types::BlockHash), GovernanceVote>,
}

fn populated() -> GovernanceEngine {
    let mut engine = GovernanceEngine::new();
    for n in 1..=128u8 {
        let mut proposal = make_emergency_proposal(1000);
        proposal.hash = TxHash::new([n; 32]);
        engine
            .submit_emergency_proposal(&mut proposal, Timestamp::new(1000), &default_params())
            .unwrap();
        for voter in 0..64 {
            engine
                .cast_exploration_vote(
                    &proposal.hash,
                    &WalletAddress::new(format!("brst_voter_{voter}")),
                    GovernanceVote::Yea,
                    Timestamp::new(1000),
                    &default_params(),
                )
                .unwrap();
        }
        engine.promotion_votes.insert(
            proposal.hash,
            OrdMap::from_iter([(dummy_wallet(), GovernanceVote::Abstain)]),
        );
        engine.endorsement_counts.insert(proposal.hash, (7, 123));
        engine.key_ballots.insert(
            (
                proposal.hash,
                dummy_wallet(),
                burst_types::BlockHash::new([n; 32]),
            ),
            GovernanceVote::Nay,
        );
    }
    engine
}

#[test]
fn cloned_engine_shares_unrelated_proposals_and_ballots() {
    let original = populated();
    let mut candidate = original.clone();
    assert!(original.proposals.ptr_eq(&candidate.proposals));
    assert!(original
        .exploration_votes
        .ptr_eq(&candidate.exploration_votes));
    assert!(original.key_ballots.ptr_eq(&candidate.key_ballots));
    let changed = TxHash::new([1; 32]);
    let untouched = TxHash::new([128; 32]);
    candidate
        .cast_exploration_vote(
            &changed,
            &WalletAddress::new("brst_new_voter"),
            GovernanceVote::Nay,
            Timestamp::new(1000),
            &default_params(),
        )
        .unwrap();
    assert_eq!(original.get_exploration_votes(&changed).unwrap().len(), 64);
    assert_eq!(candidate.get_exploration_votes(&changed).unwrap().len(), 65);
    assert!(original.exploration_votes[&untouched].ptr_eq(&candidate.exploration_votes[&untouched]));
    assert!(original.timings.ptr_eq(&candidate.timings));
    assert!(original.key_ballots.ptr_eq(&candidate.key_ballots));
    assert_eq!(original.proposals[&changed].exploration_votes_nay, 0);
    assert_eq!(candidate.proposals[&changed].exploration_votes_nay, 1);
}

#[test]
fn persistent_snapshots_decode_in_both_legacy_directions() {
    let mut original = populated();
    Arc::make_mut(&mut original.pending_changes).push((crate::GovernableParam::TrstExpirySecs, 42));
    Arc::make_mut(&mut original.pending_amendments).push(make_proposal(1000, 0).content);
    Arc::make_mut(&mut original.pending_evictions).push((dummy_wallet(), true));
    let encoded = bincode::serialize(&original).unwrap();
    let legacy: LegacyEngine = bincode::deserialize(&encoded).unwrap();
    assert_eq!(legacy.proposals.len(), 128);
    assert_eq!(
        legacy
            .exploration_votes
            .values()
            .map(|votes| votes.len())
            .sum::<usize>(),
        8192
    );
    let legacy_bytes = bincode::serialize(&legacy).unwrap();
    let restored: GovernanceEngine = bincode::deserialize(&legacy_bytes).unwrap();
    assert_eq!(restored, original);
}

fn records(engine: &GovernanceEngine) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut rows = std::collections::BTreeMap::new();
    engine
        .visit_records(|record| {
            let (key, bytes) = record.storage_entry()?;
            assert!(rows.insert(key, bytes).is_none());
            Ok(())
        })
        .unwrap();
    rows
}

#[test]
fn record_delta_matches_complete_export_and_reverses_exactly() {
    let original = populated();
    let baseline = records(&original);
    let mut next = original.clone();
    next.cast_exploration_vote(
        &TxHash::new([1; 32]),
        &WalletAddress::new("brst_delta"),
        GovernanceVote::Nay,
        Timestamp::new(1000),
        &default_params(),
    )
    .unwrap();
    let mut updated = baseline.clone();
    let mut count = 0;
    next.visit_record_delta(&original, |old, new| {
        count += 1;
        if let Some(old) = old {
            let (key, value) = old.storage_entry()?;
            assert_eq!(updated.remove(&key), Some(value));
        }
        if let Some(new) = new {
            let (key, value) = new.storage_entry()?;
            assert!(updated.insert(key, value).is_none());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 2, "only the changed proposal and its new ballot");
    assert_eq!(updated, records(&next));
    original
        .visit_record_delta(&next, |old, new| {
            if let Some(old) = old {
                let (key, value) = old.storage_entry()?;
                assert_eq!(updated.remove(&key), Some(value));
            }
            if let Some(new) = new {
                let (key, value) = new.storage_entry()?;
                updated.insert(key, value);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(updated, baseline);
    original
        .visit_record_delta(&original.clone(), |_, _| {
            panic!("unchanged state emitted a record")
        })
        .unwrap();
}

#[test]
fn records_roundtrip_empty_buckets_effects_and_reject_misbound_data() {
    let mut engine = populated();
    engine
        .exploration_votes
        .insert(TxHash::new([255; 32]), OrdMap::new());
    Arc::make_mut(&mut engine.pending_changes).push((crate::GovernableParam::TrstExpirySecs, 42));
    Arc::make_mut(&mut engine.pending_amendments).push(make_proposal(1000, 0).content);
    Arc::make_mut(&mut engine.pending_evictions).push((dummy_wallet(), true));
    assert_eq!(bincode::serialize(&engine).unwrap(), bincode::serialize(&engine.snapshot_view()).unwrap());
    let rows = records(&engine);
    let restored = GovernanceEngine::from_records(
        rows.iter()
            .rev()
            .map(|(key, value)| GovernanceRecord::from_storage_entry(key, value)),
    )
    .unwrap();
    assert_eq!(restored, engine);
    let (key, bytes) = rows.iter().next().unwrap();
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(GovernanceRecord::from_storage_entry(key, &trailing).is_err());
    let mut version = bytes.clone();
    version[0] = 2;
    assert!(GovernanceRecord::from_storage_entry(key, &version).is_err());
    assert!(GovernanceRecord::from_storage_entry(&format!("{key}x"), bytes).is_err());
    let decoded: Vec<_> = rows
        .iter()
        .map(|(k, v)| GovernanceRecord::from_storage_entry(k, v).unwrap())
        .collect();
    assert!(GovernanceEngine::from_records(
        decoded
            .iter()
            .cloned()
            .chain(decoded.iter().take(1).cloned())
            .map(Ok)
    )
    .is_err());
    assert!(GovernanceEngine::from_records(
        decoded
            .iter()
            .filter(|r| !matches!(r, GovernanceRecord::PendingEffects { .. }))
            .cloned()
            .map(Ok)
    )
    .is_err());
    assert!(GovernanceEngine::from_records(
        decoded
            .iter()
            .filter(|r| !matches!(r, GovernanceRecord::VoteBucket { .. }))
            .cloned()
            .map(Ok)
    )
    .is_err());
    assert!(GovernanceEngine::from_records([Err("read failed".into())]).is_err());
}

#[test]
fn pending_effect_snapshots_share_payloads_and_detach_on_mutation_and_drain() {
    let changes = vec![(crate::GovernableParam::BrnRate, 42), (crate::GovernableParam::TrstExpirySecs, 9)];
    let amendments = vec![ProposalContent::ConstitutionalAmendment {
        title: "Shared amendment".into(), text: "x".repeat(128 * 1024),
    }];
    let evictions = vec![(dummy_wallet(), true), (dummy_wallet(), false)];
    let mut original = GovernanceEngine::new();
    original.pending_changes = Arc::new(changes.clone());
    original.pending_amendments = Arc::new(amendments.clone());
    original.pending_evictions = Arc::new(evictions.clone());
    assert_eq!(bincode::serialize(&original.pending_changes).unwrap(), bincode::serialize(&changes).unwrap());
    assert_eq!(bincode::serialize(&original.pending_amendments).unwrap(), bincode::serialize(&amendments).unwrap());
    assert_eq!(bincode::serialize(&original.pending_evictions).unwrap(), bincode::serialize(&evictions).unwrap());
    let mut snapshot = original.clone();
    assert!(Arc::ptr_eq(&snapshot.pending_changes, &original.pending_changes));
    assert!(Arc::ptr_eq(&snapshot.pending_amendments, &original.pending_amendments));
    assert!(Arc::ptr_eq(&snapshot.pending_evictions, &original.pending_evictions));
    Arc::make_mut(&mut snapshot.pending_changes).push((crate::GovernableParam::BrnRate, 99));
    Arc::make_mut(&mut snapshot.pending_amendments).clear();
    Arc::make_mut(&mut snapshot.pending_evictions).reverse();
    assert_eq!(*original.pending_changes, changes);
    assert_eq!(*original.pending_amendments, amendments);
    assert_eq!(*original.pending_evictions, evictions);
    let mut shared = original.clone();
    assert_eq!(shared.drain_pending_changes(), changes);
    assert_eq!(shared.drain_activated_amendments(), amendments);
    assert_eq!(shared.drain_pending_evictions(), evictions);
    assert!(shared.drain_pending_changes().is_empty());
    assert!(shared.drain_activated_amendments().is_empty());
    assert!(shared.drain_pending_evictions().is_empty());
    assert_eq!(*original.pending_changes, changes);
    assert_eq!(*original.pending_amendments, amendments);
    assert_eq!(*original.pending_evictions, evictions);
    // With no other owner, draining moves the payload instead of copying it.
    let payload = original.pending_amendments.as_ptr();
    let drained = original.drain_activated_amendments();
    assert_eq!(drained.as_ptr(), payload);
    assert_eq!(drained, amendments);
}

#[test]
fn shared_suffix_tally_preserves_direct_and_key_ballot_overrides() {
    let a = WalletAddress::new("brst_a");
    let b = WalletAddress::new("brst_b");
    let c = WalletAddress::new("brst_c");
    let mut delegation = DelegationEngine::default();
    delegation.delegate(&a, &b).unwrap();
    delegation.delegate(&b, &c).unwrap();
    let mut votes = OrdMap::new();
    votes.insert(b.clone(), GovernanceVote::Nay);
    votes.insert(c.clone(), GovernanceVote::Yea);
    let voters = vec![a.clone(), b.clone(), c.clone(), a.clone()];
    let proposal = make_proposal(1000, 0);
    // A resolves all the way to C; B's own direct ballot still takes precedence.
    assert_eq!(GovernanceEngine::count_votes_with_delegation(&votes, &delegation,
        &voters, Some(&proposal)), (2, 1, 0));
    delegation.project_key_ballot(a.clone(), None);
    assert_eq!(GovernanceEngine::count_votes_with_delegation(&votes, &delegation,
        &voters, Some(&proposal)), (1, 1, 0));
    votes.insert(a, GovernanceVote::Abstain);
    assert_eq!(GovernanceEngine::count_votes_with_delegation(&votes, &delegation,
        &voters, Some(&proposal)), (1, 1, 1));
}
