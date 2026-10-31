use super::*;

fn fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.push(1, BlockType::Open, 10, |_| {});
    fixture.push(1, BlockType::ChangeSigner, 11, |block| {
        block.link = BlockHash::new([77; 32]);
    });
    fixture.push(1, BlockType::Epoch, 12, |_| {});
    sign(0, fixture.blocks.last_mut().unwrap());
    fixture
}

#[test]
fn protocol_epoch_replays_creator_signature_and_preserves_account_rights() {
    let mut fixture = fixture();
    let marker = fixture.blocks.last().unwrap().clone();
    let state = fixture.replay(20).unwrap();
    let account = state.account(&address(1)).unwrap();
    assert_eq!(account.epoch, burst_ledger::CURRENT_BLOCK_VERSION);
    assert_eq!(account.head, marker.hash);
    assert_eq!(account.representative, address(1));
    assert_eq!(account.trst_balance, 0);
    assert_eq!(account.state, WalletState::Unverified);
    assert_eq!(marker.active_signing_grant(), fixture.blocks[2].hash);

    // The owner's next action still uses the same device grant, not the creator.
    fixture.push(1, BlockType::ChangeRepresentative, 13, |block| {
        block.representative = address(2);
    });
    let expected = fixture.replay(20).unwrap();
    assert_eq!(expected.account(&address(1)).unwrap().epoch, marker.version);
    let (dir, store, heads) = fixture.store();
    drop(store);
    let reopened = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let history = ConfirmedHistory::read(
        &reopened,
        NetworkId::Dev,
        fixture.blocks[0].hash,
        20,
        &heads,
        &[],
    )
    .unwrap();
    let restored = ConfirmedReplay::replay(&history, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(
        bincode::serialize(restored.account(&address(1)).unwrap()).unwrap(),
        bincode::serialize(expected.account(&address(1)).unwrap()).unwrap(),
    );
}

#[test]
fn protocol_epoch_rejects_owner_signature_and_creator_account_actions() {
    let mut wrong_signer = fixture();
    sign(1, wrong_signer.blocks.last_mut().unwrap());
    wrong_signer.assert_rejected("invalid block signature");
    for case in 0..5 {
        let mut fixture = fixture();
        let block = fixture.blocks.last_mut().unwrap();
        match case {
            0 => block.representative = address(2),
            1 => block.brn_spent = 1,
            2 => block.link = BlockHash::new([88; 32]),
            3 => block.release_sources.push(BlockHash::new([89; 32])),
            4 => block.signing_grant = BlockHash::ZERO,
            _ => unreachable!(),
        }
        sign(0, block);
        if matches!(case, 2 | 3) {
            let (_dir, store, heads) = fixture.store();
            let error = ConfirmedHistory::read(
                &store,
                NetworkId::Dev,
                fixture.blocks[0].hash,
                20,
                &heads,
                &[],
            )
            .err()
            .expect("malformed epoch must fail history decoding");
            assert!(
                error
                    .to_string()
                    .contains("epoch marker cannot contain an account action"),
                "{error}"
            );
        } else {
            fixture.assert_rejected(if case == 4 {
                "signing-device grant is no longer active"
            } else {
                "epoch marker cannot change balances or representative"
            });
        }
    }
}
