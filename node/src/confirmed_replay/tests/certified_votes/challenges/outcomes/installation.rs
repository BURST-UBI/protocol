use super::*;
use crate::checkpoint_store::chain as certificate_chain;
use crate::confirmed_replay::installation::PreparedInstallation;

// Supply already-confirmed signed prefixes in order. This tests durable state
// installation, not the running node's admission/election/cementation workers.
fn append_confirmed_prefix(fixture: &Fixture, store: &LmdbStore, cutoff: u64) {
    let chain = certificate_chain::load(store, NetworkId::Dev, &fixture.params).unwrap();
    let history = fixture.history(cutoff);
    let state =
        ConfirmedReplay::replay_with_events(&history, NetworkId::Dev, &fixture.params, &chain)
            .unwrap();
    let mut heights = HashMap::new();
    let mut batch = store.tx_begin_write().unwrap();
    for block in &fixture.blocks {
        let height = heights.entry(block.account.clone()).or_insert(0u64);
        *height += 1;
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                *height,
            )
            .unwrap();
    }
    for account in state.accounts.values() {
        batch.put_account_info(account, false).unwrap();
        if account.block_count > 0 {
            batch.put_frontier(&account.address, &account.head).unwrap();
        }
    }
    // Runtime recovery checks the complete published projection, not only
    // account afterimages. Keep the installed authority at its certified cut.
    let old_pending: Vec<_> = batch
        .pending_entries()
        .unwrap()
        .map(|row| {
            let (key, _) = row.unwrap();
            let (address, source) = key.split_at(key.len() - 32);
            (
                WalletAddress::new(std::str::from_utf8(address).unwrap()),
                <[u8; 32]>::try_from(source).unwrap(),
            )
        })
        .collect();
    for (address, source) in old_pending {
        batch.delete_pending(&address, &source).unwrap();
    }
    for ((address, source), pending) in &state.pending {
        batch
            .put_pending(
                address,
                source.as_bytes(),
                &bincode::serialize(pending).unwrap(),
            )
            .unwrap();
    }
    batch.clear_checkpoint_lifecycle().unwrap();
    for (key, value) in state.projected_lifecycle_metadata() {
        batch.put_meta(key, value).unwrap();
    }
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &state.verification.snapshot().encode().unwrap(),
        )
        .unwrap();
    crate::ledger_updater::persist_economics(&mut batch, &state.brn, &state.trst).unwrap();
    batch
        .put_brn_meta(
            burst_verification::bootstrap::BootstrapPhase::KEY,
            &state.bootstrap_phase.encoding(),
        )
        .unwrap();
    batch
        .put_brn_meta(
            b"protocol_params",
            &bincode::serialize(&state.params).unwrap(),
        )
        .unwrap();
    state
        .governance
        .as_ref()
        .unwrap()
        .persist(&mut batch)
        .unwrap();
    batch.commit().unwrap();
}

pub(super) fn installed_chain(
    full: &Fixture,
    selections: &[CertifiedCheckpoint],
) -> (tempfile::TempDir, LmdbStore, Vec<HistoryHead>) {
    let first_prefix = Fixture {
        params: full.params.clone(),
        blocks: full
            .blocks
            .iter()
            .filter(|block| {
                selections[0]
                    .replayed()
                    .history_hashes()
                    .unwrap()
                    .contains(&block.hash)
            })
            .cloned()
            .collect(),
    };
    let (dir, store, _) = first_prefix.store();
    for (index, selection) in selections.iter().enumerate() {
        if index > 0 {
            let prefix = Fixture {
                params: full.params.clone(),
                blocks: full
                    .blocks
                    .iter()
                    .filter(|block| {
                        selection
                            .replayed()
                            .history_hashes()
                            .unwrap()
                            .contains(&block.hash)
                    })
                    .cloned()
                    .collect(),
            };
            append_confirmed_prefix(&prefix, &store, selection.certificate().checkpoint.cutoff);
        }
        let mut accepted = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &full.params,
            selection.certificate().clone(),
            selection.heads().to_vec(),
            address(20),
        )
        .unwrap();
        accepted
            .attach_beacon(&store, &full.params, selection.beacon().unwrap().clone())
            .unwrap();
        let installed = PreparedInstallation::next(&store, NetworkId::Dev, &full.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
        assert_eq!(
            installed.anchor.hash(),
            selection.certificate().checkpoint.hash()
        );
        assert!(installed.retired_blocks.is_empty());
        assert_eq!(
            installed.anchor.roster().total_humans(),
            if index == 0 { 9 } else { 10 }
        );
        assert!(
            PreparedInstallation::next(&store, NetworkId::Dev, &full.params)
                .unwrap()
                .is_none()
        );
    }
    append_confirmed_prefix(full, &store, CUTOFF + 70);
    let heads = full.history(CUTOFF + 70).heads().to_vec();
    (dir, store, heads)
}

#[test]
fn checkpoint_installer_commits_successive_selections_and_fraud_settlement_once() {
    let (mut full, selections) = completed(ChallengeReason::Fraud);
    merged_payments(&mut full);
    let (dir, store, heads) = installed_chain(&full, &selections);
    let candidate = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &full.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certify(&candidate.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 20]);
    let outcome = candidate
        .certify(certificate)
        .unwrap()
        .persist(&store, NetworkId::Dev, &full.params)
        .unwrap();
    let expected_accounts = outcome.state().accounts.clone();
    let expected_trst = outcome.state().trst.save_wallets();
    let expected_pending = outcome.state().pending.clone();
    let nominal_balance = store
        .account_store()
        .get_account(&address(8))
        .unwrap()
        .trst_balance;
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &full.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(
        installed.anchor.hash(),
        outcome.certificate().checkpoint.hash()
    );
    assert_eq!(installed.anchor.roster().total_humans(), 9);
    assert_eq!(installed.anchor.roster().weight(&address(20)), 0);
    assert_eq!(installed.trst.save_wallets(), expected_trst);
    for (address, account) in expected_accounts {
        assert_eq!(
            bincode::serialize(&store.account_store().get_account(&address).unwrap()).unwrap(),
            bincode::serialize(&account).unwrap()
        );
    }
    for ((address, hash), pending) in expected_pending {
        assert_eq!(
            bincode::serialize(&store.pending_store().get_pending(&address, &hash).unwrap())
                .unwrap(),
            bincode::serialize(&pending).unwrap()
        );
    }
    let reward_source = full
        .blocks
        .iter()
        .filter(|block| {
            block.block_type == BlockType::VerificationVote
                && block.timestamp == Timestamp::new(CUTOFF + 60)
        })
        .max_by_key(|block| *block.hash.as_bytes())
        .unwrap()
        .hash;
    let reward = store
        .pending_store()
        .get_pending(&address(8), &reward_source.into_tx_hash())
        .unwrap();
    assert!(reward.amount > 0);
    full.push(8, BlockType::Receive, CUTOFF + 71, |block| {
        block.payment_epoch = outcome.certificate().checkpoint.hash();
        block.link = reward_source;
        block.trst_balance += reward.amount;
    });
    let batch = store.tx_begin_write().unwrap();
    let dependencies =
        crate::verification_commit::dependencies::read(&batch, full.blocks.last().unwrap())
            .unwrap();
    assert!(dependencies.contains(&reward_source));
    for source in full.blocks.iter().filter(|block| {
        matches!(
            block.block_type,
            BlockType::Challenge | BlockType::VerificationVote
        ) && block.timestamp >= Timestamp::new(CUTOFF + 34)
            && block.timestamp <= Timestamp::new(CUTOFF + 60)
    }) {
        assert!(dependencies.contains(&source.hash));
    }
    drop(batch);
    assert_eq!(
        store
            .account_store()
            .get_account(&address(8))
            .unwrap()
            .trst_balance,
        nominal_balance
    );
    assert_eq!(
        store
            .account_store()
            .get_account(&address(20))
            .unwrap()
            .state,
        WalletState::Revoked
    );
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &full.params)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
        expected_trst
    );
}
