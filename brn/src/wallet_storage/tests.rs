use super::*;
use std::collections::BTreeMap;

#[test]
fn retention_rows_preserve_boundaries_release_proofs_and_callback_stop() {
    use crate::retention::Reference::{Account, Block, Boundary, Outcome};
    let owner = burst_types::WalletAddress::new("brst_owner");
    let mut state = BrnWalletState::new(Timestamp::new(1));
    state.stop_accrual(Timestamp::new(10));
    state.paused_intervals.push((Timestamp::new(3), Timestamp::new(4)));
    state.record_release(hash(1), 2, &[hash(1), hash(2)], Timestamp::new(6)).unwrap();
    state.record_unproven_release(hash(3), 2, Timestamp::new(7)).unwrap();
    let encoded = rows(&state);
    let cases = [
        (HEADER.to_vec(), vec![Account(&owner), Boundary(Timestamp::new(1)), Boundary(Timestamp::new(10))]),
        (pause_key(0), vec![Account(&owner), Boundary(Timestamp::new(3)), Boundary(Timestamp::new(4))]),
        (release_key(&hash(1)), vec![Account(&owner), Outcome(hash(1)), Boundary(Timestamp::new(6)), Block(hash(1)), Block(hash(2))]),
        (release_key(&hash(3)), vec![Account(&owner), Outcome(hash(3)), Boundary(Timestamp::new(7))]),
    ];
    for (key, expected) in cases {
        let bytes = &encoded[&key];
        let mut actual = Vec::new();
        visit_retention_references(&owner, &key, bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
        assert_eq!(actual, expected.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>());
        for stop in 1..=expected.len() {
            let mut calls = 0;
            assert!(visit_retention_references(&owner, &key, bytes, |_| {
                calls += 1;
                if calls == stop { Err(invalid()) } else { Ok(()) }
            }).is_err());
            assert_eq!(calls, stop);
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(visit_retention_references(&owner, &key, &trailing, |_| panic!("invalid row emitted references")).is_err());
    }
    for key in [vec![], vec![0, 0], vec![PAUSE], vec![RELEASE], vec![3]] {
        assert!(visit_retention_references(&owner, &key, &[], |_| panic!("invalid key emitted references")).is_err());
    }
    let mut header = Header::from(&state);
    header.version = 2;
    assert!(visit_retention_references(&owner, HEADER, &encode(&header).unwrap(), |_| panic!("invalid version emitted references")).is_err());
}

fn hash(id: u64) -> BlockHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&id.to_be_bytes());
    BlockHash::new(bytes)
}

fn fixture() -> BrnWalletState {
    let mut state = BrnWalletState::new(Timestamp::new(1));
    state.total_burned = 7;
    state.total_staked = 11;
    state.accrual_active = false;
    state.accrual_stopped_at = Some(Timestamp::new(100));
    state.paused_intervals = vec![(Timestamp::new(2), Timestamp::new(3))].into();
    for id in 1..=1024 {
        state.record_release(hash(id), id as u128, &[hash(id), hash(2000)], Timestamp::new(5)).unwrap();
    }
    state
}

fn rows(state: &BrnWalletState) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut rows = BTreeMap::new();
    visit_delta(None, Some(state), |key, old, new| {
        assert!(old.is_none());
        assert!(rows.insert(key.to_vec(), new.unwrap()).is_none());
        Ok(())
    }).unwrap();
    rows
}

#[test]
fn wallet_record_deltas_match_complete_encoding_and_inverse_for_all_row_kinds() {
    let before = fixture();
    let original = rows(&before);
    for case in 0..8 {
        let mut after = before.clone();
        let mut releases: BTreeMap<_, _> = after.pending_releases.iter()
            .map(|(key, value)| (*key, value.clone())).collect();
        match case {
            0 => after.total_burned += 1,
            1 => after.pending_releases.get(&hash(1)).cloned().map(|mut release| {
                release.sources.push(hash(2001));
                releases.insert(hash(1), release);
            }).unwrap(),
            2 => { releases.remove(&hash(1)); },
            3 => after.record_unproven_release(hash(2002), 19, Timestamp::new(6)).unwrap(),
            4 => after.paused_intervals.push((Timestamp::new(10), Timestamp::new(20))),
            5 => after.paused_intervals.clear(),
            6 => after.paused_intervals = vec![(Timestamp::new(2), Timestamp::new(4))].into(),
            _ => (),
        }
        if matches!(case, 1 | 2) { after.pending_releases = releases.into(); }
        let mut actual = original.clone();
        let mut changes = Vec::new();
        visit_delta(Some(&before), Some(&after), |key, old, new| {
            assert_eq!(actual.get(key), old.as_ref());
            changes.push((key.to_vec(), old, new.clone()));
            if let Some(new) = new { actual.insert(key.to_vec(), new); } else { actual.remove(key); }
            Ok(())
        }).unwrap();
        assert_eq!(actual, rows(&after));
        assert_eq!(changes.len(), [1, 1, 2, 2, 2, 2, 1, 0][case]);
        if case == 0 { assert_eq!(changes[0].0, HEADER); }
        let decoded = decode(actual.iter().map(Ok)).unwrap().unwrap();
        assert_eq!(decoded, after);
        assert_eq!(bincode::serialize(&decoded).unwrap(), bincode::serialize(&after).unwrap());
        for (key, old, new) in changes.into_iter().rev() {
            assert_eq!(actual.get(&key), new.as_ref());
            if let Some(old) = old { actual.insert(key, old); } else { actual.remove(&key); }
        }
        assert_eq!(actual, original);
    }
    let mut removed = original.clone();
    visit_delta(Some(&before), None, |key, old, new| {
        assert_eq!(removed.remove(key), old);
        assert!(new.is_none());
        Ok(())
    }).unwrap();
    assert!(removed.is_empty());
    assert!(decode(removed.into_iter().map(Ok)).unwrap().is_none());
    let mut emitted = 0;
    assert!(visit_delta(None, Some(&before), |_, _, _| {
        emitted += 1;
        Err(BrnError::Other("sink stopped".into()))
    }).is_err());
    assert_eq!(emitted, 1);
}

#[test]
fn wallet_record_decoder_rejects_incomplete_mixed_and_unordered_inventories() {
    let original: Vec<_> = rows(&fixture()).into_iter().collect();
    for case in 0..12 {
        let mut corrupt = original.clone();
        match case {
            0 => { corrupt.remove(0); },
            1 => { corrupt.remove(1); },
            2 => { corrupt.pop(); },
            3 => { corrupt.insert(2, corrupt[2].clone()); },
            4 => corrupt.swap(2, 3),
            5 => corrupt[1].0 = pause_key(1),
            6 => corrupt[0].1.push(0),
            7 => corrupt[2].1.push(0),
            8 => corrupt[2].0.push(0),
            9 => corrupt.push((vec![3], vec![])),
            10 => {
                let mut header: Header = decode_value(&corrupt[0].1).unwrap();
                header.releases = u64::MAX;
                corrupt[0].1 = encode(&header).unwrap();
            }
            _ => corrupt[0].1[0] = 2,
        }
        assert!(decode(corrupt.into_iter().map(Ok)).is_err(), "case {case}");
    }
    let mut rows = original.into_iter().map(Ok).collect::<Vec<_>>();
    rows.insert(2, Err(BrnError::Other("read failure".into())));
    assert!(decode(rows).is_err());
}

#[test]
fn wallet_records_preserve_legacy_malformed_proof_fallback_and_overflow_accounting() {
    let mut state = BrnWalletState::new(Timestamp::EPOCH);
    let mut releases = BTreeMap::new();
    releases.insert(hash(1), StakeRelease {
        amount: u128::MAX, sources: vec![hash(3), hash(2), hash(2)], completed_at: Timestamp::EPOCH,
    });
    releases.insert(hash(2), StakeRelease {
        amount: 1, sources: vec![], completed_at: Timestamp::EPOCH,
    });
    state.pending_releases = releases.into();
    let decoded = decode(rows(&state).into_iter().map(Ok)).unwrap().unwrap();
    let legacy = BrnWalletState::from_persisted_bytes(&bincode::serialize(&state).unwrap()).unwrap();
    assert_eq!(decoded, legacy);
    assert_eq!(decoded.spendable_balance_checked(&crate::RateHistory::default(), Timestamp::new(10)), None);
    assert_eq!(decoded.release_sources(10), legacy.release_sources(10));
    assert_eq!(decoded.has_more_releases(&[hash(2)]), legacy.has_more_releases(&[hash(2)]));
}
