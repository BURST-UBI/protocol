use super::*;

pub(super) fn full(state: &ConfirmedReplay) -> u64 {
    state
        .accounts
        .values()
        .filter(|account| account.state.is_verified())
        .count() as u64
}

pub(super) fn advance(
    count: u64,
    mutations: &crate::confirmed_replay::mutations::Mutations,
) -> Result<u64, ReplayError> {
    let mut removed = 0u64;
    let mut added = 0u64;
    for row in mutations
        .changes()
        .filter(|row| row.table == UndoTable::Accounts)
    {
        let verified = |bytes: &Option<Vec<u8>>| -> Result<u64, ReplayError> {
            let Some(bytes) = bytes else {
                return Ok(0);
            };
            let account: burst_store::account::AccountInfo =
                bincode::deserialize(bytes).map_err(replay_error)?;
            if account.address.as_str().as_bytes() != row.key {
                return Err(crate::confirmed_replay::invalid(
                    "verified count account key mismatch",
                ));
            }
            Ok(u64::from(account.state.is_verified()))
        };
        removed = removed
            .checked_add(verified(&row.before)?)
            .ok_or_else(|| crate::confirmed_replay::invalid("verified count removal overflow"))?;
        added = added
            .checked_add(verified(&row.after)?)
            .ok_or_else(|| crate::confirmed_replay::invalid("verified count addition overflow"))?;
    }
    count
        .checked_sub(removed)
        .and_then(|value| value.checked_add(added))
        .ok_or_else(|| crate::confirmed_replay::invalid("verified count transition overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::AccountInfo;
    use burst_types::{WalletAddress, WalletState};

    #[test]
    fn recorded_verified_count_handles_creation_reversal_and_invalid_images() {
        let mut a = AccountInfo::unopened(WalletAddress::new("brst_count_a"));
        a.state = WalletState::Verified;
        let mut b = AccountInfo::unopened(WalletAddress::new("brst_count_b"));
        let encode = |account: &AccountInfo| Some(bincode::serialize(account).unwrap());
        let mut changes = crate::confirmed_replay::mutations::Mutations::default();
        changes
            .record(
                UndoTable::Accounts,
                a.address.as_str().as_bytes().to_vec(),
                None,
                encode(&a),
            )
            .unwrap();
        assert_eq!(advance(0, &changes).unwrap(), 1);
        let verified_a = encode(&a);
        a.state = WalletState::Challenged;
        changes.record(UndoTable::Accounts, a.address.as_str().as_bytes().to_vec(), verified_a, encode(&a)).unwrap();
        assert_eq!(advance(0, &changes).unwrap(), 1);
        assert!(advance(u64::MAX, &changes).is_err());
        let mut swap = crate::confirmed_replay::mutations::Mutations::default();
        let old_a = encode(&a);
        a.state = WalletState::Revoked;
        swap.record(
            UndoTable::Accounts,
            a.address.as_str().as_bytes().to_vec(),
            old_a,
            encode(&a),
        )
        .unwrap();
        let old_b = encode(&b);
        b.state = WalletState::Verified;
        swap.record(
            UndoTable::Accounts,
            b.address.as_str().as_bytes().to_vec(),
            old_b,
            encode(&b),
        )
        .unwrap();
        assert_eq!(advance(1, &swap).unwrap(), 1);
        assert_eq!(advance(u64::MAX, &swap).unwrap(), u64::MAX);
        assert!(advance(0, &swap).is_err());
        let mut invalid = crate::confirmed_replay::mutations::Mutations::default();
        invalid
            .record(UndoTable::Accounts, b"wrong".to_vec(), None, encode(&b))
            .unwrap();
        assert!(advance(0, &invalid).is_err());
        changes
            .record(
                UndoTable::Accounts,
                b"malformed".to_vec(),
                None,
                Some(vec![0]),
            )
            .unwrap();
        assert!(advance(0, &changes).is_err());
    }
}
