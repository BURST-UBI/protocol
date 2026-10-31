use super::*;
use burst_wallet_core::wallet::ProcessResponse;

pub(super) async fn claim(client: &NodeClient, source: &str, now: Timestamp) -> ProcessResponse {
    let library = std::env::var("NATIVE_FAKETIME_LIBRARY")
        .expect("set NATIVE_FAKETIME_LIBRARY to a built libfaketimeMT.so.1");
    assert!(std::path::Path::new(&library).is_absolute());
    assert!(std::path::Path::new(&library).is_file());
    let directory = tempfile::tempdir().unwrap();
    let result_path = directory.path().join("return-result.json");
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wallet/tests/native-session.mjs");
    let output = tokio::process::Command::new("node")
        .arg(runner)
        .arg("--expiry-return-fixture")
        .env("NATIVE_FIXTURE_RPC_URL", client.node_url())
        .env("NATIVE_RETURN_SOURCE", source)
        .env("NATIVE_RETURN_TIME", now.as_secs().to_string())
        .env("NATIVE_RESULT_PATH", &result_path)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "native desktop return claim failed"
    );
    serde_json::from_slice(&std::fs::read(result_path).unwrap()).unwrap()
}
