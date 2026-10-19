use super::*;

pub(in crate::confirmed_replay::tests) mod live;
mod publication;
mod queued;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_installation_prepares_only_the_next_epoch_without_writes() {
    let (mut fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let next =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    fixture.push(
        1,
        BlockType::ChangeRepresentative,
        next.anchor().cutoff() + 2,
        |block| {
            block.payment_epoch = next.anchor().hash();
            block.representative = address(0);
        },
    );
    fixture.append_confirmed_prefix(&source, next.anchor().cutoff() + 3);
    let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
        &source,
        NetworkId::Dev,
        &fixture.params,
        next.anchor().cutoff() + 4,
    )
    .unwrap()
    .unwrap()
    .manifest()
    .clone();
    let roster_certificate = certified_votes::certify(&roster.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    crate::checkpoint_store::chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &fixture.params,
        roster_certificate.clone(),
        &crate::checkpoint_agreement::ReplayManifest::Roster(roster),
    )
    .unwrap();
    let latest = authenticate(
        &source,
        &fixture.params,
        roster_certificate.checkpoint.hash(),
    )
    .into_continuation();
    let source = Arc::new(source);
    for height in [1, 2, 3] {
        let current = match height {
            1 => &old,
            2 => &next,
            _ => &latest,
        };
        let (_dir, local) = cemented::sparse(current, &source);
        records(&local, &source, &fixture, &bytes);
        let key = format!("checkpoint_chain/v1/dev/{}/3", old.anchor().genesis_hash());
        local
            .meta_store()
            .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
            .unwrap();
        let suffix = fixture.blocks.last().unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("checkpoint_history/v1/{}", suffix.hash),
                &bincode::serialize(suffix).unwrap(),
            )
            .unwrap();
        assert!(
            crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &fixture.params).is_err()
        );
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = Service::start(source.clone(), local.clone(), 0).await;
        let metadata = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        let candidate = metadata
            .prepare_next_from_peers(local.clone(), service.registry.clone())
            .await
            .unwrap();
        if height == 3 {
            assert!(candidate.is_none());
        } else {
            let candidate = candidate.unwrap();
            let expected = if height == 1 { &next } else { &latest };
            assert_eq!(candidate.anchor().hash(), expected.anchor().hash());
            compact::assert_same(candidate.state(), expected.state());
            let batch = local.tx_begin_write().unwrap();
            candidate.verify_unchanged(&batch).unwrap();
        }
        let requests = service.stop().await;
        if height == 2 {
            assert!(requests > 0);
        }
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            current.anchor().hash()
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_installation_rechecks_the_writer_after_preparation() {
    let (fixture, _source_dir, source, manifest, _) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let source = Arc::new(source);
    for key in [
        installed::BASE_KEY.to_string(),
        burst_store::meta::PAYMENT_EPOCH_KEY.to_string(),
        crate::confirmed_replay::installation::CUTOFF_KEY.to_string(),
        format!(
            "checkpoint_chain/v1/dev/{}/head",
            old.anchor().genesis_hash()
        ),
        format!("checkpoint_chain/v1/dev/{}/2", old.anchor().genesis_hash()),
    ] {
        let (_dir, local) = cemented::sparse(&old, &source);
        records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = Service::start(source.clone(), local.clone(), 0).await;
        let metadata = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        let candidate = metadata
            .prepare_next_from_peers(local.clone(), service.registry.clone())
            .await
            .unwrap()
            .unwrap();
        service.stop().await;
        local.meta_store().put_meta(&key, &[99; 32]).unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            candidate
                .verify_unchanged(&local.tx_begin_write().unwrap())
                .is_err(),
            "{key}"
        );
        assert!(candidate.commit(&local).is_err(), "{key}");
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
