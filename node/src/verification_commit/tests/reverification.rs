use super::*;

#[test]
fn reverification_preserves_frozen_brn_and_outstanding_release_proofs() {
    for previous in [WalletState::Revoked, WalletState::Deactivated] {
        let wallet = address(20);
        let mut account = burst_store::account::AccountInfo::unopened(wallet.clone());
        account.state = previous;
        account.total_brn_burned = 20;
        account.total_brn_staked = 10;
        let mut brn = BrnEngine::with_rate(3, Timestamp::new(10));
        let mut state = burst_brn::BrnWalletState::new(Timestamp::new(10));
        state.total_burned = 20;
        state.total_staked = 10;
        state.stop_accrual(Timestamp::new(30));
        state.resume_accrual(Timestamp::new(40));
        state
            .record_release(
                BlockHash::new([1; 32]),
                5,
                &[BlockHash::new([1; 32]), BlockHash::new([2; 32])],
                Timestamp::new(50),
            )
            .unwrap();
        state.stop_accrual(Timestamp::new(100));
        let frozen = state
            .available_balance_checked(&brn.rate_history, Timestamp::new(100))
            .unwrap();
        let releases = bincode::serialize(&state.pending_releases).unwrap();
        brn.track_wallet(wallet.clone(), state);
        verify_account(&mut account, Timestamp::new(200), &mut brn).unwrap();
        let restored = brn.get_wallet(&wallet).unwrap();
        assert_eq!(account.state, WalletState::Verified);
        assert_eq!(account.total_brn_burned, 20);
        assert_eq!(account.total_brn_staked, 10);
        assert_eq!(
            restored.verified_at,
            Timestamp::new(10),
            "{previous:?} lost earned BRN"
        );
        assert_eq!(restored.total_burned, 20);
        assert_eq!(restored.total_staked, 10);
        assert_eq!(
            bincode::serialize(&restored.pending_releases).unwrap(),
            releases
        );
        assert_eq!(
            restored.paused_intervals,
            vec![
                (Timestamp::new(30), Timestamp::new(40)),
                (Timestamp::new(100), Timestamp::new(200))
            ]
        );
        assert_eq!(
            restored.available_balance_checked(&brn.rate_history, Timestamp::new(200)),
            Some(frozen)
        );
        assert_eq!(
            restored.spendable_balance_checked(&brn.rate_history, Timestamp::new(210)),
            Some(frozen + 30 - 5)
        );
    }
}
