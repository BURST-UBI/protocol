use super::*;
use crate::{
    checkpoint_agreement::ReplayManifest, checkpoint_deadline::PreparedDeadline,
    checkpoint_store::chain, confirmed_replay::installation::PreparedInstallation,
};

fn pending_fixture() -> (Fixture, BlockHash) {
    let mut fixture = Fixture::new();
    let burn = fixture.push(0, BlockType::Burn, 100, |block| {
        block.brn_spent = 40;
        block.link = destination(1);
    });
    (fixture, burn)
}

#[test]
fn expired_self_payment_replaces_pending_with_one_return_credit() {
    let mut fixture = Fixture::new();
    let burn = fixture.push(0, BlockType::Burn, 100, |block| {
        block.brn_spent = 40;
        block.link = destination(0);
    });
    let history = fixture.history(1100);
    let mut replay =
        ConfirmedReplay::replay_deadline_candidate(&history, NetworkId::Dev, &fixture.params, &[])
            .unwrap();
    assert!(replay
        .apply_agreed_deadlines(&history, &fixture.params)
        .unwrap()
        .is_some());
    assert_eq!(replay.pending.len(), 1);
    assert!(
        replay
            .pending(&address(0), burn.into_tx_hash())
            .unwrap()
            .return_credit
    );
    assert!(replay
        .apply_agreed_deadlines(&history, &fixture.params)
        .unwrap()
        .is_none());
    assert_eq!(
        replay
            .portfolio(&address(0))
            .unwrap()
            .tokens
            .iter()
            .map(|token| token.amount)
            .sum::<u128>(),
        40
    );
}

#[test]
fn pending_expiry_requires_a_certificate_and_can_be_the_first_checkpoint() {
    let (mut fixture, burn) = pending_fixture();
    let (_dir, store, _) = fixture.store();
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, 1099)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        fixture.replay(1100),
        Err(ReplayError::PendingExpiry)
    ));
    let prepared = PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, 1100)
        .unwrap()
        .unwrap();
    let manifest = prepared.manifest().clone();
    assert_eq!(manifest.checkpoint.height, 1);
    assert!(PreparedDeadline::replay_manifest(&store, &fixture.params, &manifest).is_ok());
    let mut changed = manifest.clone();
    changed.checkpoint.selection_root = BlockHash::new([99; 32]);
    assert!(PreparedDeadline::replay_manifest(&store, &fixture.params, &changed).is_err());
    let certificate = certified_votes::certify(&manifest.checkpoint, &[0]);
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate,
        &ReplayManifest::Deadline(manifest.clone()),
    )
    .unwrap();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(1101),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(replay.pending.len(), 1);
    assert!(
        replay
            .pending(&address(0), burn.into_tx_hash())
            .unwrap()
            .return_credit
    );
    assert!(replay.portfolio(&address(1)).is_none());
    let sender = replay.portfolio(&address(0)).unwrap();
    assert_eq!(sender.tokens.len(), 1);
    assert_eq!(sender.tokens[0].amount, 40);
    assert_eq!(sender.tokens[0].origin, burn.into_tx_hash());
    assert_eq!(sender.tokens[0].state, burst_types::TrstState::Expired);
    assert_eq!(sender.cached_transferable, 0);
    assert_eq!(replay.account(&address(0)).unwrap().expired_trst, 40);
    assert_eq!(replay.account(&address(0)).unwrap().trst_balance, 0);
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(
        store
            .account_store()
            .get_account(&address(0))
            .unwrap()
            .expired_trst,
        40
    );
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, 1101)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        manifest.checkpoint.hash()
    );
    fixture.push(1, BlockType::Open, 1101, |block| {
        block.link = burn;
        block.trst_balance = 40;
        block.payment_epoch = manifest.checkpoint.hash();
    });
    let error = ConfirmedReplay::replay_with_events(
        &fixture.history(1101),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .expect("returned transfer was claimed twice");
    assert!(
        error.to_string().contains("missing replayed pending"),
        "{error}"
    );
}

#[test]
fn pending_expiry_honors_a_receive_before_expiry_and_rejects_one_at_expiry() {
    let (mut fixture, burn) = pending_fixture();
    fixture.push(1, BlockType::Open, 1099, |block| {
        block.link = burn;
        block.trst_balance = 40;
    });
    let history = fixture.history(1100);
    let mut replay =
        ConfirmedReplay::replay_deadline_candidate(&history, NetworkId::Dev, &fixture.params, &[])
            .unwrap();
    assert!(replay
        .apply_agreed_deadlines(&history, &fixture.params)
        .unwrap()
        .is_none());
    assert_eq!(replay.account(&address(1)).unwrap().expired_trst, 40);
    assert!(replay.portfolio(&address(0)).is_none());
    let receive = fixture.blocks.last_mut().unwrap();
    receive.timestamp = Timestamp::new(1100);
    sign(1, receive);
    assert!(matches!(
        ConfirmedReplay::replay_deadline_candidate(
            &fixture.history(1100),
            NetworkId::Dev,
            &fixture.params,
            &[]
        ),
        Err(ReplayError::PendingExpiry)
    ));
}

#[test]
fn pending_expiry_commitment_is_independent_of_pending_map_order() {
    let (mut fixture, _) = pending_fixture();
    fixture.push(0, BlockType::Burn, 101, |block| {
        block.brn_spent = 60;
        block.link = destination(2);
    });
    let history = fixture.history(1101);
    let mut left =
        ConfirmedReplay::replay_deadline_candidate(&history, NetworkId::Dev, &fixture.params, &[])
            .unwrap();
    let mut right =
        ConfirmedReplay::replay_deadline_candidate(&history, NetworkId::Dev, &fixture.params, &[])
            .unwrap();
    let mut entries: Vec<_> = right.pending.drain().collect();
    entries.reverse();
    right.pending.extend(entries);
    assert_eq!(
        left.apply_agreed_deadlines(&history, &fixture.params)
            .unwrap(),
        right
            .apply_agreed_deadlines(&history, &fixture.params)
            .unwrap()
    );
    assert_eq!(left.account(&address(0)).unwrap().expired_trst, 60);
    assert!(left
        .apply_agreed_deadlines(&history, &fixture.params)
        .unwrap()
        .is_none());
}

#[test]
fn pending_expiry_returns_a_send_to_its_sender_not_its_burn_originator() {
    let mut fixture = Fixture::new();
    fixture.push(0, BlockType::Endorse, 90, |block| {
        block.brn_spent = 1;
        block.link = destination(1);
    });
    let burn = fixture.push(0, BlockType::Burn, 100, |block| {
        block.brn_spent = 41;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |block| {
        block.link = burn;
        block.trst_balance = 40;
    });
    fixture.push(1, BlockType::Send, 102, |block| {
        block.link = destination(2);
        block.trst_balance = 0;
        block.origin = burn.into_tx_hash();
    });
    let history = fixture.history(1100);
    let mut replay =
        ConfirmedReplay::replay_deadline_candidate(&history, NetworkId::Dev, &fixture.params, &[])
            .unwrap();
    assert!(replay
        .apply_agreed_deadlines(&history, &fixture.params)
        .unwrap()
        .is_some());
    assert_eq!(replay.account(&address(0)).unwrap().expired_trst, 0);
    assert_eq!(replay.account(&address(1)).unwrap().expired_trst, 40);
    let returned = replay
        .portfolio(&address(1))
        .unwrap()
        .tokens
        .iter()
        .find(|token| token.amount == 40)
        .unwrap();
    assert_eq!(returned.origin, burn.into_tx_hash());
    assert_eq!(returned.origin_wallet, address(0));
    assert_eq!(returned.holder, address(1));
    assert!(replay.portfolio(&address(2)).is_none());
}
