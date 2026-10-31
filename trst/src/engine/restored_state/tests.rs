use super::*;

pub(super) fn fixture() -> TrstEngine {
    let mut engine = TrstEngine::with_expiry(100);
    for id in 1..=3 {
        let owner = WalletAddress::new(format!("brst_owner_{id}"));
        let token = engine
            .mint(
                TxHash::new([id; 32]),
                owner.clone(),
                50,
                owner,
                Timestamp::new(100),
            )
            .unwrap();
        engine.track_token(token);
    }
    engine
}

#[test]
fn scoped_transfer_matches_full_restore_and_updates_holder_indexes() {
    let baseline = fixture();
    let sender = WalletAddress::new("brst_owner_1");
    let recipient = WalletAddress::new("brst_recipient");
    let unrelated = WalletAddress::new("brst_owner_3");
    let mut state = RestoredTrstState::from_engine(baseline.clone()).unwrap();
    let mut portfolio = (**baseline.wallets.get(&sender).unwrap()).clone();
    for token in &mut portfolio.tokens {
        token.holder = recipient.clone();
    }
    state.replace_portfolio(&sender, None).unwrap();
    state
        .replace_portfolio(&recipient, Some(portfolio))
        .unwrap();
    assert!(std::ptr::eq(
        &**state.engine().wallets.get(&unrelated).unwrap(),
        &**baseline.wallets.get(&unrelated).unwrap()
    ));
    assert_eq!(baseline.wallets.get(&sender).unwrap().tokens.len(), 1);
    let mut restored = TrstEngine::load_wallets(&state.engine().save_wallets(), 100).unwrap();
    restored.merger_graph = baseline.merger_graph.clone();
    restored.validate_bases().unwrap();
    assert_eq!(state.engine().origin_holders, restored.origin_holders);
    assert_eq!(state.engine().revoked_holders, restored.revoked_holders);
    assert_eq!(state.engine().wallet_origins, baseline.wallet_origins);
    let mut actual = state.into_engine();
    actual.revoke_by_originator(&sender);
    restored.revoke_by_originator(&sender);
    assert_eq!(actual.save_wallets(), restored.save_wallets());
}

#[test]
fn invalid_portfolio_patch_is_atomic_and_cannot_create_provenance() {
    let baseline = fixture();
    let owner = WalletAddress::new("brst_owner_1");
    let mut state = RestoredTrstState::from_engine(baseline.clone()).unwrap();
    for mutation in 0..7 {
        let mut portfolio = (**baseline.wallets.get(&owner).unwrap()).clone();
        match mutation {
            0 => portfolio.cached_transferable += 1,
            1 => portfolio.earliest_expiry = Some(Timestamp::new(999)),
            2 => portfolio.tokens[0].holder = WalletAddress::new("brst_wrong"),
            3 => portfolio.tokens[0].origin_wallet = WalletAddress::new("brst_wrong"),
            4 => portfolio.tokens[0].origin = TxHash::new([99; 32]),
            5 => {
                let duplicate = portfolio.tokens[0].clone();
                portfolio.tokens.push(duplicate);
            }
            _ => {
                portfolio.tokens[0].state = TrstState::Revoked;
                portfolio.tokens[0].revoked_origin = Some(TxHash::new([99; 32]));
                portfolio.cached_transferable = 0;
                portfolio.earliest_expiry = None;
            }
        }
        assert!(
            state.replace_portfolio(&owner, Some(portfolio)).is_err(),
            "mutation {mutation}"
        );
        assert!(state
            .engine()
            .wallets
            .shares_storage_with(&baseline.wallets));
    }
    let portfolio = (**baseline.wallets.get(&owner).unwrap()).clone();
    state.replace_portfolio(&owner, Some(portfolio)).unwrap();
    assert!(state
        .engine()
        .wallets
        .shares_storage_with(&baseline.wallets));
    let mut corrupt = baseline.clone();
    corrupt.wallets.get_mut(&owner).unwrap().cached_transferable += 1;
    assert!(RestoredTrstState::from_engine(corrupt).is_err());
}

#[test]
fn revoked_portfolio_move_keeps_restoration_and_expiry_indexes_correct() {
    let mut engine = fixture();
    let sender = WalletAddress::new("brst_owner_1");
    let recipient = WalletAddress::new("brst_recipient");
    engine.revoke_by_originator(&sender);
    let mut state = RestoredTrstState::from_engine(engine.clone()).unwrap();
    let mut portfolio = (**engine.wallets.get(&sender).unwrap()).clone();
    for token in &mut portfolio.tokens {
        token.holder = recipient.clone();
    }
    state.replace_portfolio(&sender, None).unwrap();
    state
        .replace_portfolio(&recipient, Some(portfolio))
        .unwrap();
    let mut expected = TrstEngine::load_wallets(&state.engine().save_wallets(), 100).unwrap();
    expected.merger_graph = engine.merger_graph.clone();
    expected.validate_bases().unwrap();
    let mut actual = state.into_engine();
    assert_eq!(actual.revoked_holders, expected.revoked_holders);
    actual
        .restore_by_originator(&sender, Timestamp::new(150))
        .unwrap();
    expected
        .restore_by_originator(&sender, Timestamp::new(150))
        .unwrap();
    actual.flush_all_expired(Timestamp::new(250));
    expected.flush_all_expired(Timestamp::new(250));
    assert_eq!(actual.save_wallets(), expected.save_wallets());
}

#[test]
fn burn_registration_and_reverse_removal_match_full_restoration() {
    let baseline = fixture();
    let owner = WalletAddress::new("brst_new_owner");
    let origin = TxHash::new([9; 32]);
    let mut expected = baseline.clone();
    let token = expected
        .mint(
            origin,
            owner.clone(),
            50,
            owner.clone(),
            Timestamp::new(100),
        )
        .unwrap();
    expected.track_token(token);
    let mut state = RestoredTrstState::from_engine(baseline.clone()).unwrap();
    let mut changes = RestoredTrstChanges::default();
    changes.owner_headers.insert(owner.clone(), true);
    changes.origins.insert(origin, Some(owner.clone()));
    changes.portfolios.insert(
        owner.clone(),
        Some((**expected.wallets.get(&owner).unwrap()).clone()),
    );
    state.replace_records(changes).unwrap();
    assert!(state.engine().wallet_snapshot_eq(&expected));
    state.engine().validate_bases().unwrap();
    let unrelated = WalletAddress::new("brst_owner_3");
    assert!(std::ptr::eq(
        &**baseline.wallets.get(&unrelated).unwrap(),
        &**state.engine().wallets.get(&unrelated).unwrap()
    ));
    // Spending/removing holdings must not silently erase historical ownership.
    state.replace_portfolio(&owner, None).unwrap();
    assert!(state
        .engine()
        .wallet_origins
        .get(&owner)
        .unwrap()
        .contains(&origin));
    let mut reverse = RestoredTrstChanges::default();
    reverse.origins.insert(origin, None);
    reverse.owner_headers.insert(owner, false);
    state.replace_records(reverse).unwrap();
    assert!(state.engine().wallet_snapshot_eq(&baseline));
}

#[test]
fn ownership_changes_validate_existing_holders_and_are_atomic() {
    let baseline = fixture();
    let owner = WalletAddress::new("brst_owner_1");
    let other = WalletAddress::new("brst_owner_2");
    let origin = TxHash::new([1; 32]);
    let mut state = RestoredTrstState::from_engine(baseline.clone()).unwrap();
    for mutation in 0..4 {
        let mut changes = RestoredTrstChanges::default();
        match mutation {
            0 => {
                changes.origins.insert(origin, None);
            }
            1 => {
                changes.origins.insert(origin, Some(other.clone()));
            }
            2 => {
                changes.owner_headers.insert(owner.clone(), false);
            }
            _ => {
                changes.origins.insert(
                    TxHash::new([9; 32]),
                    Some(WalletAddress::new("brst_no_header")),
                );
            }
        }
        assert!(
            state.replace_records(changes).is_err(),
            "mutation {mutation}"
        );
        assert!(state
            .engine()
            .wallets
            .shares_storage_with(&baseline.wallets));
        assert!(state
            .engine()
            .wallet_origins
            .shares_storage_with(&baseline.wallet_origins));
    }
}

#[test]
fn origin_removal_preserves_graph_and_revocation_dependencies_without_holders() {
    let owner = WalletAddress::new("brst_owner_1");
    let origin = TxHash::new([1; 32]);
    for revoked in [false, true] {
        let mut baseline = fixture();
        if revoked {
            baseline.revoke_by_originator(&owner);
        } else {
            baseline.merger_graph.record_merge(MergeNode {
                merge_tx: TxHash::new([9; 32]),
                source_origins: vec![
                    MergeSource { origin, amount: 25 },
                    MergeSource {
                        origin: TxHash::new([2; 32]),
                        amount: 25,
                    },
                ],
                total_amount: 50,
                revoked_contribs: HashMap::new(),
            });
        }
        let mut state = RestoredTrstState::from_engine(baseline.clone()).unwrap();
        let mut changes = RestoredTrstChanges::default();
        changes.portfolios.insert(owner.clone(), None);
        changes.origins.insert(origin, None);
        changes.owner_headers.insert(owner.clone(), false);
        assert!(state.replace_records(changes).is_err());
        assert!(state.engine().wallet_snapshot_eq(&baseline));
        if !revoked {
            let mut collision = RestoredTrstChanges::default();
            collision
                .origins
                .insert(TxHash::new([9; 32]), Some(owner.clone()));
            assert!(state.replace_records(collision).is_err());
        }
    }
}

#[test]
fn absent_registration_for_merge_with_descendants_is_a_noop() {
    let mut baseline = fixture();
    for (id, sources) in [(9, [1, 2]), (10, [9, 3])] {
        baseline.merger_graph.record_merge(MergeNode {
            merge_tx: TxHash::new([id; 32]),
            source_origins: sources
                .into_iter()
                .map(|id| MergeSource {
                    origin: TxHash::new([id; 32]),
                    amount: 25,
                })
                .collect(),
            total_amount: 50,
            revoked_contribs: HashMap::new(),
        });
    }
    let mut state = RestoredTrstState::from_engine(baseline).unwrap();
    let before = state.clone();
    let mut changes = RestoredTrstChanges::default();
    // A transient, cancelled origin-row write can leave this key dirty even
    // though a merge was never registered as a burn in the validated baseline.
    changes.origins.insert(TxHash::new([9; 32]), None);
    state.replace_records(changes).unwrap();
    assert!(state
        .engine
        .wallets
        .shares_storage_with(&before.engine.wallets));
    assert!(state
        .engine
        .wallet_origins
        .shares_storage_with(&before.engine.wallet_origins));
    assert!(state.owners.shares_storage_with(&before.owners));
}

#[test]
fn retained_successor_checks_cut_and_rebuilds_changed_memberships() {
    let mut state = RestoredTrstState::from_engine(fixture()).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let before = state.engine().clone();
    let holder = WalletAddress::new("brst_owner_1");
    let mut after = before.clone();
    after.debit_wallet(&holder, &TxHash::new([1; 32]), 50);
    // Caller-visible derived indexes are not an authority for publication.
    after.origin_holders.clear();
    after
        .revoked_holders
        .entry(TxHash::new([99; 32]))
        .or_default()
        .insert(holder.clone());
    let next = state
        .retained_holdings_successor(&before, &after)
        .unwrap()
        .unwrap();
    let full = RestoredTrstState::from_engine(after.clone()).unwrap();
    assert_eq!(next.engine().origin_holders, full.engine().origin_holders);
    assert_eq!(next.engine().revoked_holders, full.engine().revoked_holders);
    assert!(next.engine().wallets[&holder].shares_storage_with(&after.wallets[&holder]));
    assert_eq!(next.engine().save_wallets(), full.engine().save_wallets());
    let mut wrong_before = before.clone();
    wrong_before
        .wallets
        .remove(&WalletAddress::new("brst_owner_3"));
    assert!(state
        .retained_holdings_successor(&wrong_before, &after)
        .unwrap()
        .is_none());
    let mut changed_period = after.clone();
    changed_period.expiry_secs += 1;
    assert!(state
        .retained_holdings_successor(&before, &changed_period)
        .unwrap()
        .is_none());
    let mut changed_registry = after.clone();
    changed_registry.wallet_origins.clear();
    assert!(state
        .retained_holdings_successor(&before, &changed_registry)
        .unwrap()
        .is_none());
    let mut changed_graph = after.clone();
    changed_graph.merger_graph = MergerGraph::new();
    // An equal empty graph may share canonical empty roots; use actual mutation.
    changed_graph.revoke_by_originator(&WalletAddress::new("brst_owner_2"));
    assert!(state
        .retained_holdings_successor(&before, &changed_graph)
        .unwrap()
        .is_none());
    assert!(state.engine().shares_wallet_snapshot_with(&before));
}

#[test]
fn retained_successor_rejects_invalid_composition_and_owner_atomically() {
    let mut state = RestoredTrstState::from_engine(fixture()).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let before = state.engine().clone();
    let holder = WalletAddress::new("brst_owner_1");
    for invalid in 0..3 {
        let root = before.wallets[&holder].indexed().unwrap();
        let mut token = root.get(&TxHash::new([1; 32])).unwrap().clone();
        match invalid {
            0 => token.origin_wallet = WalletAddress::new("brst_forged"),
            1 => token.basis = TrstBasis::Single(TxHash::new([99; 32])),
            _ => {
                token.state = TrstState::Revoked;
                token.revoked_origin = Some(TxHash::new([99; 32]));
            }
        }
        let mut after = before.clone();
        after.wallets.insert(
            holder.clone(),
            WalletPortfolio::from_indexed(root.with_replaced(token).unwrap()).into(),
        );
        assert!(state.retained_holdings_successor(&before, &after).is_err());
        assert!(state.engine().shares_wallet_snapshot_with(&before));
    }
}

#[test]
fn retained_successor_creates_and_removes_portfolios_without_copying_derived_indexes() {
    let mut state = RestoredTrstState::from_engine(fixture()).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let before = state.engine().clone();
    let sender = WalletAddress::new("brst_owner_1");
    let recipient = WalletAddress::new("brst_new_holder");
    let mut token = before.wallets[&sender]
        .indexed()
        .unwrap()
        .get(&TxHash::new([1; 32]))
        .unwrap()
        .clone();
    token.holder = recipient.clone();
    let mut after = before.clone();
    after.wallets.remove(&sender);
    after.track_token(token);
    let next = state
        .retained_holdings_successor(&before, &after)
        .unwrap()
        .unwrap();
    assert!(next.engine().wallets[&recipient].indexed().is_some());
    assert!(!next.engine().wallets.contains_key(&sender));
    let full = RestoredTrstState::from_engine(after).unwrap();
    assert_eq!(next.engine().save_wallets(), full.engine().save_wallets());
    assert_eq!(next.engine().origin_holders, full.engine().origin_holders);
    assert_eq!(next.engine().revoked_holders, full.engine().revoked_holders);
    let before = next.engine().clone();
    let mut after = before.clone();
    after.debit_wallet(&recipient, &TxHash::new([1; 32]), 1);
    assert!(next
        .retained_holdings_successor(&before, &after)
        .unwrap()
        .is_some());
}

#[test]
fn appended_merge_successor_matches_full_restore_and_checks_ownership() {
    let holder = WalletAddress::new("brst_owner_1");
    let mut initial = fixture();
    let extra = initial
        .mint(
            TxHash::new([4; 32]),
            holder.clone(),
            30,
            holder.clone(),
            Timestamp::new(100),
        )
        .unwrap();
    initial.track_token(extra);
    let mut state = RestoredTrstState::from_engine(initial).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let before = state.engine().clone();
    let ids = [TxHash::new([1; 32]), TxHash::new([4; 32])];
    let mut after = before.clone();
    let inputs = after.wallets[&holder].select_merge_inputs(&ids).unwrap();
    let merged = after
        .merge(
            &inputs,
            holder.clone(),
            TxHash::new([9; 32]),
            Timestamp::new(100),
        )
        .unwrap();
    after.bulk_untrack(&holder, &ids.into_iter().collect());
    after.track_token(merged);
    after.origin_holders.clear();
    after
        .revoked_holders
        .entry(TxHash::new([99; 32]))
        .or_default()
        .insert(holder.clone());
    let next = state
        .retained_append_successor(&before, &after)
        .unwrap()
        .unwrap();
    let full = RestoredTrstState::from_engine(after.clone()).unwrap();
    assert_eq!(next.engine().save_wallets(), full.engine().save_wallets());
    assert_eq!(next.engine().origin_holders, full.engine().origin_holders);
    assert_eq!(next.engine().revoked_holders, full.engine().revoked_holders);
    assert!(next.engine().wallets[&holder].shares_storage_with(&after.wallets[&holder]));
    assert!(next
        .engine()
        .merger_graph
        .shares_storage_with(&after.merger_graph));
    let mut removed = Vec::new();
    after
        .merger_graph
        .visit_record_delta(&before.merger_graph, |old, new| {
            assert!(old.is_none());
            removed.push(new.unwrap());
            Ok(())
        })
        .unwrap();
    assert!(next
        .retained_graph_removal(&removed, RestoredTrstChanges::default())
        .is_err());
    let mut changes = RestoredTrstChanges::default();
    changes
        .portfolios
        .insert(holder.clone(), Some((*before.wallets[&holder]).clone()));
    let rewound = next
        .retained_graph_removal(&removed, changes)
        .unwrap()
        .unwrap();
    assert_eq!(rewound.engine().save_wallets(), before.save_wallets());
    assert!(rewound.engine().merger_graph == before.merger_graph);
    assert_eq!(rewound.revocation_counts, state.revocation_counts);
    assert_eq!(rewound.revoked_token_origins, state.revoked_token_origins);
    assert_eq!(rewound.revision_index, state.revision_index);
    assert!(rewound.engine().wallets[&holder].indexed().is_some());
    let before_again = rewound.engine().clone();
    let mut after_again = before_again.clone();
    let inputs = after_again.wallets[&holder]
        .select_merge_inputs(&ids)
        .unwrap();
    let merged = after_again
        .merge(
            &inputs,
            holder.clone(),
            TxHash::new([10; 32]),
            Timestamp::new(100),
        )
        .unwrap();
    after_again.bulk_untrack(&holder, &ids.into_iter().collect());
    after_again.track_token(merged);
    assert!(rewound
        .retained_append_successor(&before_again, &after_again)
        .unwrap()
        .is_some());
    let mut wrong_before = before.clone();
    wrong_before.wallets.remove(&holder);
    assert!(state
        .retained_append_successor(&wrong_before, &after)
        .unwrap()
        .is_none());
    for collision in [false, true] {
        let mut invalid = before.clone();
        invalid.merger_graph.record_merge(MergeNode {
            merge_tx: TxHash::new([if collision { 3 } else { 9 }; 32]),
            source_origins: vec![
                MergeSource {
                    origin: TxHash::new([1; 32]),
                    amount: 25,
                },
                MergeSource {
                    origin: TxHash::new([if collision { 2 } else { 99 }; 32]),
                    amount: 25,
                },
            ],
            total_amount: 50,
            revoked_contribs: HashMap::new(),
        });
        assert!(state.retained_append_successor(&before, &invalid).is_err());
        assert!(RestoredTrstState::from_engine(invalid).is_err());
    }
    assert!(state.engine().shares_wallet_snapshot_with(&before));
}

#[test]
fn retained_revocation_successor_checks_removed_tags_and_ownership() {
    let mut state = RestoredTrstState::from_engine(fixture()).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let holder = WalletAddress::new("brst_owner_1");
    let before = state.engine().clone();
    let mut after = before.clone();
    after.revoke_by_originator(&holder);
    after.revoked_holders.clear();
    let revoked = state
        .retained_revocation_successor(&before, &after)
        .unwrap()
        .unwrap();
    let full = RestoredTrstState::from_engine(after).unwrap();
    assert_eq!(revoked.revoked_token_origins, full.revoked_token_origins);
    assert_eq!(revoked.revision_index, full.revision_index);
    assert_eq!(
        revoked.engine().save_wallets(),
        full.engine().save_wallets()
    );
    assert_eq!(
        revoked.engine().revoked_holders,
        full.engine().revoked_holders
    );
    let before = revoked.engine().clone();
    let mut bad = before.clone();
    bad.merger_graph.apply_restoration(&TxHash::new([1; 32]));
    bad.revoked_holders.clear();
    assert!(revoked
        .retained_revocation_successor(&before, &bad)
        .is_err());
    assert!(RestoredTrstState::from_engine(bad).is_err());
    let mut after = before.clone();
    after
        .restore_by_originator(&holder, Timestamp::new(100))
        .unwrap();
    let restored = revoked
        .retained_revocation_successor(&before, &after)
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.engine().save_wallets(),
        state.engine().save_wallets()
    );
    let mut bad = before.clone();
    bad.merger_graph.apply_revocation(TxHash::new([99; 32]));
    assert!(revoked
        .retained_revocation_successor(&before, &bad)
        .is_err());
    assert!(RestoredTrstState::from_engine(bad).is_err());
    assert!(revoked.engine().shares_wallet_snapshot_with(&before));
}

#[test]
fn retained_outcome_causes_match_full_restore_through_overlapping_causes() {
    let mut state = RestoredTrstState::from_engine(fixture()).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let owner = WalletAddress::new("brst_owner_1");
    let origins = [(owner, TxHash::new([1; 32]))];
    for (add, cause) in [(true, 1), (true, 2), (false, 1), (false, 2)] {
        let before = state.engine().clone();
        let mut after = before.clone();
        if add {
            after
                .invalidate_outcome_origins(BlockHash::new([cause; 32]), &origins)
                .unwrap();
        } else {
            after
                .restore_outcome_origins(BlockHash::new([cause; 32]), &origins, Timestamp::new(100))
                .unwrap();
        }
        let next = state
            .retained_revocation_successor(&before, &after)
            .unwrap()
            .unwrap();
        let full = RestoredTrstState::from_engine(after.clone()).unwrap();
        assert_eq!(next.engine().save_wallets(), full.engine().save_wallets());
        assert_eq!(
            next.engine().merger_graph.to_bytes(),
            full.engine().merger_graph.to_bytes()
        );
        if before.merger_graph.shares_storage_with(&after.merger_graph) {
            assert!(state
                .retained_holdings_successor(&before, &after)
                .unwrap()
                .is_some());
        }
        state = next;
    }
    let before = state.engine().clone();
    for invalid in 0..3 {
        let mut after = before.clone();
        after.outcome_invalidations.insert(
            TxHash::new([1; 32]),
            OutcomeInvalidation {
                causes: match invalid {
                    0 => HashSet::new(),
                    1 => [BlockHash::ZERO].into_iter().collect(),
                    _ => [BlockHash::new([1; 32])].into_iter().collect(),
                },
                wallet_revoked: false,
            },
        );
        assert!(state.retained_holdings_successor(&before, &after).is_err());
        assert!(RestoredTrstState::from_engine(after).is_err());
    }
}

#[test]
fn contribution_counts_follow_append_then_revocation_restore_without_touching_prior_cut() {
    let mut engine = fixture();
    let owner = WalletAddress::new("brst_owner_1");
    engine.revoke_by_originator(&owner);
    let mut state = RestoredTrstState::from_engine(engine).unwrap();
    state.retain_portfolio_indexes().unwrap();
    let before = state.engine().clone();
    let mut after = before.clone();
    after.merger_graph.record_merge(MergeNode {
        merge_tx: TxHash::new([90; 32]),
        source_origins: vec![
            MergeSource {
                origin: TxHash::new([1; 32]),
                amount: 10,
            },
            MergeSource {
                origin: TxHash::new([2; 32]),
                amount: 10,
            },
        ],
        total_amount: 20,
        revoked_contribs: [(TxHash::new([1; 32]), 10)].into_iter().collect(),
    });
    let appended = state
        .retained_append_successor(&before, &after)
        .unwrap()
        .unwrap();
    assert_eq!(
        appended.revocation_counts.get(&TxHash::new([1; 32])),
        Some(&1)
    );
    let before = appended.engine().clone();
    let mut after = before.clone();
    after
        .restore_by_originator(&owner, Timestamp::new(100))
        .unwrap();
    let next = appended
        .retained_revocation_successor(&before, &after)
        .unwrap()
        .unwrap();
    let full = RestoredTrstState::from_engine(after).unwrap();
    assert_eq!(next.revocation_counts, full.revocation_counts);
    assert_eq!(next.revoked_token_origins, full.revoked_token_origins);
    assert_eq!(next.revision_index, full.revision_index);
    assert!(next.revocation_counts.is_empty());
    assert_eq!(next.engine().save_wallets(), full.engine().save_wallets());
    assert_eq!(
        appended.revocation_counts.get(&TxHash::new([1; 32])),
        Some(&1)
    );
    assert!(state.revocation_counts.is_empty());
}

#[test]
fn graph_removal_checks_revoked_tokens_by_composition_not_only_revocation_tag() {
    let mut engine = fixture();
    let holder = WalletAddress::new("brst_owner_3");
    engine.revoke_by_originator(&holder);
    let graph_before = engine.merger_graph.clone();
    let merge = TxHash::new([90; 32]);
    engine.merger_graph.record_merge(MergeNode {
        merge_tx: merge,
        source_origins: vec![MergeSource {
            origin: TxHash::new([1; 32]),
            amount: 50,
        }],
        total_amount: 50,
        revoked_contribs: HashMap::new(),
    });
    engine.wallets.get_mut(&holder).unwrap().tokens[0].origin = merge;
    let mut rows = Vec::new();
    engine
        .merger_graph
        .visit_record_delta(&graph_before, |old, new| {
            assert!(old.is_none());
            rows.push(new.unwrap());
            Ok(())
        })
        .unwrap();
    let state = RestoredTrstState::from_engine(engine).unwrap();
    assert!(!state.engine().origin_holders.contains_key(&merge));
    assert_eq!(state.revoked_token_origins.get(&merge), Some(&1));
    assert!(state
        .retained_graph_removal(&rows, RestoredTrstChanges::default())
        .is_err());
    let mut changes = RestoredTrstChanges::default();
    changes.portfolios.insert(holder.clone(), None);
    let rewound = state
        .retained_graph_removal(&rows, changes)
        .unwrap()
        .unwrap();
    assert!(!rewound.revoked_token_origins.contains_key(&merge));
    assert!(rewound.engine().merger_graph == graph_before);
    RestoredTrstState::from_engine(rewound.into_engine()).unwrap();
    assert_eq!(state.revoked_token_origins.get(&merge), Some(&1));
}

fn revision_fixture() -> TrstEngine {
    let holder = WalletAddress::new("brst_owner_1");
    let mut engine = fixture();
    let extra = engine
        .mint(
            TxHash::new([4; 32]),
            holder.clone(),
            30,
            WalletAddress::new("brst_owner_2"),
            Timestamp::new(100),
        )
        .unwrap();
    engine.track_token(extra);
    let ids = [TxHash::new([1; 32]), TxHash::new([4; 32])];
    let inputs = engine.wallets[&holder].select_merge_inputs(&ids).unwrap();
    let merged = engine
        .merge(
            &inputs,
            holder.clone(),
            TxHash::new([90; 32]),
            Timestamp::new(100),
        )
        .unwrap();
    engine.bulk_untrack(&holder, &ids.into_iter().collect());
    engine.track_token(merged);
    engine
}

fn inverse_graph_rows(
    before: &TrstEngine,
    current: &TrstEngine,
) -> Vec<(
    Option<crate::merger_graph::GraphRecord>,
    Option<crate::merger_graph::GraphRecord>,
)> {
    let mut rows = Vec::new();
    current
        .merger_graph
        .visit_record_delta(&before.merger_graph, |old, new| {
            rows.push((new, old));
            Ok(())
        })
        .unwrap();
    rows
}
fn inverse_wallet_records(before: &TrstEngine, current: &TrstEngine) -> RestoredTrstChanges {
    let mut changes = RestoredTrstChanges::default();
    before
        .visit_portfolio_changes(current, |holder, _, new| {
            changes.portfolios.insert(holder.clone(), new.cloned());
            Ok::<_, String>(())
        })
        .unwrap();
    for origin in before
        .outcome_invalidations
        .changed_keys(&current.outcome_invalidations)
    {
        changes.invalidations.insert(
            *origin,
            before
                .outcome_invalidations
                .get(origin)
                .map(|value| RestoredTrstInvalidation {
                    wallet_revoked: value.wallet_revoked,
                    causes: value.causes.clone(),
                }),
        );
    }
    changes
}

#[test]
fn retained_revision_undo_matches_cold_state_with_outcomes_and_multiple_bases() {
    let original = revision_fixture();
    let owner = WalletAddress::new("brst_owner_1");
    let mut revoked = original.clone();
    revoked.revoke_by_originator(&owner);
    let mut restored = revoked.clone();
    restored
        .restore_by_originator(&owner, Timestamp::new(100))
        .unwrap();
    let mut outcome_before = original.clone();
    let other = WalletAddress::new("brst_owner_2");
    outcome_before
        .invalidate_outcome_origins(
            BlockHash::new([77; 32]),
            &[(other.clone(), TxHash::new([4; 32]))],
        )
        .unwrap();
    outcome_before.revoke_by_originator(&other);
    let mut outcome_after = outcome_before.clone();
    outcome_after
        .restore_by_originator(&other, Timestamp::new(100))
        .unwrap();
    for (before, current) in [
        (&original, &revoked),
        (&revoked, &restored),
        (&outcome_before, &outcome_after),
    ] {
        let mut state = RestoredTrstState::from_engine(current.clone()).unwrap();
        state.retain_portfolio_indexes().unwrap();
        let rows = inverse_graph_rows(before, current);
        let next = state
            .retained_graph_rewind(&rows, inverse_wallet_records(before, current))
            .unwrap()
            .unwrap();
        let full = RestoredTrstState::from_engine(before.clone()).unwrap();
        assert_eq!(next.engine().save_wallets(), before.save_wallets());
        assert!(next.engine().merger_graph == before.merger_graph);
        assert_eq!(next.revision_index, full.revision_index);
        assert_eq!(next.token_revisions, full.token_revisions);
        assert_eq!(next.revoked_token_origins, full.revoked_token_origins);
        assert_eq!(state.engine().save_wallets(), current.save_wallets());
    }
}

#[test]
fn revision_undo_rejects_a_surviving_held_remainder_and_invalid_outcome_patch() {
    let owner = WalletAddress::new("brst_owner_1");
    let unrelated = WalletAddress::new("brst_owner_3");
    let mut before = revision_fixture();
    before.revoke_by_originator(&owner);
    let mut after = before.clone();
    after
        .restore_by_originator(&owner, Timestamp::new(100))
        .unwrap();
    let rows = inverse_graph_rows(&before, &after);
    let merge = TxHash::new([90; 32]);
    let basis = after.merger_graph.current_basis(&merge);
    assert!(matches!(basis, TrstBasis::Remainder(_)));
    let mut with_reference = after.clone();
    let portfolio = with_reference.wallets.get_mut(&unrelated).unwrap();
    portfolio.tokens[0].origin = merge;
    portfolio.tokens[0].basis = basis;
    let state = RestoredTrstState::from_engine(with_reference.clone()).unwrap();
    // Patch the original transition's holdings, leaving the later reference.
    assert!(state
        .retained_graph_rewind(&rows, inverse_wallet_records(&before, &after))
        .is_err());
    let next = state
        .retained_graph_rewind(&rows, inverse_wallet_records(&before, &with_reference))
        .unwrap()
        .unwrap();
    assert_eq!(next.engine().save_wallets(), before.save_wallets());
    let mut changes = RestoredTrstChanges::default();
    changes.invalidations.insert(
        TxHash::new([1; 32]),
        Some(RestoredTrstInvalidation {
            wallet_revoked: true,
            causes: HashSet::from([BlockHash::ZERO]),
        }),
    );
    let mut unchanged = next.clone();
    assert!(unchanged.replace_records(changes).is_err());
    assert_eq!(unchanged.engine().save_wallets(), before.save_wallets());
}

#[test]
fn comparison_restoration_matches_mutation_validation_and_checked_counts() {
    let mut engine = fixture();
    engine.merger_graph.record_merge(MergeNode {
        merge_tx: TxHash::new([90; 32]),
        source_origins: vec![
            MergeSource {
                origin: TxHash::new([1; 32]),
                amount: 10,
            },
            MergeSource {
                origin: TxHash::new([2; 32]),
                amount: 10,
            },
        ],
        total_amount: 20,
        revoked_contribs: Default::default(),
    });
    let unrevoked = engine.clone();
    engine.revoke_by_originator(&WalletAddress::new("brst_owner_1"));
    for baseline in [unrevoked, engine] {
        let stages = std::cell::RefCell::new(Vec::new());
        let full = RestoredTrstState::from_engine_with_poll(baseline.clone(), &|stage| {
            stages.borrow_mut().push(stage);
            Ok(())
        })
        .unwrap();
        let comparison_stages = std::cell::RefCell::new(Vec::new());
        let read =
            RestoredTrstState::validate_for_comparison_with_poll(baseline.clone(), &|stage| {
                comparison_stages.borrow_mut().push(stage);
                Ok(())
            })
            .unwrap();
        assert_eq!(stages.into_inner(), comparison_stages.into_inner());
        assert_eq!(read.save_wallets(), full.engine.save_wallets());
        assert!(read.merger_graph == full.engine.merger_graph);
        assert!(read.origin_holders == full.engine.origin_holders);
        assert!(read.revoked_holders == full.engine.revoked_holders);
        let counts = baseline
            .merger_graph
            .build_revocation_counts_with_poll::<HashMap<TxHash, usize>>(&|_| Ok(()))
            .unwrap();
        assert_eq!(
            counts,
            full.revocation_counts
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect()
        );
        let (revoked, revisions) = RestoredTrstState::build_token_counts_with_poll::<
            HashMap<TxHash, usize>,
        >(&baseline, &|_| Ok(()))
        .unwrap();
        assert_eq!(
            revoked,
            full.revoked_token_origins
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect()
        );
        assert_eq!(
            revisions,
            full.token_revisions.iter().map(|(k, v)| (*k, *v)).collect()
        );
        for mutation in 0..4 {
            let mut bad = baseline.clone();
            let owner = WalletAddress::new("brst_owner_2");
            match mutation {
                0 => bad.wallet_origins.clear(),
                1 => bad.wallets.get_mut(&owner).unwrap().cached_transferable += 1,
                2 => bad.wallets.get_mut(&owner).unwrap().earliest_expiry = None,
                _ => {
                    bad.outcome_invalidations.insert(
                        TxHash::new([2; 32]),
                        OutcomeInvalidation {
                            causes: HashSet::new(),
                            wallet_revoked: false,
                        },
                    );
                }
            }
            let old = RestoredTrstState::from_engine(bad.clone()).err().unwrap();
            let new = RestoredTrstState::validate_for_comparison_with_poll(bad, &|_| Ok(()))
                .err()
                .unwrap();
            assert_eq!(old, new);
        }
    }
}
