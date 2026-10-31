//! Browser-test adapter, not an application wallet or production key source.
#![cfg(target_arch = "wasm32")]
use burst_wallet_core::{NodeClient, Wallet};
use std::time::Duration;
use wasm_bindgen::prelude::*;

fn error(value: impl ToString) -> JsValue {
    JsValue::from_str(&value.to_string())
}

#[wasm_bindgen]
pub fn crypto_check() -> Result<String, JsValue> {
    let first = Wallet::create().map_err(error)?;
    let second = Wallet::create().map_err(error)?;
    let signature = first.sign(b"browser-test");
    let root = burst_types::BlockHash::new([2; 32]);
    let work = burst_work::WorkGenerator
        .generate(&root, 1000)
        .map_err(error)?;
    Ok(serde_json::json!({
        "distinct":first.address != second.address,
        "signature_valid":burst_crypto::verify_signature(b"browser-test", &signature, &first.primary_keys.public),
        "work_valid":burst_work::validate_work(&root, work.0, 1000),
        "now":burst_types::Timestamp::now().as_secs(),
    }).to_string())
}

#[wasm_bindgen]
pub fn test_identity() -> String {
    let keys = burst_crypto::keypair_from_seed(&[73; 32]);
    serde_json::json!({"address":burst_crypto::derive_address(&keys.public).as_str(),
        "public_key":hex::encode(keys.public.0)})
    .to_string()
}

#[wasm_bindgen]
pub async fn pay(url: &str) -> Result<String, JsValue> {
    let keys = burst_crypto::keypair_from_seed(&[73; 32]);
    let recipient =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[74; 32]).public);
    NodeClient::new(url)
        .map_err(error)?
        .pay_signed(&keys, &recipient, 7)
        .await
        .map(|result| result.hash)
        .map_err(error)
}

#[wasm_bindgen]
pub async fn rpc(url: &str, action: &str) -> Result<String, JsValue> {
    NodeClient::new(url)
        .map_err(error)?
        .rpc_call(action, serde_json::json!({}))
        .await
        .map(|value| value.to_string())
        .map_err(error)
}

#[wasm_bindgen]
pub async fn bounded(url: &str, milliseconds: u32, limit: u32) -> Result<u32, JsValue> {
    burst_web_runtime::fetch_bytes(
        url,
        None,
        Duration::from_millis(milliseconds.into()),
        limit as usize,
    )
    .await
    .map(|bytes| bytes.len() as u32)
    .map_err(error)
}

#[wasm_bindgen]
pub async fn group(url: &str) -> Result<bool, JsValue> {
    burst_groups::GroupClient::with_timeout(Duration::from_millis(500))
        .map_err(error)?
        .verify_member(url, "browser-wallet")
        .await
        .map(|member| member.valid)
        .map_err(error)
}

#[wasm_bindgen]
pub async fn group_graph(base: &str, max_hops: u8) -> Result<String, JsValue> {
    let mut registry = burst_groups::GroupRegistry::new();
    for id in ["root", "member"] {
        registry
            .register(burst_groups::GroupInfo {
                id: id.into(),
                name: id.into(),
                description: String::new(),
                member_count: 0,
                endpoint_url: format!("{base}/graph-{id}"),
            })
            .map_err(error)?;
    }
    let path = burst_groups::GroupClient::with_timeout(Duration::from_millis(500))
        .map_err(error)?
        .verify_member_through_groups(&registry, "root", "browser-wallet", max_hops)
        .await
        .map_err(error)?;
    serde_json::to_string(&path.map(|path| path.groups)).map_err(error)
}

#[wasm_bindgen]
pub async fn wait_for_confirmation(url: &str) -> Result<(), JsValue> {
    NodeClient::new(url)
        .map_err(error)?
        .wait_confirmed(&"01".repeat(32), Duration::from_millis(50))
        .await
        .map_err(error)
}
