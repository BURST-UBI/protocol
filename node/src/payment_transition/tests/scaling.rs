use super::*;
use std::time::Instant;

const MAP_SIZE: usize = 512 * 1024 * 1024;

fn seeded(fragments: u64, unrelated_brn: u64) -> (Fixture, StateBlock) {
    let fixture = Fixture::with_capacity(1_000_000, MAP_SIZE);
    {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut state = restore(&batch, &fixture.params).unwrap();
        for n in 0..unrelated_brn {
            let mut bytes = [37; 32]; bytes[..8].copy_from_slice(&n.to_be_bytes());
            let address = burst_crypto::derive_address(&PublicKey(bytes));
            state.brn.track_wallet(address, burst_brn::BrnWalletState::new(Timestamp::new(0)));
        }
        // Population records are synthetic background BRN state. The affected
        // wallet's holdings below are built through signed economic transitions.
        ledger_updater::persist_brn(&mut batch, &state.brn).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &state.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &state.trst).unwrap();
        batch.commit().unwrap();
    }
    let mut time = fixture.params.new_wallet_rate_limit_duration_secs + 100;
    let link = BlockHash::new(burst_crypto::decode_address(fixture.address.as_str()).unwrap());
    let mut burn = fixture.next(BlockType::Burn, time);
    burn.brn_spent = fragments as u128;
    burn.link = link;
    fixture.commit(&mut burn);
    let origin = burn.hash.into_tx_hash();
    time += 1;
    let mut receive = fixture.next(BlockType::Receive, time);
    receive.link = burn.hash;
    receive.trst_balance = fragments as u128;
    fixture.commit(&mut receive);
    for _ in 1..fragments {
        time += 1;
        let mut send = fixture.next(BlockType::Send, time);
        send.link = link; send.origin = origin; send.trst_balance -= 1;
        fixture.commit(&mut send);
        time += 1;
        let mut receive = fixture.next(BlockType::Receive, time);
        receive.link = send.hash; receive.trst_balance += 1;
        fixture.commit(&mut receive);
    }
    {
        let batch = fixture.store.tx_begin_write().unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        assert_eq!(state.trst.get_portfolio(&fixture.address).unwrap().tokens.len(), fragments as usize);
        assert_eq!(batch.get_account(&fixture.address).unwrap().trst_balance, fragments as u128);
    }
    let mut send = fixture.next(BlockType::Send, time + 1);
    send.link = link; send.origin = origin; send.trst_balance = 0;
    sign(&mut send);
    (fixture, send)
}

fn output(batch: &LmdbWriteTransaction<'_>, fixture: &Fixture, block: &StateBlock) -> Vec<u8> {
    let account = batch.get_account(&fixture.address).unwrap();
    let pending = batch.get_pending(&fixture.address, &block.hash.into_tx_hash()).unwrap();
    assert_eq!(account.head, block.hash);
    assert_eq!(account.trst_balance, 0);
    assert_eq!(account.expired_trst, 0);
    assert_eq!(account.revoked_trst, 0);
    assert_eq!(pending.provenance.iter().map(|p| p.amount).sum::<u128>(), pending.amount);
    let state = restore(batch, &fixture.params).unwrap();
    assert_eq!(state.trst.transferable_balance_snapshot(&fixture.address).unwrap_or(0), 0);
    bincode::serialize(&(account, pending, state.trst.save_wallets())).unwrap()
}

fn exercise(fragments: u64, unrelated_brn: u64) {
    let start = Instant::now();
    let (fixture, block) = seeded(fragments, unrelated_brn);
    let seed_ns = start.elapsed().as_nanos();
    let (expected, reference_apply_ns) = {
        let start = Instant::now();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        apply(&mut batch, &block, &fixture.params, block.timestamp).unwrap();
        let elapsed = start.elapsed().as_nanos();
        let pending = batch.get_pending(&fixture.address, &block.hash.into_tx_hash()).unwrap();
        assert_eq!(pending.amount, fragments as u128);
        (output(&batch, &fixture, &block), elapsed)
    };
    let Fixture { directory, store, params, address } = fixture;
    drop(store);
    let fixture = Fixture { store: LmdbStore::open(directory.path(), 32, MAP_SIZE).unwrap(), directory, params, address };
    for sample in 0..4 {
        let total = Instant::now();
        let start = Instant::now();
        authenticate(&block, &fixture.params, block.timestamp).unwrap();
        let auth_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let writer_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        let state = restore(&batch, &fixture.params).unwrap();
        let mut staged = StagedEngines::new(&state.brn, &state.trst);
        let restore_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        crate::economic_candidate::stage(&mut batch, &block, &fixture.params, &mut staged).unwrap();
        let stage_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        ledger_updater::refresh_token_counters(&mut batch, &block.account, staged.trst(), block.timestamp).unwrap();
        let counters_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        staged.persist(&mut batch).unwrap();
        let persist_ns = start.elapsed().as_nanos();
        let prepared_ns = total.elapsed().as_nanos();
        assert_eq!(output(&batch, &fixture, &block), expected);
        drop(staged); drop(state);
        let start = Instant::now();
        if sample == 3 { batch.commit().unwrap(); } else { drop(batch); }
        let finish_ns = start.elapsed().as_nanos();
        let batch = fixture.store.tx_begin_write().unwrap();
        if sample == 3 {
            assert_eq!(output(&batch, &fixture, &block), expected);
            assert!(batch.block_undo(&block.hash).unwrap().is_some());
        } else {
            assert_eq!(batch.get_account(&fixture.address).unwrap().head, block.previous);
            assert!(batch.get_block(&block.hash).is_err());
            assert!(batch.get_pending(&fixture.address, &block.hash.into_tx_hash()).is_err());
            // Publish the unchanged retained cut before warm samples.
            restore(&batch, &fixture.params).unwrap();
            batch.commit().unwrap();
        }
        let rss = std::fs::read_to_string("/proc/self/status").unwrap().lines()
            .filter(|line| line.starts_with("VmRSS:")).collect::<Vec<_>>().join(" ");
        println!("signed_send fragments={fragments} unrelated_brn={unrelated_brn} sample={sample} seed_ns={seed_ns} reference_apply_ns={reference_apply_ns} auth_ns={auth_ns} writer_ns={writer_ns} restore_ns={restore_ns} stage_ns={stage_ns} counters_ns={counters_ns} persist_ns={persist_ns} prepared_ns={prepared_ns} finish_ns={finish_ns} committed={} {rss}", sample == 3);
    }
}

#[test]
fn signed_fragmented_send_stages_match_apply_and_abort() { exercise(8, 3); }

#[test]
#[ignore = "explicit capped-host signed shared payment transition scaling; not consensus throughput"]
fn signed_fragmented_payment_scaling() {
    for fragments in [1, 10, 100, 1_000] { exercise(fragments, 0); }
    for unrelated in [1_000, 10_000] { exercise(1, unrelated); }
}

fn signed_merge_replacement_case(unrelated: u64, shared_parent: bool) {
    use burst_trst::merger_graph::{MergeNode, MergeSource};
    let fixture = Fixture::with_capacity(1_000_000, MAP_SIZE);
    let ids = fixture.create_holdings();
    let background_owner = burst_crypto::derive_address(&PublicKey([73; 32]));
    let expected_graph = {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let mut state = restore(&batch, &fixture.params).unwrap();
        // Background history is synthetic; the affected burns, receipts, merge
        // and competing replacement below use the signed transition fixture.
        for n in 0..unrelated {
            let hash = |domain| {
                let mut bytes = [domain; 32]; bytes[..8].copy_from_slice(&n.to_be_bytes());
                TxHash::new(bytes)
            };
            let origin = if shared_parent {
                state.trst.get_portfolio(&fixture.address).unwrap().tokens[0].origin
            } else {
                let origin = hash(40);
                state.trst.mint(origin, background_owner.clone(), 1,
                    background_owner.clone(), Timestamp::new(0)).unwrap();
                origin
            };
            state.trst.merger_graph.record_merge(MergeNode {
                merge_tx: hash(41), source_origins: vec![MergeSource { origin, amount: 1 }],
                total_amount: 1, revoked_contribs: Default::default(),
            });
        }
        crate::graph_storage::stage_replayed(&mut batch, &state.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &state.trst).unwrap();
        batch.commit().unwrap();
        state.trst.merger_graph
    };
    let mut replacement = fixture.next(BlockType::ChangeRepresentative, 105);
    sign(&mut replacement);
    let mut merge = fixture.next(BlockType::Merge, 104);
    merge.merge_sources = ids;
    fixture.commit(&mut merge);
    // Establish the committed losing cut before every independent evaluation.
    for sample in 0..3 {
        let batch = fixture.store.tx_begin_write().unwrap();
        let state = restore(&batch, &fixture.params).unwrap();
        assert!(state.trst.merger_graph.contains_merge(&merge.hash.into_tx_hash()));
        drop(state);
        batch.commit().unwrap();
        let started = Instant::now();
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let writer_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        let warm = restore(&batch, &fixture.params).unwrap();
        let warm_restore_ns = started.elapsed().as_nanos();
        drop(warm);
        let started = Instant::now();
        let mut rewind_ns = 0;
        let mut apply_ns = 0;
        let (result, height) = crate::rollback_plan::stage_with(
            &mut batch, merge.hash, &replacement, |batch, block| {
                rewind_ns = started.elapsed().as_nanos();
                let applying = Instant::now();
                let state = apply(batch, block, &fixture.params, block.timestamp);
                apply_ns = applying.elapsed().as_nanos();
                state
            }).unwrap();
        let staged_ns = started.elapsed().as_nanos();
        assert_eq!(result.removed.len(), 1);
        assert!(result.state.trst.merger_graph == expected_graph);
        assert_eq!(result.state.trst.get_portfolio(&fixture.address).unwrap().tokens.len(), 2);
        assert_eq!(batch.get_account(&fixture.address).unwrap().head, replacement.hash);
        drop(result);
        let finishing = Instant::now();
        if sample == 2 {
            batch.commit_block(&replacement.hash, &replacement.account, height).unwrap();
        } else { drop(batch); }
        let finish_ns = finishing.elapsed().as_nanos();
        let batch = fixture.store.tx_begin_write().unwrap();
        assert_eq!(batch.get_account(&fixture.address).unwrap().head,
            if sample == 2 { replacement.hash } else { merge.hash });
        let state = restore(&batch, &fixture.params).unwrap();
        if sample == 2 { assert!(state.trst.merger_graph == expected_graph); }
        else { assert!(state.trst.merger_graph.contains_merge(&merge.hash.into_tx_hash())); }
        let memory: Vec<_> = std::fs::read_to_string("/proc/self/status").unwrap().lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .map(str::to_owned).collect();
        println!("signed_merge_replacement {}", serde_json::json!({
            "unrelated_merges":unrelated, "shared_parent":shared_parent, "sample":sample, "committed":sample == 2,
            "writer_ns":writer_ns, "warm_restore_ns":warm_restore_ns,
            "plan_rewind_ns":rewind_ns, "apply_ns":apply_ns, "staged_ns":staged_ns,
            "finish_ns":finish_ns, "memory":memory,
        }));
    }
}

#[test]
fn signed_merge_replacement_probe_preserves_background_and_abort() {
    signed_merge_replacement_case(8, false);
    signed_merge_replacement_case(8, true);
}

#[test]
#[ignore = "explicit capped-host signed merge replacement diagnostic; not consensus throughput"]
fn signed_merge_replacement_scaling() {
    for unrelated in [100, 1_000, 10_000] { signed_merge_replacement_case(unrelated, false); }
}

#[test]
#[ignore = "explicit capped-host shared-parent merge rewind diagnostic; not consensus throughput"]
fn signed_shared_parent_replacement_scaling() {
    for siblings in [100, 1_000, 10_000] { signed_merge_replacement_case(siblings, true); }
}
