use super::*;
use burst_store::{account::AccountStore, block::BlockStore};

pub(super) fn check_membership(
    roster: &burst_consensus::checkpoint::HumanRoster,
    population: u32,
) -> Result<(), String> {
    if roster.total_humans() != u64::from(population) {
        return Err("saved electorate population differs from authenticated roster".into());
    }
    for representative in 0..3 {
        let expected = (u64::from(population) + 2 - u64::from(representative)) / 3;
        if roster.weight(&address(representative)) != expected {
            return Err("saved electorate representative weight differs".into());
        }
    }
    let expected: HashSet<_> = (0..population)
        .map(|index| {
            burst_crypto::derive_address(
                &burst_crypto::keypair_from_seed(
                    &crate::confirmed_replay::tests::electorate::seed(index),
                )
                .public,
            )
        })
        .collect();
    if expected.len() != population as usize
        || roster
            .humans()
            .iter()
            .map(|entry| entry.human.clone())
            .collect::<HashSet<_>>()
            != expected
    {
        return Err("saved electorate contains unexpected or duplicate humans".into());
    }
    Ok(())
}

pub(super) fn restore(
    store: &LmdbStore,
    baseline: &Path,
    target: u32,
) -> Result<(Fixture, u32, BlockHash, u64), String> {
    // The target is fixture metadata, independent of the bootstrap threshold.
    let bytes = std::fs::read(baseline.join("resource-probe.bin")).map_err(|e| e.to_string())?;
    let (version, params, epoch, population, now, saved_target): (
        u8,
        ProtocolParams,
        BlockHash,
        u32,
        u64,
        u32,
    ) = bincode::deserialize(&bytes).map_err(|e| e.to_string())?;
    if version != 2 || saved_target != target || !(3..=target).contains(&population) {
        return Err("saved electorate target or population is incompatible".into());
    }
    restore_at(store, params, epoch, population, now)
}

fn restore_at(
    store: &LmdbStore,
    params: ProtocolParams,
    epoch: BlockHash,
    population: u32,
    now: u64,
) -> Result<(Fixture, u32, BlockHash, u64), String> {
    let base = installed::load_base(store, NetworkId::Dev, &params)
        .map_err(|e| e.to_string())?
        .ok_or("saved electorate has no authenticated snapshot")?
        .into_continuation();
    if base.anchor().hash() != epoch || base.anchor().cutoff().checked_add(1) != Some(now) {
        return Err("saved electorate epoch or clock differs from authenticated snapshot".into());
    }
    check_membership(base.anchor().roster(), population)?;
    if base.state().accounts.len() != population as usize
        || store
            .account_store()
            .account_count()
            .map_err(|e| e.to_string())?
            != u64::from(population)
    {
        return Err("saved electorate has uncheckpointed accounts".into());
    }
    for account in base.state().accounts.values() {
        if store
            .account_store()
            .get_account(&account.address)
            .map_err(|e| e.to_string())?
            != *account
        {
            return Err("saved electorate has uncheckpointed account changes".into());
        }
    }
    let fixture = authenticated_heads(store, &base, params)?;
    let plan = base.plan_cemented_tail(store).map_err(|e| e.to_string())?;
    let proofs = crate::checkpoint_sync::witness::read_plan(store, &base, plan.witness_plan())
        .map_err(|e| e.to_string())?;
    let tail = plan.finish(proofs).map_err(|e| e.to_string())?;
    base.verify_installed_metadata(store, &tail)
        .map_err(|e| e.to_string())?;
    tail.verify_unchanged(&store.tx_begin_write().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok((fixture, population, epoch, now))
}

pub(super) fn authenticated_heads(
    store: &LmdbStore,
    base: &crate::confirmed_replay::snapshot::SnapshotContinuation,
    params: ProtocolParams,
) -> Result<Fixture, String> {
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let mut heads = Vec::with_capacity(base.state().accounts.len());
    for account in base.state().accounts.values() {
        let stored = store
            .account_store()
            .get_account(&account.address)
            .map_err(|e| e.to_string())?;
        if (stored.head, stored.block_count, stored.confirmation_height)
            != (
                account.head,
                account.block_count,
                account.confirmation_height,
            )
        {
            return Err("saved electorate has uncheckpointed account heads".into());
        }
        let bytes = store
            .block_store()
            .get_block(&account.head)
            .map_err(|e| e.to_string())?;
        let mut cursor = base
            .history_cursor(&account.address)
            .map_err(|e| e.to_string())?
            .ok_or("saved electorate account has no authenticated history")?;
        let proofs = cursor.accept(&[bytes]).map_err(|e| e.to_string())?;
        if proofs.len() != 1 || proofs[0].block().hash != account.head {
            return Err("saved electorate head proof differs".into());
        }
        if account.head != genesis.hash {
            heads.push(proofs[0].block().clone());
        }
    }
    heads.sort_by_key(|block| (block.timestamp.as_secs(), *block.hash.as_bytes()));
    let mut blocks = vec![genesis];
    blocks.extend(heads);
    Ok(Fixture { params, blocks })
}

/// Finish only already-certified work on a private copy. Ordinary resume keeps
/// rejecting uncheckpointed tails; this path never signs a replacement candidate.
pub(super) fn finish_accepted(baseline: &Path, target: u32, expected: u32) -> PathBuf {
    finish_saved(baseline, target, expected, false)
}

/// Authenticate the already-stored signed suffix before certifying it on a copy.
/// This is distinct from recovery of an already accepted checkpoint.
pub(super) fn finish_suffix(baseline: &Path, target: u32, expected: u32) -> PathBuf {
    finish_saved(baseline, target, expected, true)
}

fn finish_saved(baseline: &Path, target: u32, expected: u32, accept_suffix: bool) -> PathBuf {
    let metadata = std::fs::read(baseline.join("resource-probe.bin")).unwrap();
    let (version, params, epoch, population, now, saved_target): (
        u8,
        ProtocolParams,
        BlockHash,
        u32,
        u64,
        u32,
    ) = bincode::deserialize(&metadata).unwrap();
    assert_eq!(version, 2);
    assert_eq!(saved_target, target);
    assert!(population < expected && expected <= target);
    let directory = tempfile::tempdir().unwrap();
    std::fs::copy(baseline.join("data.mdb"), directory.path().join("data.mdb")).unwrap();
    let path = directory.keep();
    eprintln!("electorate accepted-recovery working={}", path.display());
    let store = LmdbStore::open(&path, 32, super::configured_map_size()).unwrap();
    let snapshot = installed::load_base(&store, NetworkId::Dev, &params)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.anchor().hash(), epoch);
    assert_eq!(snapshot.anchor().cutoff().checked_add(1), Some(now));
    check_membership(snapshot.anchor().roster(), population).unwrap();
    let mut recovered = snapshot
        .recover_stored_successors(&store, |base, plan, _| {
            crate::checkpoint_sync::witness::read_plan(&store, base, plan)
        })
        .unwrap();
    if accept_suffix {
        let started = Instant::now();
        assert_eq!(
            recovered.continuation.anchor().hash(),
            epoch,
            "suffix recovery must start before certificate acceptance"
        );
        assert!(expected - population <= 4096);
        assert_eq!(
            store.account_store().account_count().unwrap(),
            u64::from(expected)
        );
        let last_address = burst_crypto::derive_address(
            &burst_crypto::keypair_from_seed(&crate::confirmed_replay::tests::electorate::seed(
                expected - 1,
            ))
            .public,
        );
        let last_account = store.account_store().get_account(&last_address).unwrap();
        assert_eq!(last_account.block_count, 1);
        let last: StateBlock =
            bincode::deserialize(&store.block_store().get_block(&last_account.head).unwrap())
                .unwrap();
        assert_eq!(last.account, last_address);
        assert_eq!(last.block_type, BlockType::Open);
        assert_eq!(last.payment_epoch, epoch);
        let signing_now = last.timestamp.as_secs().checked_add(2).unwrap();
        let base = &recovered.continuation;
        let tail = cemented::finish(base, &store, &store);
        let candidate = base
            .prepare_next_cemented_candidate(
                &store,
                &tail,
                &recovered.selections,
                SigningClock {
                    now: signing_now,
                    error_bound: 0,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(&candidate, CompactReplayCandidate::Roster(_)));
        eprintln!(
            "electorate saved-suffix stage=authenticated population={expected} elapsed_ms={}",
            started.elapsed().as_millis()
        );
        let certificate =
            certified_votes::certify(candidate.input().manifest().checkpoint(), &[0, 1, 2]);
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            candidate.input(),
            &certificate,
        )
        .unwrap();
        eprintln!(
            "electorate saved-suffix stage=accepted population={expected} elapsed_ms={}",
            started.elapsed().as_millis()
        );
        recovered = recovered
            .advance_locally(&store, certificate.checkpoint.hash())
            .unwrap();
    }
    let base = &recovered.continuation;
    check_membership(base.anchor().roster(), expected).unwrap();
    assert_eq!(base.state().accounts.len(), expected as usize);
    assert_eq!(
        store.account_store().account_count().unwrap(),
        u64::from(expected)
    );
    let next_epoch = base.anchor().hash();
    assert_ne!(next_epoch, epoch);
    let next_now = base.anchor().cutoff().checked_add(1).unwrap();
    let fixture = authenticated_heads(&store, base, params.clone()).unwrap();
    drop(recovered);
    install(&fixture, &store, next_epoch, &[0, 1, 2]);
    // Reuse every ordinary resume check before publishing new fixture metadata.
    restore_at(&store, params.clone(), next_epoch, expected, next_now).unwrap();
    save(
        &path, &store, &params, next_epoch, expected, next_now, target,
    );
    drop(store);
    let reopened = LmdbStore::open(&path, 32, super::configured_map_size()).unwrap();
    restore(&reopened, &path, target).unwrap();
    eprintln!("electorate accepted-recovery verified population={expected} epoch={next_epoch}");
    path
}

pub(super) fn save(
    path: &Path,
    store: &LmdbStore,
    params: &ProtocolParams,
    epoch: BlockHash,
    population: u32,
    now: u64,
    target: u32,
) {
    store.force_sync().unwrap();
    std::fs::write(
        path.join("resource-probe.bin"),
        bincode::serialize(&(2u8, params, epoch, population, now, target)).unwrap(),
    )
    .unwrap();
}

#[test]
fn electorate_resume_preserves_source_and_rejects_uncheckpointed_tail() {
    let first = grow(257, 128, 131, None, true, None).unwrap();
    let source_hash = burst_crypto::blake2b_256(&std::fs::read(first.join("data.mdb")).unwrap());
    let metadata = std::fs::read(first.join("resource-probe.bin")).unwrap();
    let next = grow(257, 128, 257, Some(&first), true, None).unwrap();
    assert_eq!(
        source_hash,
        burst_crypto::blake2b_256(&std::fs::read(first.join("data.mdb")).unwrap())
    );
    assert_eq!(
        metadata,
        std::fs::read(first.join("resource-probe.bin")).unwrap()
    );
    let copy = tempfile::tempdir().unwrap();
    std::fs::copy(first.join("data.mdb"), copy.path().join("data.mdb")).unwrap();
    let store = LmdbStore::open(copy.path(), 32, super::configured_map_size()).unwrap();
    assert!(restore(&store, &first, 258)
        .err()
        .unwrap()
        .contains("target"));
    let (mut fixture, _, epoch, now) = restore(&store, &first, 257).unwrap();
    fixture.push(0, BlockType::ChangeRepresentative, now, |block| {
        block.payment_epoch = epoch;
        block.representative = address(1);
    });
    cemented::append(&store, fixture.blocks.last().unwrap(), true);
    assert!(restore(&store, &first, 257)
        .err()
        .unwrap()
        .contains("uncheckpointed"));
    drop(store);
    std::fs::remove_dir_all(first).unwrap();
    std::fs::remove_dir_all(next).unwrap();
}

#[test]
fn accepted_electorate_recovery_projects_state_before_strict_resume() {
    let source = grow(257, 128, 131, None, true, None).unwrap();
    // This case enrolls through 257; 385 is the saved future target.
    {
        let store = LmdbStore::open(&source, 32, super::configured_map_size()).unwrap();
        let (fixture, population, epoch, now) = restore(&store, &source, 257).unwrap();
        save(
            &source,
            &store,
            &fixture.params,
            epoch,
            population,
            now,
            385,
        );
    }
    let metadata = std::fs::read(source.join("resource-probe.bin")).unwrap();
    let pending = tempfile::tempdir().unwrap();
    std::fs::copy(source.join("data.mdb"), pending.path().join("data.mdb")).unwrap();
    std::fs::write(pending.path().join("resource-probe.bin"), &metadata).unwrap();
    let store = LmdbStore::open(pending.path(), 32, super::configured_map_size()).unwrap();
    let (mut fixture, population, epoch, _) = restore(&store, pending.path(), 385).unwrap();
    let recovered = installed::load_base(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .recover_stored_successors(&store, |base, plan, _| {
            crate::checkpoint_sync::witness::read_plan(&store, base, plan)
        })
        .unwrap();
    let base = &recovered.continuation;
    let start = fixture.blocks.len();
    let cutoff = fixture.extend_electorate(&store, population..257, epoch, |index| {
        address((index % 3) as u8)
    });
    cemented::append_batch(&store, &fixture.blocks[start..], true);
    let tail = cemented::finish(base, &store, &store);
    let candidate = base
        .prepare_next_cemented_candidate(
            &store,
            &tail,
            &recovered.selections,
            SigningClock {
                now: cutoff + 1,
                error_bound: 0,
            },
        )
        .unwrap()
        .unwrap();
    let certificate =
        certified_votes::certify(candidate.input().manifest().checkpoint(), &[0, 1, 2]);
    crate::checkpoint_store::compact::accept_candidate(
        &store,
        base.anchor(),
        candidate.input(),
        &certificate,
    )
    .unwrap();
    assert!(restore(&store, pending.path(), 385)
        .err()
        .unwrap()
        .contains("uncheckpointed"));
    drop(store);
    let before =
        burst_crypto::blake2b_256(&std::fs::read(pending.path().join("data.mdb")).unwrap());
    let completed = finish_accepted(pending.path(), 385, 257);
    assert_eq!(
        before,
        burst_crypto::blake2b_256(&std::fs::read(pending.path().join("data.mdb")).unwrap())
    );
    assert_eq!(
        metadata,
        std::fs::read(pending.path().join("resource-probe.bin")).unwrap()
    );
    let repaired = LmdbStore::open(&completed, 32, super::configured_map_size()).unwrap();
    let (_, population, repaired_epoch, _) = restore(&repaired, &completed, 385).unwrap();
    assert_eq!(population, 257);
    assert_eq!(repaired_epoch, certificate.checkpoint.hash());
    drop(repaired);
    std::fs::remove_dir_all(completed).unwrap();
    std::fs::remove_dir_all(source).unwrap();
}

#[test]
fn stored_suffix_recovery_authenticates_before_acceptance_and_reaches_final_target() {
    let source = grow(257, 128, 131, None, true, None).unwrap();
    let metadata = std::fs::read(source.join("resource-probe.bin")).unwrap();
    let pending = tempfile::tempdir().unwrap();
    std::fs::copy(source.join("data.mdb"), pending.path().join("data.mdb")).unwrap();
    std::fs::write(pending.path().join("resource-probe.bin"), &metadata).unwrap();
    let store = LmdbStore::open(pending.path(), 32, super::configured_map_size()).unwrap();
    let (mut fixture, population, epoch, _) = restore(&store, pending.path(), 257).unwrap();
    let first = fixture.blocks.len();
    fixture.extend_electorate(&store, population..257, epoch, |index| {
        address((index % 3) as u8)
    });
    for block in &fixture.blocks[first..] {
        cemented::append(&store, block, true);
    }
    assert!(restore(&store, pending.path(), 257)
        .err()
        .unwrap()
        .contains("uncheckpointed"));
    store.force_sync().unwrap();
    drop(store);
    let before =
        burst_crypto::blake2b_256(&std::fs::read(pending.path().join("data.mdb")).unwrap());
    let completed = finish_suffix(pending.path(), 257, 257);
    assert_eq!(
        before,
        burst_crypto::blake2b_256(&std::fs::read(pending.path().join("data.mdb")).unwrap())
    );
    assert_eq!(
        metadata,
        std::fs::read(pending.path().join("resource-probe.bin")).unwrap()
    );
    let reopened = LmdbStore::open(&completed, 32, super::configured_map_size()).unwrap();
    let (_, actual, next_epoch, _) = restore(&reopened, &completed, 257).unwrap();
    assert_eq!(actual, 257);
    assert_ne!(next_epoch, epoch);
    drop(reopened);
    std::fs::remove_dir_all(completed).unwrap();
    std::fs::remove_dir_all(source).unwrap();
}
