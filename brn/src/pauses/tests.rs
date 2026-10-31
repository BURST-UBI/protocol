use super::*;
use crate::{BrnWalletState, RateHistory};
use proptest::prelude::*;

#[test]
fn shared_pause_snapshots_preserve_sequence_bytes_and_exact_deltas() {
    let rows: Vec<_> = (0..1024)
        .map(|i| (Timestamp::new(i * 2), Timestamp::new(i * 2 + 1)))
        .collect();
    let before: PauseHistory = rows.clone().into();
    let mut after = before.clone();
    assert!(before.rows.ptr_eq(&after.rows));
    assert_eq!(
        bincode::serialize(&before).unwrap(),
        bincode::serialize(&rows).unwrap()
    );
    let mut changes = Vec::new();
    after
        .visit_changes(&before, |i, old, new| {
            changes.push((i, old.copied(), new.copied()));
            Ok::<_, ()>(())
        })
        .unwrap();
    assert!(changes.is_empty());
    let added = (Timestamp::new(3000), Timestamp::new(4000));
    after.push(added);
    after
        .visit_changes(&before, |i, old, new| {
            changes.push((i, old.copied(), new.copied()));
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(changes, vec![(1024, None, Some(added))]);
    assert_eq!(before, rows);
    let mut visited = 0;
    assert_eq!(
        PauseHistory::default().visit_changes(&after, |_, _, _| {
            visited += 1;
            Err("stop")
        }),
        Err("stop")
    );
    assert_eq!(visited, 1);
    after.clear();
    assert!(after.is_empty());
    assert_eq!(before.len(), 1024);
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    wallet.paused_intervals = before;
    let mut changed = wallet.clone();
    changed.total_burned = 1;
    assert!(wallet
        .paused_intervals
        .rows
        .ptr_eq(&changed.paused_intervals.rows));
    let mut keys = Vec::new();
    crate::wallet_storage::visit_delta(Some(&wallet), Some(&changed), |key, _, _| {
        keys.push(key.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(keys, vec![crate::wallet_storage::HEADER]);
    changed.paused_intervals.push(added);
    keys.clear();
    crate::wallet_storage::visit_delta(Some(&wallet), Some(&changed), |key, _, _| {
        keys.push(key.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[1], [&[1][..], &1024u64.to_be_bytes()].concat());
}

// A Vec-backed wallet provides an independent encoding and interval-loop reference.
#[derive(Serialize)]
struct LegacyWallet<'a> {
    verified_at: Timestamp,
    total_burned: u128,
    total_staked: u128,
    accrual_active: bool,
    accrual_stopped_at: Option<Timestamp>,
    paused_intervals: &'a Vec<(Timestamp, Timestamp)>,
    pending_releases: &'a crate::releases::PendingReleases,
}

proptest! {
    #[test]
    fn arbitrary_pause_history_keeps_legacy_balance_and_wallet_encoding(
        raw in prop::collection::vec((0u64..1000, 0u64..1000), 0..60),
        now in 0u64..1200, rate in any::<u128>(), burned in 0u128..1000,
        staked in 0u128..1000, active in any::<bool>(), stopped in 0u64..1200,
    ) {
        // Deliberately include unordered, overlapping and backwards intervals.
        let rows: Vec<_> = raw.into_iter().map(|(a,b)| (Timestamp::new(a),Timestamp::new(b))).collect();
        let mut wallet = BrnWalletState::new(Timestamp::new(10));
        wallet.paused_intervals = rows.clone().into();
        wallet.total_burned = burned;
        wallet.total_staked = staked;
        wallet.accrual_active = active;
        wallet.accrual_stopped_at = Some(Timestamp::new(stopped));
        let rates = RateHistory::new(rate, Timestamp::EPOCH);
        let effective = Timestamp::new(if active { now } else { stopped.min(now) });
        let expected = (|| {
            let mut sum = 0u128;
            let mut cursor = wallet.verified_at;
            for &(start, end) in &rows {
                if start >= effective { break; }
                sum = sum.checked_add(rates.total_accrued_checked(cursor, start.min(effective))?)?;
                cursor = cursor.max(end);
            }
            sum.checked_add(rates.total_accrued_checked(cursor, effective)?)?
                .checked_sub(burned)?.checked_sub(staked)
        })();
        prop_assert_eq!(wallet.available_balance_checked(&rates, Timestamp::new(now)), expected);
        let old = LegacyWallet { verified_at: wallet.verified_at, total_burned: burned,
            total_staked: staked, accrual_active: active, accrual_stopped_at: wallet.accrual_stopped_at,
            paused_intervals: &rows, pending_releases: &wallet.pending_releases };
        let bytes = bincode::serialize(&old).unwrap();
        prop_assert_eq!(bincode::serialize(&wallet).unwrap(), bytes.clone());
        prop_assert_eq!(BrnWalletState::from_persisted_bytes(&bytes).unwrap(), wallet);
    }
}
