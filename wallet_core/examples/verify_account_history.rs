//! Compare paginated RPC history with a complete archived response, without signing.
use burst_rpc_messages::accounts::{
    AccountHistoryRequest, AccountHistoryResponse, AccountInfoRequest, AccountInfoResponse,
};
use burst_rpc_messages::pagination::PaginationParams;
use burst_wallet_core::NodeClient;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::HashSet, error::Error};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
struct Archive {
    result: AccountHistoryResponse,
}

async fn rpc<T: DeserializeOwned>(
    node: &NodeClient,
    action: &str,
    request: impl Serialize,
) -> Result<T> {
    Ok(serde_json::from_value(
        node.rpc_call(action, serde_json::to_value(request)?)
            .await?,
    )?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("provide an archived account_history response")?;
    let endpoint = args.next().ok_or("provide the target RPC URL")?;
    if args.next().is_some() {
        return Err("usage: verify_account_history <archive.json> <rpc-url>".into());
    }
    let expected: Archive = serde_json::from_reader(std::fs::File::open(path)?)?;
    let expected = expected.result;
    assert!(!expected.history.is_empty(), "reference history is empty");
    assert!(
        expected.cursor.is_none(),
        "reference history must be complete"
    );
    let unique: HashSet<_> = expected.history.iter().map(|entry| &entry.hash).collect();
    assert_eq!(
        unique.len(),
        expected.history.len(),
        "reference repeats entries"
    );
    let node = NodeClient::new(endpoint)?;
    let before: AccountInfoResponse = rpc(
        &node,
        "account_info",
        AccountInfoRequest {
            account: expected.account.clone(),
        },
    )
    .await?;
    assert_eq!(before.block_count, expected.history.len() as u64);
    assert_eq!(before.confirmation_height, before.block_count);
    for count in [1, 2, 25] {
        let mut cursor = None;
        let mut cursors = HashSet::new();
        let mut entries = Vec::new();
        loop {
            let page: AccountHistoryResponse = rpc(
                &node,
                "account_history",
                AccountHistoryRequest {
                    account: expected.account.clone(),
                    pagination: PaginationParams {
                        cursor,
                        count: Some(count),
                    },
                },
            )
            .await?;
            assert_eq!(page.account, expected.account);
            assert!(!page.history.is_empty() && page.history.len() <= count as usize);
            entries.extend(page.history);
            assert!(
                entries.len() <= expected.history.len(),
                "history repeats entries"
            );
            cursor = page.cursor;
            match &cursor {
                Some(next) => assert!(cursors.insert(next.clone()), "pagination did not advance"),
                None => break,
            }
        }
        assert_eq!(
            serde_json::to_value(&entries)?,
            serde_json::to_value(&expected.history)?,
            "history differs with page size {count}",
        );
    }
    let after: AccountInfoResponse = rpc(
        &node,
        "account_info",
        AccountInfoRequest {
            account: expected.account,
        },
    )
    .await?;
    // BRN accrues with time; compare persisted state rather than that live balance.
    assert_eq!(after.head, before.head);
    assert_eq!(after.block_count, before.block_count);
    assert_eq!(after.confirmation_height, before.confirmation_height);
    assert_eq!(after.payment_epoch, before.payment_epoch);
    assert_eq!(after.total_brn_burned, before.total_brn_burned);
    assert_eq!(after.total_brn_staked, before.total_brn_staked);
    assert_eq!(after.trst_balance, before.trst_balance);
    assert_eq!(after.trst_expired, before.trst_expired);
    assert_eq!(after.trst_revoked, before.trst_revoked);
    assert_eq!(after.verification_state, before.verification_state);
    assert_eq!(after.representative, before.representative);
    println!(
        "PASS complete paginated account history matches archive; balances and authority unchanged"
    );
    Ok(())
}
