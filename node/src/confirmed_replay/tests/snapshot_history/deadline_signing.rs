use super::*;
use crate::checkpoint_agreement::{self, Command, CompactReplayCandidate, Outgoing};
use crate::checkpoint_replay::SigningClock;
use crate::confirmed_replay::snapshot::{tail::SnapshotTail, SnapshotContinuation};
use burst_consensus::checkpoint_round::{Ballot, Phase, Quorum};

pub(super) fn assert_signing(
    base: &SnapshotContinuation,
    tail: &SnapshotTail,
    selections: &[&CertifiedCheckpoint],
    source: &LmdbStore,
    certificate: &burst_consensus::checkpoint::CheckpointCertificate,
) {
    assert_candidate_signing(base, source, certificate, || {
        CompactReplayCandidate::Deadline(Box::new(
            base.deadline_vote_coverage(tail, selections)
                .unwrap()
                .unwrap(),
        ))
    });
}

pub(super) fn assert_candidate_signing(
    base: &SnapshotContinuation,
    source: &LmdbStore,
    certificate: &burst_consensus::checkpoint::CheckpointCertificate,
    recover: impl Fn() -> CompactReplayCandidate,
) {
    let coverage = recover();
    let input = coverage.input();
    let checkpoint = input.checkpoint();
    let anchor = base.anchor();
    let seed = (0..32)
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[seed; 32]);
    let (dir, local) = cemented::sparse(base, source);
    local
        .meta_store()
        .put_meta(
            &format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash()),
            &anchor.height().to_be_bytes(),
        )
        .unwrap();
    let candidates = [input];
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let mut rejected_clocks = vec![
        None,
        Some(SigningClock {
            now: checkpoint.cutoff,
            error_bound: 1,
        }),
    ];
    if matches!(
        input,
        crate::checkpoint_agreement::CompactReplayInput::Activation(_)
    ) {
        rejected_clocks.push(Some(SigningClock {
            now: checkpoint.cutoff + 60,
            error_bound: 0,
        }));
    }
    for clock in rejected_clocks {
        assert!(checkpoint_agreement::apply_compact_candidates(
            &local,
            anchor,
            &key.private,
            Command::Propose(checkpoint.clone()),
            &candidates,
            clock
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
    let clock = Some(SigningClock {
        now: checkpoint.cutoff + 2,
        error_bound: 0,
    });
    let proposed = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Propose(checkpoint.clone()),
        &candidates,
        clock,
    )
    .unwrap();
    let [Outgoing::Proposal(proposal)] = proposed.as_slice() else {
        panic!("missing compact proposal")
    };
    proposal.verify(anchor).unwrap();
    let votes = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Proposal(proposal),
        &candidates,
        clock,
    )
    .unwrap();
    assert!(
        matches!(votes.as_slice(), [Outgoing::Vote(vote)] if vote.ballot.phase == Phase::Prevote)
    );
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(
        checkpoint_agreement::read_retained_manifests(&local, anchor, &address(seed)).unwrap(),
        vec![input.manifest()]
    );
    let recovered = recover();
    let candidates = [recovered.input()];
    let mut ballot = Ballot::precommit(checkpoint, 0);
    ballot.phase = Phase::Prevote;
    let proof = Quorum {
        signatures: (0..32)
            .filter(|seed| anchor.roster().weight(&address(*seed)) > 0)
            .map(|seed| burst_consensus::checkpoint::CheckpointSignature {
                representative: address(seed),
                signature: burst_crypto::sign_message(
                    &ballot.signing_payload(),
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                ),
            })
            .collect(),
        ballot,
    };
    proof.verify(anchor).unwrap();
    let prepared = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Prepared(proposal, &proof),
        &candidates,
        None,
    )
    .unwrap();
    assert!(
        matches!(prepared.as_slice(), [Outgoing::Vote(vote)] if vote.ballot.phase == Phase::Precommit)
    );
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let resumed = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Resume,
        &candidates,
        None,
    )
    .unwrap();
    assert!(resumed.contains(&prepared[0]));
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let accounts = local
        .tx_begin_write()
        .unwrap()
        .bounded_accounts(131_072)
        .unwrap();
    let mut invalid = certificate.clone();
    invalid.signatures.clear();
    assert!(crate::checkpoint_store::compact::accept_candidate(
        &local,
        anchor,
        candidates[0],
        &invalid
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
    crate::checkpoint_store::compact::accept_candidate(&local, anchor, candidates[0], certificate)
        .unwrap();
    let record_key = format!(
        "checkpoint_chain/v1/dev/{}/{}",
        anchor.genesis_hash(),
        checkpoint.height
    );
    assert_eq!(
        local.meta_store().get_meta(&record_key).unwrap(),
        source.meta_store().get_meta(&record_key).unwrap()
    );
    assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
    assert_eq!(
        bincode::serialize(
            &local
                .tx_begin_write()
                .unwrap()
                .bounded_accounts(131_072)
                .unwrap()
        )
        .unwrap(),
        bincode::serialize(&accounts).unwrap()
    );
    let persisted = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    crate::checkpoint_store::compact::accept_candidate(&local, anchor, candidates[0], certificate)
        .unwrap();
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        persisted
    );
    let mut changed_keys: HashSet<_> = candidates[0]
        .archive_rows()
        .map(Result::unwrap)
        .map(|(key, _)| key.as_bytes().to_vec())
        .collect();
    changed_keys.insert(record_key.into_bytes());
    changed_keys
        .insert(format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash()).into_bytes());
    if let crate::checkpoint_agreement::ReplayManifest::Activation(manifest) =
        candidates[0].manifest()
    {
        let key =
            crate::checkpoint_store::archive_selection::beacon_key(manifest.activation.selection);
        let expected = bincode::serialize(&manifest.activation.beacon()).unwrap();
        assert_eq!(
            local.meta_store().get_meta(&key).unwrap(),
            expected,
            "activation must retain its exact authenticated selection beacon"
        );
        if let Some((_, old)) = before.iter().find(|(stored, _)| stored == key.as_bytes()) {
            assert_eq!(
                old, &expected,
                "activation must not replace a different beacon"
            );
        }
        changed_keys.insert(key.into_bytes());
    }
    let economic_rows = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
        rows.into_iter()
            .filter(|(key, _)| !changed_keys.contains(key))
            .collect::<Vec<_>>()
    };
    assert_eq!(economic_rows(persisted), economic_rows(before));
    let (archive_key, archive_bytes) = candidates[0].archive_rows().next().unwrap().unwrap();
    local
        .meta_store()
        .put_meta(&archive_key, b"damaged archive")
        .unwrap();
    let damaged = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(crate::checkpoint_store::compact::accept_candidate(
        &local,
        anchor,
        candidates[0],
        certificate
    )
    .is_err());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        damaged
    );
    local
        .meta_store()
        .put_meta(&archive_key, &archive_bytes)
        .unwrap();
    let decided = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Decide(certificate.clone()),
        &candidates,
        None,
    )
    .unwrap();
    assert!(decided
        .iter()
        .any(|item| matches!(item, Outgoing::Decision(saved) if saved == certificate)));
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let resumed = checkpoint_agreement::apply_compact_candidates(
        &local,
        anchor,
        &key.private,
        Command::Resume,
        &candidates,
        None,
    )
    .unwrap();
    assert_eq!(resumed, vec![Outgoing::Decision(certificate.clone())]);
    crate::checkpoint_store::compact::accept_candidate(&local, anchor, candidates[0], certificate)
        .unwrap();
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}
