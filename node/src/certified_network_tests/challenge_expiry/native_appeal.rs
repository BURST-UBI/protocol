use super::*;
use burst_types::appeal::AppealReview;
use burst_wallet_core::wallet::ProcessResult;

pub(in crate::certified_network_tests) async fn offline_panel(
    client: &NodeClient,
    vote: &burst_transactions::verification_vote::VerificationVoteTx,
) -> StateBlock {
    let directory = tempfile::tempdir().unwrap();
    let result_path = directory.path().join("panel-result.json");
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wallet/tests/native-session.mjs");
    let output = tokio::process::Command::new("node")
        .arg(runner)
        .arg("--offline-panel-fixture")
        .env("NATIVE_FIXTURE_RPC_URL", client.node_url())
        .env("NATIVE_OFFLINE_ACTION", "panel")
        .env("NATIVE_PANEL_TARGET", vote.target_wallet.as_str())
        .env("NATIVE_PANEL_CONTEXT", vote.selection_context.to_string())
        .env("NATIVE_PANEL_VOTE", vote.vote.to_string())
        .env("NATIVE_PANEL_STAKE", vote.stake_amount.to_string())
        .env("NATIVE_APPEAL_TIME", vote.timestamp.as_secs().to_string())
        .env("NATIVE_RESULT_PATH", &result_path)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "installed offline panel journey failed"
    );
    serde_json::from_slice(&std::fs::read(result_path).unwrap()).unwrap()
}

pub(super) async fn submit(
    client: &NodeClient,
    review: &AppealReview,
    now: Timestamp,
) -> ProcessResult {
    let library = std::env::var("NATIVE_FAKETIME_LIBRARY")
        .expect("set NATIVE_FAKETIME_LIBRARY to a built libfaketimeMT.so.1");
    assert!(std::path::Path::new(&library).is_absolute());
    assert!(std::path::Path::new(&library).is_file());
    let directory = tempfile::tempdir().unwrap();
    let result_path = directory.path().join("appeal-result.json");
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wallet/tests/native-session.mjs");
    // Only the disposable desktop process sees the historical wall clock.
    // Its normal Timestamp::now(), signing and RPC submission remain unchanged.
    let output = tokio::process::Command::new("node")
        .arg(runner)
        .arg("--appeal-fixture")
        .env("NATIVE_FIXTURE_RPC_URL", client.node_url())
        .env("NATIVE_APPEAL_TARGET", &review.account)
        .env(
            "NATIVE_APPEAL_REASON",
            format!("{:?}", review.outcome.as_ref().unwrap().reason),
        )
        .env(
            "NATIVE_APPEAL_LEGITIMATE",
            review.outcome.as_ref().unwrap().legitimate.to_string(),
        )
        .env("NATIVE_APPEAL_TIME", now.as_secs().to_string())
        .env("NATIVE_RESULT_PATH", &result_path)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "native desktop appeal failed");
    serde_json::from_slice(&std::fs::read(result_path).unwrap()).unwrap()
}
