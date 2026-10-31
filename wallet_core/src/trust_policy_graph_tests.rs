use super::*;
use burst_groups::GroupInfo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn transitive_payment_checks_every_creator_and_never_falls_back_on_unknown_groups() {
    for scenario in ["member", "nonmember", "unknown", "outage"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut registry = GroupRegistry::new();
        for id in ["root", "leaf"] {
            registry
                .register(GroupInfo {
                    id: id.into(),
                    name: id.into(),
                    description: String::new(),
                    member_count: 0,
                    endpoint_url: format!("{base}/{id}"),
                })
                .unwrap();
        }
        let payment = tests::payment();
        let mut creators: Vec<_> = payment
            .provenance
            .origins
            .iter()
            .map(|o| o.originator.clone())
            .collect();
        creators.sort();
        let denied = creators[1].clone();
        let denied_for_assert = denied.clone();
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests = observed.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    assert!(bytes.len() < 8192);
                    bytes.push(socket.read_u8().await.unwrap());
                }
                let request = String::from_utf8(bytes).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                requests.lock().unwrap().push(path.to_owned());
                let (status, body) = if path == "/root/trust" {
                    (200, serde_json::json!({"version":1,"groups":[if scenario=="unknown" {"unknown"} else {"leaf"}]}).to_string())
                } else if path.starts_with("/root/verify/") {
                    (200, r#"{"valid":false,"score":0}"#.into())
                } else if path.starts_with("/leaf/verify/") {
                    if scenario == "outage" {
                        (503, "unavailable".into())
                    } else {
                        (200, serde_json::json!({"valid":scenario!="nonmember" || !path.ends_with(&denied),"score":0.5}).to_string())
                    }
                } else {
                    (404, "unexpected request".into())
                };
                socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let ids = vec!["root".into()];
        let result = evaluate_payment_group_graph(
            &payment,
            GroupRequirements {
                group_ids: &ids,
                require_all: false,
                max_hops: 1,
            },
            OfflineFallback::Protocol,
            &GroupClient::new().unwrap(),
            &registry,
        )
        .await;
        server.abort();
        match scenario {
            "member" | "outage" => assert_eq!(result.decision, TrustDecision::Accept),
            "nonmember" => assert!(matches!(result.decision, TrustDecision::Reject { .. })),
            "unknown" => assert!(matches!(result.decision, TrustDecision::Warn { .. })),
            _ => unreachable!(),
        }
        assert_eq!(result.used_protocol_fallback, scenario == "outage");
        if scenario == "nonmember" {
            assert!((1..=2).contains(&result.checked_originators));
        } else {
            assert_eq!(result.checked_originators, 2);
        }
        let requests = observed.lock().unwrap();
        for creator in creators {
            if scenario == "nonmember" && creator != denied_for_assert { continue; }
            assert!(requests.contains(&format!("/root/verify/{creator}")));
            assert_eq!(
                requests.contains(&format!("/leaf/verify/{creator}")),
                scenario != "unknown"
            );
        }
    }
}

#[tokio::test]
async fn invalid_graph_policy_holds_without_network_or_protocol_fallback() {
    let ids = vec!["root".into()];
    for depth in [9, u8::MAX] {
        let result = evaluate_payment_group_graph(
            &tests::payment(),
            GroupRequirements {
                group_ids: &ids,
                require_all: false,
                max_hops: depth,
            },
            OfflineFallback::Protocol,
            &GroupClient::new().unwrap(),
            &GroupRegistry::new(),
        )
        .await;
        assert!(matches!(result.decision, TrustDecision::Warn { .. }));
        assert!(!result.used_protocol_fallback);
        assert_eq!(result.checked_originators, 0);
    }
    let policy = TrustPolicy::RequireGroupGraph {
        trusted_groups: ids,
        require_all: false,
        max_hops: 9,
    };
    let encoded = serde_json::to_vec(&policy).unwrap();
    let decoded: TrustPolicy = serde_json::from_slice(&encoded).unwrap();
    assert!(matches!(
        evaluate_policy(
            &decoded,
            &WalletAddress::new("brst_origin"),
            &GroupClient::new().unwrap(),
            &GroupRegistry::new(),
            &TrustContext::default()
        )
        .await,
        TrustDecision::Warn { .. }
    ));
}

#[tokio::test]
async fn payment_requests_share_one_limit_across_creators_and_groups() {
    use std::{sync::Arc, time::Duration};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = release.clone();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let gate = gate.clone();
            let seen = seen.clone();
            handlers.spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") { header.push(socket.read_u8().await.unwrap()); }
                seen.send(()).unwrap();
                gate.acquire().await.unwrap().forget();
                let body = r#"{"valid":true,"score":1}"#;
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    let mut report = tests::payment();
    let template = report.provenance.origins[0].clone();
    report.provenance.origins = (1..=12).map(|id| {
        let mut origin = template.clone();
        origin.hash = format!("{id:02x}").repeat(32);
        origin.originator = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[id;32]).public).to_string();
        origin
    }).collect();
    let mut registry = GroupRegistry::new();
    for id in ["first", "second"] {
        registry.register(GroupInfo { id: id.into(), name: id.into(), description: String::new(),
            endpoint_url: format!("{base}/{id}"), member_count: 0 }).unwrap();
    }
    let query = tokio::spawn(async move {
        evaluate_payment_groups(&report, &["first".into(), "second".into()], true,
            OfflineFallback::Review, &GroupClient::new().unwrap(), &registry).await
    });
    for _ in 0..8 {
        tokio::time::timeout(Duration::from_secs(2), requests.recv()).await.unwrap().unwrap();
    }
    assert!(tokio::time::timeout(Duration::from_millis(50), requests.recv()).await.is_err(), "more than eight requests in flight");
    release.add_permits(24);
    let result = tokio::time::timeout(Duration::from_secs(2), query).await.unwrap().unwrap();
    assert_eq!(result.decision, TrustDecision::Accept);
    assert_eq!(result.checked_originators, 12);
    for _ in 8..24 { requests.try_recv().unwrap(); }
    assert!(requests.try_recv().is_err());
    server.abort();
}

#[tokio::test]
async fn decisive_group_response_does_not_wait_for_a_stalled_group() {
    use std::time::Duration;
    for require_all in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (closed, mut disconnected) = tokio::sync::mpsc::unbounded_channel();
        let slow_started = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let server = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let closed = closed.clone();
                let slow_started = slow_started.clone();
                handlers.spawn(async move {
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") { header.push(socket.read_u8().await.unwrap()); }
                    if String::from_utf8(header).unwrap().contains("/slow/") {
                        slow_started.add_permits(1);
                        let mut byte = [0];
                        closed.send(socket.read(&mut byte).await).unwrap();
                    } else {
                        slow_started.acquire().await.unwrap().forget();
                        let body = format!(r#"{{"valid":{},"score":1}}"#, !require_all);
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    }
                });
            }
        });
        let mut registry = GroupRegistry::new();
        for id in ["slow", "fast"] {
            registry.register(GroupInfo { id: id.into(), name: id.into(), description: String::new(),
                endpoint_url: format!("{base}/{id}"), member_count: 0 }).unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), check_group_membership_with_depth(
            &["slow".into(), "fast".into()], require_all,
            &WalletAddress::new(tests::payment().provenance.origins[0].originator.clone()),
            &GroupClient::new().unwrap(), &registry, 0,
        )).await.unwrap();
        if require_all { assert!(matches!(result.decision, TrustDecision::Reject { .. })); }
        else { assert_eq!(result.decision, TrustDecision::Accept); }
        assert!(!result.offline_only);
        let closed = tokio::time::timeout(Duration::from_secs(2), disconnected.recv()).await.unwrap().unwrap();
        assert!(matches!(closed, Ok(0)) || closed.is_err(), "losing request was not cancelled");
        server.abort();
    }
}
