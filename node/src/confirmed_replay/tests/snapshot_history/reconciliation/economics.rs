use super::*;
use crate::confirmed_replay::{
    installation::{CUTOFF_KEY, INSTALLED_KEY},
    snapshot::installed,
};
use burst_store_lmdb::block_undo::UndoTable;

#[test]
fn snapshot_reconciliation_accepts_started_genesis_authority_but_rejects_damaged_markers() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for case in 0..5 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local) = local_genesis(&fixture);
        let authority =
            crate::payment_authority::initialize(&local, NetworkId::Dev, &fixture.params).unwrap();
        assert_eq!(authority.epoch, fixture.blocks[0].hash);
        match case {
            1 => local.meta_store().delete_meta(CUTOFF_KEY).unwrap(),
            2 => local
                .meta_store()
                .put_meta(CUTOFF_KEY, &1u64.to_be_bytes())
                .unwrap(),
            3 => local
                .meta_store()
                .delete_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)
                .unwrap(),
            4 => local
                .meta_store()
                .delete_meta(crate::confirmed_governance::HEADER)
                .unwrap(),
            _ => (),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let proofs = witnesses(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
        let result = snapshot.reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        );
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        drop(result);
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

#[test]
fn compact_economic_projection_matches_full_installation_without_a_prefix() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let expected = ConfirmedReplay::replay_with_events(
        &fixture.history(certificate.checkpoint.cutoff),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    let expected_mints: HashMap<_, _> = fixture
        .blocks
        .iter()
        .filter(|b| b.block_type == BlockType::Burn)
        .map(|b| (b.hash.into_tx_hash(), b.timestamp))
        .collect();
    assert!(!expected_mints.is_empty());
    assert_eq!(expected.minted_burns, expected_mints);
    while let Some(next) =
        PreparedInstallation::next(&source, NetworkId::Dev, &fixture.params).unwrap()
    {
        next.commit(&source).unwrap();
    }
    let (_dir, local) = local_genesis(&fixture);
    crate::payment_authority::initialize(&local, NetworkId::Dev, &fixture.params).unwrap();
    local
        .meta_store()
        .put_meta("application-preference", b"preserve")
        .unwrap();
    local
        .meta_store()
        .put_meta("verification_stake/v2/stale", b"remove")
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let needs = snapshot.installation_needs(&local).unwrap();
    let hashes: Vec<_> = needs
        .snapshot_heads
        .iter()
        .chain(&needs.cemented_heads)
        .map(|h| h.hash)
        .collect();
    let proofs = witnesses(&snapshot, &fixture, &source, &hashes);
    let prepared = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap();
    assert!(prepared.retained_blocks() < prepared.ledger_blocks());
    let txn = prepared.transaction();
    let head_key = format!("checkpoint_chain/v1/dev/{}/head", fixture.blocks[0].hash);
    assert_eq!(
        txn.get_meta(&head_key).unwrap(),
        Some(certificate.checkpoint.height.to_be_bytes().to_vec())
    );
    assert!(local
        .meta_store()
        .get_meta_txn(&local.tx_begin_read().unwrap(), &head_key)
        .unwrap()
        .is_none());
    assert_eq!(
        txn.checkpoint_metadata(
            format!("checkpoint_chain/v1/dev/{}/", fixture.blocks[0].hash).as_bytes()
        )
        .unwrap()
        .len(),
        1
    );
    let full = source.tx_begin_write().unwrap();
    for (key, value) in full.checkpoint_metadata(b"").unwrap() {
        let Ok(key) = std::str::from_utf8(&key) else {
            continue;
        };
        if key.starts_with("confirmed_authority/")
            || key.starts_with("confirmed_governance/")
            || burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key)
            || [
                INSTALLED_KEY,
                CUTOFF_KEY,
                crate::endorsement_commit::SNAPSHOT_KEY,
                burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
                burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                crate::confirmed_governance::KEY,
                "confirmed_authority/v1",
                "verified_count",
                "merger_graph",
                burst_trst::TrstEngine::meta_key(),
                burst_governance::DelegationEngine::meta_key(),
            ]
            .contains(&key)
        {
            assert_eq!(txn.get_meta(key).unwrap(), Some(value), "{key}");
        }
    }
    for key in [
        b"rate_history".as_slice(),
        b"next_stake_id",
        b"protocol_params",
        burst_verification::bootstrap::BootstrapPhase::KEY,
    ] {
        assert_eq!(
            txn.undo_value(UndoTable::BrnMeta, key).unwrap(),
            full.undo_value(UndoTable::BrnMeta, key).unwrap()
        );
    }
    let decode_governance = |batch: &burst_store_lmdb::LmdbWriteTransaction<'_>| {
        let bytes = batch
            .undo_value(UndoTable::BrnMeta, b"governance_engine")
            .unwrap()
            .unwrap();
        serde_json::to_value(
            bincode::deserialize::<burst_governance::GovernanceEngine>(&bytes).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(decode_governance(txn), decode_governance(&full));
    let expected_electorate: Vec<_> = full.account_entries().unwrap()
        .map(Result::unwrap)
        .filter(|account| account.state.is_verified())
        .map(|account| account.address)
        .collect();
    assert!(!expected_electorate.is_empty());
    assert_eq!(full.verified_accounts().unwrap(), expected_electorate);
    assert_eq!(txn.verified_accounts().unwrap(), expected_electorate);

    let decode_consti = |batch: &burst_store_lmdb::LmdbWriteTransaction<'_>| {
        serde_json::to_value(
            bincode::deserialize::<burst_consti::ConstiEngine>(
                &batch.get_meta("consti_engine").unwrap().unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(decode_consti(txn), decode_consti(&full));
    let restored = crate::payment_transition::restore(txn, &expected.params).unwrap();
    assert_eq!(restored.brn.wallets, expected.brn.wallets);
    assert_eq!(restored.brn.rate_history, expected.brn.rate_history);
    assert_eq!(restored.trst.save_wallets(), expected.trst.save_wallets());
    assert_eq!(
        restored.trst.merger_graph.to_bytes(),
        expected.trst.merger_graph.to_bytes()
    );
    for ((wallet, hash), pending) in &expected.pending {
        assert_eq!(
            bincode::serialize(&txn.get_pending(wallet, hash).unwrap()).unwrap(),
            bincode::serialize(pending).unwrap()
        );
    }
    for (hash, at) in expected_mints {
        let mut origin = hash.as_bytes().to_vec();
        origin.extend_from_slice(hash.as_bytes());
        let mut expiry = at
            .as_secs()
            .saturating_add(expected.params.trst_expiry_secs)
            .to_be_bytes()
            .to_vec();
        expiry.extend_from_slice(hash.as_bytes());
        for (table, key) in [
            (UndoTable::TrstOrigins, origin),
            (UndoTable::TrstExpiry, expiry),
            (UndoTable::TrstReverse, hash.as_bytes().to_vec()),
        ] {
            assert_eq!(
                txn.undo_value(table, &key).unwrap(),
                full.undo_value(table, &key).unwrap()
            );
            assert!(txn.undo_value(table, &key).unwrap().is_some());
        }
    }
    let bytes = installed::read_base_bytes(|key| txn.get_meta(key))
        .unwrap()
        .unwrap();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let recovered = AuthenticatedSnapshot::read(&genesis, &mut bytes.as_slice()).unwrap();
    compact::assert_same(&recovered.into_state(), &expected);
    assert_eq!(
        txn.get_meta("application-preference").unwrap(),
        Some(b"preserve".to_vec())
    );
    assert!(txn
        .get_meta("verification_stake/v2/stale")
        .unwrap()
        .is_none());
    assert_eq!(txn.payment_epoch().unwrap(), certificate.checkpoint.hash());
    assert!(txn
        .get_meta(&burst_store::meta::closed_payment_epoch_key(
            &fixture.blocks[0].hash
        ))
        .unwrap()
        .is_some());
    drop(prepared);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    assert_eq!(local.block_store().block_count().unwrap(), 1);
}

#[test]
fn snapshot_head_staging_preserves_sparse_certificates_and_rejects_gaps() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let mut anchors = Vec::new();
    for end in 0..=events.len() {
        anchors.push(chain::next_anchor(&events[..end], NetworkId::Dev, &fixture.params).unwrap());
    }
    let bytes =
        super::super::installed_recovery::archive(&source, &fixture, certificate.checkpoint.parent);
    let prefix = format!("checkpoint_chain/v1/dev/{}/", fixture.blocks[0].hash);
    for case in 0..7 {
        let (_dir, local) = local_genesis(&fixture);
        let mut batch = local.tx_begin_write().unwrap();
        batch.put_meta(installed::BASE_KEY, &bytes).unwrap();
        if case != 5 {
            batch
                .put_meta(&format!("{prefix}head"), &1u64.to_be_bytes())
                .unwrap();
        }
        match case {
            1 => batch
                .put_meta(
                    &format!("{prefix}1"),
                    &source.meta_store().get_meta(&format!("{prefix}1")).unwrap(),
                )
                .unwrap(),
            2 => batch
                .put_meta(&format!("{prefix}head"), &2u64.to_be_bytes())
                .unwrap(),
            3 => batch
                .put_meta(installed::BASE_KEY, b"damaged archive")
                .unwrap(),
            4 => batch
                .put_meta(
                    &format!("{prefix}1"),
                    &source.meta_store().get_meta(&format!("{prefix}2")).unwrap(),
                )
                .unwrap(),
            6 => batch
                .put_meta(
                    &format!("{prefix}01"),
                    &source.meta_store().get_meta(&format!("{prefix}1")).unwrap(),
                )
                .unwrap(),
            _ => (),
        }
        let before = batch.checkpoint_metadata(b"").unwrap();
        let result = crate::checkpoint_store::stage_snapshot_head(
            &mut batch,
            anchors.iter().cloned().map(Ok),
        );
        assert_eq!(result.is_ok(), case < 2, "case {case}");
        if result.is_ok() {
            assert_eq!(
                batch.get_meta(&format!("{prefix}head")).unwrap(),
                Some(certificate.checkpoint.height.to_be_bytes().to_vec())
            );
            for (key, value) in &before {
                let key = std::str::from_utf8(key).unwrap();
                if key != format!("{prefix}head") {
                    assert_eq!(batch.get_meta(key).unwrap().as_ref(), Some(value));
                }
            }
        } else {
            assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        }
        drop(batch);
        assert!(local
            .meta_store()
            .get_meta_txn(&local.tx_begin_read().unwrap(), &format!("{prefix}head"))
            .unwrap()
            .is_none());
    }
}

#[test]
fn stored_snapshot_base_reauthenticates_after_reopen_and_rejects_corruption() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let (dir, local) = local_genesis(&fixture);
    assert!(
        installed::load_base(&local, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
    let needs = snapshot.installation_needs(&local).unwrap();
    let hashes: Vec<_> = needs
        .snapshot_heads
        .iter()
        .chain(&needs.cemented_heads)
        .map(|h| h.hash)
        .collect();
    let proofs = witnesses(&snapshot, &fixture, &source, &hashes);
    let prepared = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap();
    let bytes = installed::read_base_bytes(|key| prepared.transaction().get_meta(key))
        .unwrap()
        .unwrap();
    drop(prepared);
    // Persist only the archive to exercise the base reader; this is deliberately
    // not an installed ledger or a node-startup test.
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    local.env().force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let recovered = installed::load_base(&local, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.anchor().hash(), certificate.checkpoint.hash());
    assert_eq!(recovered.recover_selections().unwrap().len(), 1);
    assert!(installed::load_base(&local, NetworkId::Test, &fixture.params).is_err());
    let mut wrong = fixture.params.clone();
    wrong.brn_rate += 1;
    assert!(installed::load_base(&local, NetworkId::Dev, &wrong).is_err());
    for case in 0..3 {
        let mut corrupt = bytes.clone();
        match case {
            0 => {
                corrupt.pop();
            }
            1 => corrupt.push(0),
            _ => {
                let last = corrupt.len() - 1;
                corrupt[last] ^= 1;
            }
        }
        local
            .meta_store()
            .put_meta(installed::BASE_KEY, &corrupt)
            .unwrap();
        assert!(installed::load_base(&local, NetworkId::Dev, &fixture.params).is_err());
    }
}

#[test]
fn snapshot_rejects_signer_metadata_and_invalid_mint_inventory_before_export() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for case in 0..4 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let anchor = snapshot.anchor().clone();
        let mut state = snapshot.into_state();
        match case {
            0 => {
                state
                    .lifecycle_metadata
                    .insert("consensus_final/v2/forged".into(), vec![1]);
            }
            1 => {
                state
                    .lifecycle_metadata
                    .insert(installed::BASE_KEY.into(), vec![1]);
            }
            2 => {
                state.minted_burns.insert(TxHash::ZERO, Timestamp::EPOCH);
            }
            _ => {
                state
                    .minted_burns
                    .insert(TxHash::new([55; 32]), Timestamp::new(anchor.cutoff() + 1));
            }
        }
        assert!(state.prepare_snapshot(&anchor).is_err(), "case {case}");
    }
}

#[test]
fn compact_economic_handoff_preserves_promises_and_rejects_unsupported_local_tails() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for case in 0..3 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local, _) = fixture.store();
        crate::payment_authority::initialize(&local, NetworkId::Dev, &fixture.params).unwrap();
        let mut tail = Fixture {
            params: fixture.params.clone(),
            blocks: fixture.blocks.clone(),
        };
        let kind = if case == 1 {
            BlockType::GovernanceActivation
        } else {
            BlockType::ChangeRepresentative
        };
        let hash = tail.push(0, kind, certificate.checkpoint.cutoff + 1, |_| {});
        let block = tail.blocks.last().unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        let mut account = batch.get_account(&block.account).unwrap();
        account.block_count += 1;
        account.head = hash;
        let bytes = if case == 2 {
            b"damaged local body".to_vec()
        } else {
            bincode::serialize(block).unwrap()
        };
        batch
            .put_block_with_account(&hash, &bytes, &account.address, account.block_count)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&account.address, &hash).unwrap();
        batch.commit().unwrap();
        let epoch = fixture.blocks[0].hash;
        local
            .meta_store()
            .reserve_consensus_vote_at(
                &epoch,
                &address(0),
                &block.account,
                &block.previous,
                &hash,
                true,
                0,
                Some(block.timestamp.as_secs()),
            )
            .unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .seal_consensus_after_certificate(&address(0), &epoch)
            .unwrap();
        batch.commit().unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let needs = snapshot.installation_needs(&local).unwrap();
        let hashes: Vec<_> = needs
            .snapshot_heads
            .iter()
            .chain(&needs.cemented_heads)
            .map(|h| h.hash)
            .collect();
        let proofs = witnesses(&snapshot, &fixture, &source, &hashes);
        let result = snapshot
            .reconcile_installation(
                local.tx_begin_write().unwrap(),
                NetworkId::Dev,
                &fixture.params,
                &proofs,
            )
            .unwrap()
            .project_ledger(&[])
            .unwrap()
            .project_economics();
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        if let Ok(prepared) = result {
            assert_eq!(prepared.retained_promises(), &[hash]);
            assert_eq!(
                prepared
                    .transaction()
                    .checkpoint_metadata(b"consensus_")
                    .unwrap(),
                before
                    .iter()
                    .filter(|(key, _)| key.starts_with(b"consensus_"))
                    .cloned()
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                prepared.transaction().payment_epoch().unwrap(),
                certificate.checkpoint.hash()
            );
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        assert_eq!(local.block_store().get_block(&hash).unwrap(), bytes);
        assert_eq!(local.meta_store().payment_epoch().unwrap(), epoch);
    }
}
