use super::*;
use burst_trst::TrstEngine;
use burst_types::{Timestamp, TxHash};

fn fixture() -> (TrstEngine, TrstEngine, BlockUndo) {
    let mut before = TrstEngine::with_expiry(100);
    for id in 1..=3 {
        let owner = WalletAddress::new(format!("brst_{id}"));
        let token = before
            .mint(
                TxHash::new([id; 32]),
                owner.clone(),
                10,
                owner,
                Timestamp::new(100),
            )
            .unwrap();
        before.track_token(token);
    }
    let mut after = before.clone();
    let owner = WalletAddress::new("brst_1");
    let portfolio = after.wallets.get_mut(&owner).unwrap();
    portfolio.tokens[0].amount = 7;
    portfolio.cached_transferable = 7;
    after.wallets.remove(&WalletAddress::new("brst_2"));
    let owner = WalletAddress::new("brst_new");
    let token = after
        .mint(
            TxHash::new([9; 32]),
            owner.clone(),
            4,
            owner,
            Timestamp::new(101),
        )
        .unwrap();
    after.track_token(token);
    let mut changes = Vec::new();
    after
        .visit_wallet_record_delta(&before, |old, new| {
            let old = old.map(|row| row.storage_entry()).transpose()?;
            let new = new.map(|row| row.storage_entry()).transpose()?;
            let key = new.as_ref().or(old.as_ref()).unwrap().0.as_bytes().to_vec();
            changes.push(UndoChange {
                table: UndoTable::Meta,
                key,
                before: old.map(|row| row.1),
                after: new.map(|row| row.1),
            });
            Ok::<_, String>(())
        })
        .unwrap();
    changes.push(UndoChange {
        table: UndoTable::Pending,
        key: b"unrelated".to_vec(),
        before: Some(vec![3]),
        after: Some(vec![4]),
    });
    changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
    let journal = BlockUndo {
        block: BlockHash::new([33; 32]),
        account: WalletAddress::new("brst_1"),
        height: 2,
        changes,
    };
    (before, after, journal)
}

fn stable_entries(engine: &TrstEngine) -> BTreeMap<String, Vec<u8>> {
    let mut rows = BTreeMap::new();
    for (owner, portfolio) in &engine.wallets {
        for (_, row) in holding_rows::delta(owner, None, Some(portfolio)).unwrap() {
            let (key, value) = row.unwrap().storage_entry().unwrap();
            assert!(rows.insert(key, value).is_none());
        }
    }
    rows
}

#[test]
fn conversion_preserves_journal_identity_other_changes_and_exact_portfolios() {
    let (before, after, original) = fixture();
    let converted = convert(
        original.clone(),
        |owner| Ok(after.wallets.get(owner).map(|p| &**p)),
        1 << 20,
    )
    .unwrap();
    assert_eq!(converted.journal.block, original.block);
    assert_eq!(converted.journal.account, original.account);
    assert_eq!(converted.journal.height, original.height);
    assert_eq!(converted.before_portfolios.len(), 3);
    for (owner, portfolio) in &converted.before_portfolios {
        assert_eq!(portfolio.as_ref(), before.wallets.get(owner).map(|p| &**p));
    }
    let positional = |change: &&UndoChange| {
        change.table == UndoTable::Meta
            && (change.key.starts_with(b"trst_state/v1/0/")
                || change.key.starts_with(b"trst_state/v1/1/"))
    };
    let preserved: Vec<_> = original
        .changes
        .iter()
        .filter(|change| !positional(change))
        .cloned()
        .collect();
    assert_eq!(
        converted
            .journal
            .changes
            .iter()
            .filter(|change| !change.key.starts_with(holding_rows::PREFIX.as_bytes()))
            .cloned()
            .collect::<Vec<_>>(),
        preserved
    );
    let mut rows = stable_entries(&after);
    for change in &converted.journal.changes {
        if !change.key.starts_with(holding_rows::PREFIX.as_bytes()) {
            continue;
        }
        let key = std::str::from_utf8(&change.key).unwrap().to_owned();
        assert_eq!(rows.get(&key), change.after.as_ref());
        if let Some(value) = &change.before {
            rows.insert(key, value.clone());
        } else {
            rows.remove(&key);
        }
    }
    assert_eq!(rows, stable_entries(&before));
}

#[test]
fn conversion_rejects_missing_stale_mixed_and_over_budget_contexts() {
    let (before, after, original) = fixture();
    assert!(convert(original.clone(), |_| Ok(None), 1 << 20).is_err());
    assert!(convert(
        original.clone(),
        |owner| Ok(before.wallets.get(owner).map(|p| &**p)),
        1 << 20
    )
    .is_err());
    assert!(convert(
        original.clone(),
        |owner| Ok(after.wallets.get(owner).map(|p| &**p)),
        1
    )
    .is_err());
    assert!(convert(original.clone(), |_| Ok(None), MAX_BYTES + 1).is_err());
    for key in [
        b"trst_engine_wallets".to_vec(),
        b"trst_holdings/v2/invalid".to_vec(),
    ] {
        let mut mixed = original.clone();
        mixed.changes.push(UndoChange {
            table: UndoTable::Meta,
            key,
            before: None,
            after: Some(vec![1]),
        });
        mixed
            .changes
            .sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        assert!(convert(
            mixed,
            |owner| Ok(after.wallets.get(owner).map(|p| &**p)),
            1 << 20
        )
        .is_err());
    }
    let mut duplicate = original.clone();
    duplicate.changes.insert(0, duplicate.changes[0].clone());
    assert!(convert(
        duplicate,
        |owner| Ok(after.wallets.get(owner).map(|p| &**p)),
        1 << 20
    )
    .is_err());
    assert_eq!(stable_entries(&after), stable_entries(&fixture().1));
}

#[test]
fn conversion_validates_preserved_economic_rows_without_touching_other_tables() {
    let (_, after, original) = fixture();
    for key in [
        b"trst_state/v1/0".to_vec(),
        b"trst_state/v1/9/unknown".to_vec(),
        b"trst_state/v1/2/ff".to_vec(),
        b"trst_state/v1/\xff".to_vec(),
    ] {
        let mut malformed = original.clone();
        malformed.changes.push(UndoChange {
            table: UndoTable::Meta,
            key,
            before: None,
            after: Some(vec![1]),
        });
        malformed
            .changes
            .sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        let mut requested = 0;
        assert!(convert(
            malformed,
            |owner| {
                requested += 1;
                Ok(after.wallets.get(owner).map(|p| &**p))
            },
            1 << 20
        )
        .is_err());
        assert_eq!(
            requested, 0,
            "invalid row must fail before requesting portfolio cuts"
        );
    }
    let mut metadata_only = original;
    metadata_only.changes.retain(|change| {
        change.table != UndoTable::Meta
            || !(change.key.starts_with(b"trst_state/v1/0/")
                || change.key.starts_with(b"trst_state/v1/1/"))
    });
    let converted = convert(
        metadata_only.clone(),
        |_| panic!("metadata requires no portfolio context"),
        0,
    )
    .unwrap();
    assert_eq!(converted.journal, metadata_only);
    assert!(converted.before_portfolios.is_empty());
}
