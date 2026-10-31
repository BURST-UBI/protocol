//! Read-only prerequisites for the concurrent RPC load branch. Public Dev keys
//! only; retained-state readiness is not fresh onboarding or release evidence.
use burst_types::{BlockHash, Timestamp};
use burst_wallet_core::{
    signed_client::parse_hash, transaction_builder::AccountState, NodeClient, Wallet,
};
use serde_json::{json, Value};
use std::{collections::HashSet, error::Error, fs::OpenOptions, io::Write};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_BURN: u128 = 1024 * 1025 / 2;

struct Options {
    report: String,
    first_seed: u8,
    accounts: usize,
    urls: Vec<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        if !(5..=11).contains(&args.len()) {
            return Err(
                "usage: --load-preflight REPORT FIRST_PUBLIC_SEED ACCOUNTS RPC_URL RPC_URL [...]"
                    .into(),
            );
        }
        let first_seed = args[1].parse::<u8>()?;
        let accounts = args[2].parse::<usize>()?;
        if !(2..=32).contains(&accounts)
            || first_seed == 0
            || usize::from(first_seed) + accounts > 256
        {
            return Err(
                "load requires 2..32 public fixture seeds in 1..255; genesis is excluded".into(),
            );
        }
        let urls = args[3..].to_vec();
        if urls.iter().collect::<HashSet<_>>().len() != urls.len() {
            return Err("duplicate RPC endpoints do not establish independent peers".into());
        }
        for url in &urls {
            let parsed = reqwest::Url::parse(url)?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(
                    "load endpoints must be HTTP(S) without credentials, query or fragment".into(),
                );
            }
        }
        Ok(Self {
            report: args[0].clone(),
            first_seed,
            accounts,
            urls,
        })
    }
}

fn parameter(params: &Value, name: &str) -> Result<u64> {
    let entries = params["governable_parameters"]
        .as_array()
        .ok_or("missing parameter inventory")?;
    let values: Vec<_> = entries
        .iter()
        .filter(|entry| entry["name"] == name)
        .collect();
    if values.len() != 1 {
        return Err(format!("missing or duplicate parameter {name}").into());
    }
    Ok(values[0]["current"]
        .as_str()
        .ok_or("non-string parameter value")?
        .parse()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Authority {
    params: BlockHash,
    epoch: BlockHash,
    difficulty: u64,
}

fn authority(info: &Value, signing: &Value, params: &Value) -> Result<Authority> {
    if info["network"] != "dev" {
        return Err("load is restricted to disposable Dev networks".into());
    }
    if !matches!(
        signing["online_signing"].as_str(),
        Some("local" | "peer_matches")
    ) {
        return Err("node is not ready for synchronized signing".into());
    }
    if signing["block_version"].as_u64() != Some(burst_ledger::CURRENT_BLOCK_VERSION as u64) {
        return Err("incompatible block version".into());
    }
    let params_hash = parse_hash(
        signing["params_hash"]
            .as_str()
            .ok_or("missing params hash")?,
    )?;
    if params["params_hash"].as_str() != Some(params_hash.to_string().as_str()) {
        return Err("parameter views changed during preflight".into());
    }
    let epoch = parse_hash(
        signing["payment_epoch"]
            .as_str()
            .ok_or("missing payment epoch")?,
    )?;
    let difficulty = u64::from_str_radix(
        signing["min_work_difficulty"]
            .as_str()
            .ok_or("missing work difficulty")?,
        16,
    )?;
    if epoch.is_zero() || difficulty == 0 || difficulty != parameter(params, "min_work_difficulty")?
    {
        return Err("load needs initialized authority and matching nonzero work difficulty".into());
    }
    Ok(Authority {
        params: params_hash,
        epoch,
        difficulty,
    })
}

fn account_cut(
    account: &Value,
    state: &AccountState,
    params: &Value,
    authority: &Authority,
    address: &str,
    now: u64,
) -> Result<Value> {
    if account["address"] != address
        || account["verification_state"] != "verified"
        || state.head.is_zero()
        || account["head"] != state.head.to_string()
        || account["block_count"].as_u64() != Some(state.block_count)
        || account["confirmation_height"].as_u64() != Some(state.block_count)
        || state.payment_epoch != authority.epoch
        || !state.signing_grant.is_zero()
        || !state.release_sources.is_empty()
    {
        return Err("account must have a verified, confirmed, uncontested signing baseline without pending releases".into());
    }
    let verified_at = account["verified_at"]
        .as_u64()
        .ok_or("missing verification age")?;
    let age = now
        .checked_sub(verified_at)
        .ok_or("verification timestamp is in the future")?;
    let minimum_age = parameter(params, "min_verification_age_secs")?
        .max(parameter(params, "new_wallet_rate_limit_duration_secs")?)
        .max(parameter(params, "new_wallet_limit_duration_secs")?);
    if age < minimum_age {
        return Err("load fixture accounts must mature under actual wallet limits".into());
    }
    let available: u128 = account["brn_balance"]
        .as_str()
        .ok_or("missing BRN balance")?
        .parse()?;
    if available < MAX_BURN {
        return Err("insufficient BRN for bounded load inventory".into());
    }
    let trst: u128 = account["trst_balance"]
        .as_str()
        .ok_or("missing TRST balance")?
        .parse()?;
    if trst != state.trst_balance || account["representative"] != state.representative.as_str() {
        return Err("account/signing views disagree".into());
    }
    let burned: u128 = account["total_brn_burned"]
        .as_str()
        .ok_or("missing burn total")?
        .parse()?;
    let staked: u128 = account["total_brn_staked"]
        .as_str()
        .ok_or("missing stake total")?
        .parse()?;
    if burned.checked_add(staked) != Some(state.brn_spent) {
        return Err("BRN spending odometer differs from accounting totals".into());
    }
    Ok(
        json!({"address":address,"head":state.head,"block_count":state.block_count,
        "trst_balance":trst.to_string(),"total_brn_burned":burned.to_string(),
        "total_brn_staked":staked.to_string(),"representative":state.representative,
        "payment_epoch":state.payment_epoch,"signing_grant":state.signing_grant,
        "verified_at":verified_at}),
    )
}

fn record(file: &mut std::fs::File, value: Value) -> Result<()> {
    serde_json::to_writer(&mut *file, &value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub(super) async fn preflight(args: &[String]) -> Result<()> {
    let options = Options::parse(args)?;
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&options.report)?;
    record(
        &mut report,
        json!({"stage":"start","mode":"read-only-load-preflight",
        "urls":options.urls,"first_public_seed":options.first_seed,"accounts":options.accounts,
        "scope":"RPC readiness only; URLs do not prove distinct hosts or artifact identities"}),
    )?;
    let result = inspect(&options, &mut report).await.map(|_| ());
    record(
        &mut report,
        json!({"stage":"terminal","outcome":if result.is_ok(){"pass"}else{"fail"},
        "error":result.as_ref().err().map(ToString::to_string),
        "scope":"no load submitted; no throughput, latency, hardware or release qualification"}),
    )?;
    result
}

struct Ready {
    authority: Authority,
    parameters: Value,
    baselines: Vec<Value>,
}

async fn inspect(options: &Options, report: &mut std::fs::File) -> Result<Ready> {
    let mut expected_authority = None;
    let mut baselines = Vec::new();
    let mut parameters = None;
    for (peer, url) in options.urls.iter().enumerate() {
        let node = NodeClient::new(url)?;
        let info = node.rpc_call("node_info", json!({})).await?;
        record(
            report,
            json!({"stage":"peer-info","peer":peer,"node_info":info}),
        )?;
        // Stop before querying account state on a non-disposable endpoint.
        if info["network"] != "dev" {
            return Err("load is restricted to disposable Dev networks".into());
        }
        let signing = node.rpc_call("params_hash", json!({})).await?;
        let params = node.rpc_call("protocol_params", json!({})).await?;
        record(
            report,
            json!({"stage":"peer","peer":peer,"node_info":info,
            "signing":signing,"parameters":params}),
        )?;
        let current = authority(&info, &signing, &params)?;
        if peer == 0 {
            parameters = Some(params.clone());
        }
        if let Some(expected) = &expected_authority {
            if expected != &current {
                return Err("peers disagree on parameters or payment epoch".into());
            }
        } else {
            expected_authority = Some(Authority {
                params: current.params,
                epoch: current.epoch,
                difficulty: current.difficulty,
            });
        }
        let online = node.rpc_call("representatives_online", json!({})).await?;
        for index in 0..options.accounts {
            let wallet = Wallet::from_seed(&[options.first_seed + index as u8; 32]);
            let address = wallet.address.as_str();
            let account = node
                .rpc_call("account_info", json!({"account":address}))
                .await?;
            let state = node.signing_state(&wallet.address).await?;
            let cut = account_cut(
                &account,
                &state,
                &params,
                &current,
                address,
                Timestamp::now().as_secs(),
            )?;
            let representatives = online["representatives"]
                .as_array()
                .ok_or("missing online representatives")?;
            if !representatives.iter().any(|rep| {
                rep["account"] == state.representative.as_str()
                    && rep["weight"]
                        .as_str()
                        .and_then(|weight| weight.parse::<u128>().ok())
                        .is_some_and(|weight| weight > 0)
            }) {
                return Err(
                    "account representative is not observed online with positive weight".into(),
                );
            }
            record(
                report,
                json!({"stage":"account","peer":peer,"public_seed":options.first_seed + index as u8,"baseline":cut}),
            )?;
            if peer == 0 {
                baselines.push(cut);
            } else if baselines[index] != cut {
                return Err("peer account baselines differ".into());
            }
        }
    }
    Ok(Ready {
        authority: expected_authority.ok_or("no peers")?,
        parameters: parameters.ok_or("no peer parameters")?,
        baselines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args() -> Vec<String> {
        [
            "report.jsonl",
            "1",
            "2",
            "http://127.0.0.1:1234",
            "http://127.0.0.1:1235",
        ]
        .map(str::to_owned)
        .to_vec()
    }
    fn params() -> (Value, Value, Value) {
        let hash = BlockHash::new([1; 32]).to_string();
        (
            json!({"network":"dev"}),
            json!({"online_signing":"local","block_version":burst_ledger::CURRENT_BLOCK_VERSION,
            "payment_epoch":hash,"params_hash":hash,"min_work_difficulty":"10"}),
            json!({"params_hash":hash,"governable_parameters":[{"name":"min_work_difficulty","current":"16"}]}),
        )
    }
    #[test]
    fn rejects_unsafe_or_ambiguous_load_options() {
        assert!(Options::parse(&args()).is_ok());
        for (field, value) in [
            (1, "0"),
            (1, "255"),
            (2, "1"),
            (2, "33"),
            (3, "file:///tmp/node"),
            (3, "http://user:secret@localhost"),
            (3, "http://localhost/?token=secret"),
            (3, "http://127.0.0.1:1235"),
        ] {
            let mut input = args();
            input[field] = value.into();
            assert!(Options::parse(&input).is_err(), "{field}");
        }
    }
    #[test]
    fn work_and_authority_must_match_without_disabling_work() {
        let (info, mut signing, mut parameters) = params();
        assert!(authority(&info, &signing, &parameters).is_ok());
        signing["min_work_difficulty"] = json!("0");
        assert!(authority(&info, &signing, &parameters).is_err());
        signing["min_work_difficulty"] = json!("10");
        signing["online_signing"] = json!("catching_up");
        assert!(authority(&info, &signing, &parameters).is_err());
        signing["online_signing"] = json!("local");
        parameters["governable_parameters"][0]["current"] = json!("17");
        assert!(authority(&info, &signing, &parameters).is_err());
        parameters["governable_parameters"][0]["current"] = json!("16");
        signing["payment_epoch"] = json!(BlockHash::ZERO.to_string());
        assert!(authority(&info, &signing, &parameters).is_err());
        assert!(authority(&json!({"network":"live"}), &signing, &parameters).is_err());
    }
    #[test]
    fn parameter_inventory_rejects_missing_duplicate_and_lossy_values() {
        let (_, _, mut params) = params();
        assert_eq!(parameter(&params, "min_work_difficulty").unwrap(), 16);
        assert!(parameter(&params, "missing").is_err());
        let entry = params["governable_parameters"][0].clone();
        params["governable_parameters"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        assert!(parameter(&params, "min_work_difficulty").is_err());
        params["governable_parameters"] = json!([{"name":"min_work_difficulty","current":16}]);
        assert!(parameter(&params, "min_work_difficulty").is_err());
    }
    #[test]
    fn account_baseline_requires_maturity_exact_totals_and_uncontested_authority() {
        let (info, signing, mut parameters) = params();
        for name in [
            "min_verification_age_secs",
            "new_wallet_rate_limit_duration_secs",
            "new_wallet_limit_duration_secs",
        ] {
            parameters["governable_parameters"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":name,"current":"100"}));
        }
        let authority = authority(&info, &signing, &parameters).unwrap();
        let address = Wallet::from_seed(&[1; 32]).address;
        let state = AccountState {
            signing_grant: BlockHash::ZERO,
            payment_epoch: authority.epoch,
            release_sources: vec![],
            head: BlockHash::new([2; 32]),
            block_count: 2,
            representative: address.clone(),
            brn_spent: 7,
            trst_balance: 3,
        };
        let account = json!({"address":address,"verification_state":"verified","head":state.head.to_string(),
            "block_count":2,"confirmation_height":2,"verified_at":100,
            "brn_balance":MAX_BURN.to_string(),"trst_balance":"3",
            "total_brn_burned":"5","total_brn_staked":"2","representative":address});
        account_cut(
            &account,
            &state,
            &parameters,
            &authority,
            address.as_str(),
            200,
        )
        .unwrap();
        for (field, value) in [
            ("confirmation_height", json!(1)),
            ("verified_at", json!(101)),
            ("verified_at", json!(201)),
            ("total_brn_burned", json!("6")),
            ("brn_balance", json!("1")),
            ("trst_balance", json!("4")),
            ("verification_state", json!("unverified")),
        ] {
            let mut changed = account.clone();
            changed[field] = value;
            assert!(
                account_cut(
                    &changed,
                    &state,
                    &parameters,
                    &authority,
                    address.as_str(),
                    200
                )
                .is_err(),
                "{field}"
            );
        }
        let mut changed = state.clone();
        changed.release_sources.push(state.head);
        assert!(account_cut(
            &account,
            &changed,
            &parameters,
            &authority,
            address.as_str(),
            200
        )
        .is_err());
        changed = state.clone();
        changed.signing_grant = state.head;
        assert!(account_cut(
            &account,
            &changed,
            &parameters,
            &authority,
            address.as_str(),
            200
        )
        .is_err());
    }

    #[tokio::test]
    async fn live_endpoint_rejection_records_failure_without_account_or_write_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let count = stream.read(&mut bytes).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
                assert!(request.len() < 8192);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&request[..end]).unwrap();
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        let body: Value =
                            serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                        assert_eq!(body, json!({"action":"node_info"}));
                        break;
                    }
                }
            }
            let body = r#"{"result":{"network":"live"}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.jsonl");
        let mut options = args();
        options[0] = path.to_str().unwrap().into();
        options[3] = url;
        let result = preflight(&options).await;
        assert!(result.is_err());
        server.await.unwrap();
        let report = std::fs::read_to_string(&path).unwrap();
        let terminal: Value = serde_json::from_str(report.lines().last().unwrap()).unwrap();
        assert_eq!(terminal["outcome"], "fail");
        assert!(preflight(&options).await.is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            report,
            "never overwrite prior evidence"
        );
    }
}

#[path = "load/run.rs"]
mod run;
pub(super) use run::execute;
