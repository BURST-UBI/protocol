use burst_groups::{GroupClient, GroupRegistry};
use serde_json::json;
use std::{fs, process::Command, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn listing() -> serde_json::Value {
    json!({"version":1,"groups":[{
        "id":"helsinki", "name":"Helsinki yhteisö", "description":"In-person membership",
        "endpoint_url":"https://unqueried.invalid/community", "member_count":42
    }]})
}

#[tokio::test]
async fn exported_directory_is_discoverable_without_contacting_membership_endpoints() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("draft.json");
    let output = directory.path().join("directory.json");
    fs::write(&input, serde_json::to_vec_pretty(&listing()).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_burst-group-directory"))
        .args([&input, &output])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let published = fs::read(&output).unwrap();
    assert_eq!(
        published,
        GroupRegistry::from_directory(&published)
            .unwrap()
            .to_directory()
            .unwrap()
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/directory.json", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192);
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        assert!(request.starts_with(b"GET /directory.json HTTP/1.1\r\n"));
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", published.len()).as_bytes()).await.unwrap();
        socket.write_all(&published).await.unwrap();
    });
    let registry = GroupClient::with_timeout(Duration::from_secs(5))
        .unwrap()
        .fetch_directory(&url)
        .await
        .unwrap();
    assert_eq!(registry.list().len(), 1);
    let group = registry.get("helsinki").unwrap();
    assert_eq!(group.name, "Helsinki yhteisö");
    assert_eq!(group.endpoint_url, "https://unqueried.invalid/community");
    assert_eq!(group.member_count, 42);
    server.await.unwrap();

    let before = fs::read(&output).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_burst-group-directory"))
        .args([&input, &output])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        fs::read(&output).unwrap(),
        before,
        "existing publication was overwritten"
    );
}

#[test]
fn invalid_or_oversized_drafts_create_no_publication() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("draft.json");
    let output = directory.path().join("directory.json");
    let mut duplicate = listing();
    let entry = duplicate["groups"][0].clone();
    duplicate["groups"].as_array_mut().unwrap().push(entry);
    let mut credentials = listing();
    credentials["groups"][0]["endpoint_url"] = json!("https://user:secret@example.test");
    for bytes in [
        serde_json::to_vec(&duplicate).unwrap(),
        serde_json::to_vec(&credentials).unwrap(),
        b"{\"version\":2,\"groups\":[]}".to_vec(),
        vec![b' '; burst_groups::registry::MAX_DIRECTORY_BYTES + 1],
    ] {
        fs::write(&input, bytes).unwrap();
        assert!(!Command::new(env!("CARGO_BIN_EXE_burst-group-directory"))
            .args([&input, &output])
            .output()
            .unwrap()
            .status
            .success());
        assert!(!output.exists());
    }
}
