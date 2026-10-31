use super::*;
use crate::checkpoint_agreement::{self, Command};
use crate::checkpoint_replay::SigningClock;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_journal_recovers_from_peer_proofs_and_rejects_concurrent_changes() {
    let (mut fixture, _source_dir, source, original, _) = selection::setup();
    let parent = original.checkpoint.parent;
    let cutoff = original.checkpoint.cutoff;
    fixture.push(0, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = parent;
    });
    let block = fixture.blocks.last().unwrap();
    let (_full_dir, full, heads) = fixture.store();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    full.meta_store()
        .put_meta(
            &format!("{prefix}/1"),
            &source
                .meta_store()
                .get_meta(&format!("{prefix}/1"))
                .unwrap(),
        )
        .unwrap();
    full.meta_store()
        .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
        .unwrap();
    let manifest = CertifiedCheckpoint::prepare(
        &full,
        NetworkId::Dev,
        &fixture.params,
        cutoff,
        &heads,
        &address(20),
    )
    .unwrap()
    .manifest();
    let later = CertifiedCheckpoint::prepare(
        &full,
        NetworkId::Dev,
        &fixture.params,
        cutoff + 1,
        &heads,
        &address(20),
    )
    .unwrap()
    .manifest();
    let source = Arc::new(source);
    for case in 0..8 {
        let snapshot = authenticate(&source, &fixture.params, parent);
        let base = authenticate(&source, &fixture.params, parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("checkpoint_history/v1/{}", block.hash),
                &bincode::serialize(block).unwrap(),
            )
            .unwrap();
        let seed = (0..9)
            .find(|seed| address(*seed) == *base.anchor().proposer(0))
            .unwrap();
        let signer = burst_crypto::keypair_from_seed(&[seed; 32]);
        let representative = address(seed);
        let mut proposal = Vec::new();
        if case != 0 {
            let plan = base.plan_tail(&local, cutoff, &manifest.heads).unwrap();
            let retained = witness_transfer::exchange(
                crate::checkpoint_sync::witness::Roots::for_plan(&base, &plan).unwrap(),
                plan.witness_hashes().to_vec(),
                &source,
                &local,
            )
            .unwrap();
            let candidate = base
                .selection_vote_coverage(plan.finish(retained).unwrap(), &manifest, &[])
                .unwrap();
            proposal = checkpoint_agreement::apply_compact_selections(
                &local,
                base.anchor(),
                &signer.private,
                Command::Propose(manifest.checkpoint.clone()),
                &[&candidate],
                Some(SigningClock {
                    now: cutoff,
                    error_bound: 0,
                }),
            )
            .unwrap();
            if case >= 4 {
                let alternative = if case == 7 {
                    let plan = base.plan_tail(&local, cutoff + 1, &later.heads).unwrap();
                    let retained = witness_transfer::exchange(
                        crate::checkpoint_sync::witness::Roots::for_plan(&base, &plan).unwrap(),
                        plan.witness_hashes().to_vec(),
                        &source,
                        &local,
                    )
                    .unwrap();
                    Some(
                        base.selection_vote_coverage(plan.finish(retained).unwrap(), &later, &[])
                            .unwrap(),
                    )
                } else {
                    None
                };
                let mut choices = vec![&candidate];
                if let Some(alternative) = &alternative {
                    choices.push(alternative);
                }
                let decided = alternative.as_ref().unwrap_or(&candidate);
                let certificate =
                    certified_votes::certify(decided.replay().checkpoint(), &[0, 1, 2, 3, 4, 5, 6]);
                if case != 6 {
                    proposal = checkpoint_agreement::apply_compact_selections(
                        &local,
                        base.anchor(),
                        &signer.private,
                        Command::Decide(certificate.clone()),
                        &choices,
                        None,
                    )
                    .unwrap();
                }
                crate::checkpoint_store::compact::accept_selection(
                    &local,
                    base.anchor(),
                    decided,
                    &certificate,
                )
                .unwrap();
                if case == 5 {
                    // Relay must not require the old payment epoch to remain
                    // installed. This fixture changes the marker only.
                    local
                        .meta_store()
                        .put_meta(
                            burst_store::meta::PAYMENT_EPOCH_KEY,
                            manifest.checkpoint.hash().as_bytes(),
                        )
                        .unwrap();
                }
            }
        }
        drop(local);
        let local = LmdbStore::open(_dir.path(), 32, 32 << 20).unwrap();
        let recovered = snapshot
            .recover_stored_epoch(&local, parent, |_, _, _| unreachable!())
            .unwrap();
        let journal_key = format!(
            "checkpoint_round/v1/dev/{}/{representative}/2",
            base.anchor().genesis_hash()
        );
        if case == 2 {
            local
                .meta_store()
                .put_meta(&journal_key, b"corrupt")
                .unwrap();
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let service = WitnessService::start(
            Arc::clone(&source),
            Arc::clone(&local),
            if case == 3 { 5 } else { 0 },
        )
        .await;
        let result = checkpoint_agreement::recover_compact_selections_from_peers(
            Arc::clone(&local),
            Arc::clone(&service.registry),
            recovered,
            representative,
        )
        .await;
        let requests = service.stop().await;
        assert_eq!(requests > 0, case == 1 || case >= 3);
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        if case == 3 {
            assert!(result
                .err()
                .unwrap()
                .to_string()
                .contains("journal changed during recovery"));
            let unchanged = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
                rows.into_iter()
                    .filter(|(key, _)| key != journal_key.as_bytes())
                    .collect::<Vec<_>>()
            };
            assert_eq!(unchanged(before), unchanged(after));
        } else {
            assert_eq!(before, after);
            if case == 2 || case == 6 {
                assert!(result.is_err());
            } else {
                let (recovered, candidates) = result.unwrap();
                compact::assert_same(recovered.continuation.state(), base.state());
                assert_eq!(
                    candidates.len(),
                    if case == 7 { 2 } else { usize::from(case != 0) }
                );
                if case != 0 {
                    assert_eq!(candidates[0].replay().manifest(), manifest);
                    let resumed = checkpoint_agreement::apply_compact_selections(
                        &local,
                        base.anchor(),
                        &signer.private,
                        Command::Resume,
                        &candidates.iter().collect::<Vec<_>>(),
                        None,
                    )
                    .unwrap();
                    assert_eq!(resumed, proposal);
                    if case >= 4 {
                        let before = local
                            .tx_begin_write()
                            .unwrap()
                            .checkpoint_metadata(b"")
                            .unwrap();
                        assert!(checkpoint_agreement::apply_compact_selections(
                            &local,
                            base.anchor(),
                            &signer.private,
                            Command::Propose(manifest.checkpoint.clone()),
                            &candidates.iter().collect::<Vec<_>>(),
                            Some(SigningClock {
                                now: cutoff,
                                error_bound: 0
                            }),
                        )
                        .is_err());
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
            }
        }
    }
}
