use super::*;
use crate::checkpoint_store::chain;

/// The source remains archival for proof delivery and independent full replay.
/// Each compact input store contains only the new suffix and proposal bodies.
pub(in crate::confirmed_replay::tests) fn assert_chain(fixture: &Fixture, store: &LmdbStore) {
    let events = chain::load(store, NetworkId::Dev, &fixture.params).unwrap();
    assert!(events.len() > 1);
    let mut omitted = assert_from(fixture, store, &events, 0);
    if events.len() > 2 {
        omitted |= assert_from(fixture, store, &events, events.len() - 2);
    }
    assert!(omitted, "must exercise execution without the full prefix");
    super::stored_recovery::assert_stored_chain(fixture, store, 0);
    if events.len() > 2 {
        super::stored_recovery::assert_stored_chain(fixture, store, events.len() - 2);
    }
}

fn assert_from(
    fixture: &Fixture,
    store: &LmdbStore,
    events: &[chain::CertifiedEvent],
    start: usize,
) -> bool {
    let first = events[start].certificate().checkpoint.hash();
    let snapshot = authenticate(store, &fixture.params, first);
    let mut recovered = snapshot.recover_selections().unwrap();
    for record in &mut recovered {
        assert!(record.replayed().history_hashes().is_err());
        let source = events[..=start]
            .iter()
            .filter_map(chain::CertifiedEvent::selection)
            .find(|source| {
                source.certificate().checkpoint.hash() == record.certificate().checkpoint.hash()
            })
            .unwrap();
        if let Some(beacon) = source.beacon() {
            record.attach_compact_beacon(beacon.clone()).unwrap();
        }
    }
    let mut base = snapshot.into_continuation();
    let mut omitted_ancestors = false;
    for index in start + 1..events.len() {
        let event = &events[index];
        let checkpoint = &event.certificate().checkpoint;
        let history = ConfirmedHistory::read_certified(
            store,
            NetworkId::Dev,
            base.anchor().genesis_hash(),
            checkpoint.cutoff,
            event.heads(),
            &[],
        )
        .unwrap();
        let mut heights = HashMap::new();
        let blocks: Vec<_> = history
            .replay_blocks()
            .iter()
            .filter(|block| {
                let height = heights.entry(block.account.clone()).or_insert(0u64);
                *height += 1;
                *height
                    > base
                        .state()
                        .account(&block.account)
                        .map_or(0, |account| account.block_count)
            })
            .cloned()
            .collect();
        let new: HashSet<_> = blocks.iter().map(|block| block.hash).collect();
        let certificates: Vec<_> = recovered.iter().collect();
        let mut needed: HashSet<_> = base
            .ordering_dependencies()
            .into_iter()
            .chain(blocks.iter().flat_map(StateBlock::dependencies))
            .chain(
                certificates
                    .iter()
                    .flat_map(|certificate| certificate.heads().iter().map(|head| head.hash)),
            )
            .filter(|hash| !new.contains(hash))
            .collect();
        let mut retained = Vec::new();
        let accounts: HashSet<_> = fixture
            .blocks
            .iter()
            .map(|block| block.account.clone())
            .collect();
        for account in accounts {
            let Some(mut cursor) = base.history_cursor(&account).unwrap() else {
                continue;
            };
            needed.insert(cursor.next().unwrap().hash);
            while let Some(next) = cursor.next() {
                for witness in cursor
                    .accept(&read_chunk(store, next.hash, MAX_WITNESS_BLOCKS).unwrap())
                    .unwrap()
                {
                    if needed.remove(&witness.block().hash) {
                        retained.push(witness);
                    }
                }
            }
        }
        assert!(
            needed.is_empty(),
            "missing proof at height {}",
            checkpoint.height
        );
        omitted_ancestors |= retained.len() + blocks.len() < history.replay_blocks().len();
        let (_tail_dir, suffix) = suffix_store(&blocks);
        for witness in history
            .proposal_witnesses()
            .iter()
            .filter(|witness| new.contains(&witness.block))
        {
            suffix
                .meta_store()
                .put_meta(
                    &crate::governance_content::key(witness.block),
                    &witness.bytes,
                )
                .unwrap();
        }
        let tail = base
            .read_tail(&suffix, checkpoint.cutoff, event.heads(), retained)
            .unwrap();
        if let chain::CertifiedEvent::Outcome(outcome) = event {
            let before = base.state().snapshot_bytes().unwrap();
            let coverage = base
                .outcome_vote_coverage_for_manifest(&tail, outcome.manifest(), &certificates)
                .unwrap();
            assert_eq!(coverage.manifest(), outcome.manifest());
            let mut forged = outcome.manifest().clone();
            forged.checkpoint.selection_root = BlockHash::ZERO;
            assert!(base
                .outcome_vote_coverage_for_manifest(&tail, &forged, &certificates)
                .is_err());
            assert_eq!(base.state().snapshot_bytes().unwrap(), before);
            super::deadline_signing::assert_candidate_signing(
                &base,
                store,
                event.certificate(),
                || {
                    crate::checkpoint_agreement::CompactReplayCandidate::Outcome(Box::new(
                        base.outcome_vote_coverage_for_manifest(
                            &tail,
                            outcome.manifest(),
                            &certificates,
                        )
                        .unwrap(),
                    ))
                },
            );
        }
        if let chain::CertifiedEvent::Deadline(deadline) = event {
            let before = base.state().snapshot_bytes().unwrap();
            assert_eq!(
                base.prepare_deadline(&tail, &certificates).unwrap(),
                Some(deadline.manifest().clone())
            );
            assert_eq!(base.state().snapshot_bytes().unwrap(), before);
            super::deadline_signing::assert_signing(
                &base,
                &tail,
                &certificates,
                store,
                event.certificate(),
            );
        }
        if let chain::CertifiedEvent::Governance(governance) = event {
            let before = base.state().snapshot_bytes().unwrap();
            let coverage = base
                .governance_vote_coverage_for_manifest(&tail, governance.manifest(), &certificates)
                .unwrap();
            assert_eq!(coverage.manifest(), governance.manifest());
            for field in 0..3 {
                let mut forged = governance.manifest().clone();
                match field {
                    0 => forged.checkpoint.selection_root = BlockHash::ZERO,
                    1 => forged.checkpoint.params_hash = BlockHash::ZERO,
                    _ => {
                        forged.proposals.push(TxHash::ZERO);
                    }
                }
                assert!(base
                    .governance_vote_coverage_for_manifest(&tail, &forged, &certificates)
                    .is_err());
            }
            assert_eq!(base.state().snapshot_bytes().unwrap(), before);
            super::deadline_signing::assert_candidate_signing(
                &base,
                store,
                event.certificate(),
                || {
                    crate::checkpoint_agreement::CompactReplayCandidate::Governance(Box::new(
                        base.governance_vote_coverage_for_manifest(
                            &tail,
                            governance.manifest(),
                            &certificates,
                        )
                        .unwrap(),
                    ))
                },
            );
        }
        if let chain::CertifiedEvent::Activation(activation) = event {
            let before = base.state().snapshot_bytes().unwrap();
            let coverage = base
                .activation_vote_coverage_for_manifest(&tail, activation.manifest(), &certificates)
                .unwrap();
            assert_eq!(coverage.manifest(), activation.manifest());
            let mut forged = activation.manifest().clone();
            forged.activation.value[0] ^= 1;
            assert!(base
                .activation_vote_coverage_for_manifest(&tail, &forged, &certificates)
                .is_err());
            assert!(coverage
                .validate_fresh_time(crate::checkpoint_replay::SigningClock {
                    now: tail.cutoff() + 60,
                    error_bound: 0
                })
                .is_err());
            assert_eq!(base.state().snapshot_bytes().unwrap(), before);
            super::deadline_signing::assert_candidate_signing(
                &base,
                store,
                event.certificate(),
                || {
                    crate::checkpoint_agreement::CompactReplayCandidate::Activation(Box::new(
                        base.activation_vote_coverage_for_manifest(
                            &tail,
                            activation.manifest(),
                            &certificates,
                        )
                        .unwrap(),
                    ))
                },
            );
        }
        if let chain::CertifiedEvent::Roster(roster) = event {
            let before = base.state().snapshot_bytes().unwrap();
            let coverage = base
                .roster_vote_coverage_for_manifest(&tail, roster.manifest(), &certificates)
                .unwrap();
            assert_eq!(coverage.manifest(), roster.manifest());
            for field in 0..3 {
                let mut forged = roster.manifest().clone();
                match field {
                    0 => forged.checkpoint.next_roster = BlockHash::ZERO,
                    1 => forged.checkpoint.selection_root = BlockHash::ZERO,
                    _ => {
                        forged.heads.pop();
                    }
                }
                assert!(base
                    .roster_vote_coverage_for_manifest(&tail, &forged, &certificates)
                    .is_err());
            }
            assert_eq!(base.state().snapshot_bytes().unwrap(), before);
            super::deadline_signing::assert_candidate_signing(
                &base,
                store,
                event.certificate(),
                || {
                    crate::checkpoint_agreement::CompactReplayCandidate::Roster(Box::new(
                        base.roster_vote_coverage_for_manifest(
                            &tail,
                            roster.manifest(),
                            &certificates,
                        )
                        .unwrap(),
                    ))
                },
            );
        }
        base = if let chain::CertifiedEvent::Selection(source) = event {
            let manifest =
                bincode::deserialize(&bincode::serialize(&source.replayed().manifest()).unwrap())
                    .unwrap();
            let (next, mut selection) = base
                .replay_selection(tail, event.certificate(), &manifest, &certificates)
                .unwrap();
            assert_eq!(selection.replayed().request(), source.replayed().request());
            assert_eq!(
                selection.replayed().lifecycle(),
                source.replayed().lifecycle()
            );
            assert!(selection.replayed().history_hashes().is_err());
            assert!(selection
                .replayed()
                .validate_parent(selection.parent())
                .is_err());
            if let Some(beacon) = source.beacon() {
                selection.attach_compact_beacon(beacon.clone()).unwrap();
            }
            recovered.push(selection);
            next
        } else {
            // Deserializing a manifest grants no trusted event constructor.
            let manifest =
                bincode::deserialize(&bincode::serialize(&event.manifest()).unwrap()).unwrap();
            base.replay_manifest(tail, event.certificate(), &manifest, &certificates)
                .unwrap()
        };
        assert_eq!(base.anchor().hash(), checkpoint.hash());
        let full = ConfirmedReplay::replay_with_events(
            &history,
            NetworkId::Dev,
            &fixture.params,
            &events[..=index],
        )
        .unwrap();
        compact::assert_same(base.state(), &full);
        assert_eq!(base.anchor().height(), checkpoint.height);
    }
    omitted_ancestors
}
