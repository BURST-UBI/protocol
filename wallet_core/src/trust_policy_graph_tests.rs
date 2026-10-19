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
        assert_eq!(result.checked_originators, 2);
        let requests = observed.lock().unwrap();
        for creator in creators {
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
