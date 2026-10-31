use super::*;
use crate::GroupInfo;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Replies = Arc<Mutex<BTreeMap<String, (u16, String)>>>;

struct Graph {
    registry: GroupRegistry,
    base: String,
    replies: Replies,
    requests: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Graph {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let replies = Replies::default();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let responses = replies.clone();
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    request.push(socket.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                assert!(request
                    .to_ascii_lowercase()
                    .contains("cache-control: no-cache, no-store"));
                observed.lock().unwrap().push(path.to_owned());
                let (status, body) = responses
                    .lock()
                    .unwrap()
                    .get(path)
                    .cloned()
                    .unwrap_or((404, "missing fixture".into()));
                let _ = socket.write_all(format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                ).as_bytes()).await;
            }
        });
        Self {
            registry: GroupRegistry::new(),
            base,
            replies,
            requests,
            server,
        }
    }

    fn group(&mut self, id: &str, member: bool, edges: &[&str]) {
        if self.registry.get(id).is_none() {
            self.registry
                .register(GroupInfo {
                    id: id.into(),
                    name: id.into(),
                    description: String::new(),
                    member_count: 0,
                    endpoint_url: format!("{}/{id}", self.base),
                })
                .unwrap();
        }
        self.reply(
            &format!("/{id}/verify/brst_origin"),
            200,
            &format!(r#"{{"valid":{member},"score":0.5}}"#),
        );
        self.reply(
            &format!("/{id}/trust"),
            200,
            &serde_json::json!({"version":1,"groups":edges}).to_string(),
        );
    }

    fn reply(&self, path: &str, status: u16, body: &str) {
        self.replies
            .lock()
            .unwrap()
            .insert(path.into(), (status, body.into()));
    }

    async fn check(&self, depth: u8) -> Result<Option<MembershipPath>, GroupError> {
        GroupClient::new()
            .unwrap()
            .verify_member_through_groups(&self.registry, "root", "brst_origin", depth)
            .await
    }
}

impl Drop for Graph {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn transitivity_is_explicit_and_depth_is_number_of_group_edges() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["middle"]);
    graph.group("middle", false, &["member"]);
    graph.group("member", true, &[]);
    assert!(graph.check(0).await.unwrap().is_none());
    assert_eq!(
        *graph.requests.lock().unwrap(),
        ["/root/verify/brst_origin"]
    );
    assert!(graph.check(1).await.unwrap().is_none());
    let path = graph.check(2).await.unwrap().unwrap();
    assert_eq!(path.groups, ["root", "middle", "member"]);
    assert!(path.member.valid);
}

#[tokio::test]
async fn cycles_terminate_and_duplicate_paths_query_each_group_once() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["a", "b"]);
    graph.group("a", false, &["root", "leaf"]);
    graph.group("b", false, &["a", "leaf"]);
    graph.group("leaf", false, &["root"]);
    assert!(graph.check(8).await.unwrap().is_none());
    let requests = graph.requests.lock().unwrap();
    assert_eq!(requests.len(), 8);
    assert_eq!(requests.iter().collect::<BTreeSet<_>>().len(), 8);
}

#[tokio::test]
async fn endorsements_and_membership_are_live_on_every_evaluation() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["first"]);
    graph.group("first", true, &[]);
    graph.group("second", true, &[]);
    assert_eq!(
        graph.check(2).await.unwrap().unwrap().groups,
        ["root", "first"]
    );
    graph.group("root", false, &["second"]);
    assert_eq!(
        graph.check(2).await.unwrap().unwrap().groups,
        ["root", "second"]
    );
    graph.group("second", false, &[]);
    assert!(graph.check(2).await.unwrap().is_none());
}

#[tokio::test]
async fn unregistered_endorsements_never_supply_an_endpoint_or_outage_fallback() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["https://unregistered.example.test"]);
    assert!(matches!(
        graph.check(2).await,
        Err(GroupError::GroupNotFound(_))
    ));
    assert_eq!(graph.requests.lock().unwrap().len(), 2);
    graph.reply(
        "/root/trust",
        200,
        r#"{"version":1,"groups":[],"endpoint_url":"https://replacement.example.test"}"#,
    );
    assert!(matches!(
        graph.check(2).await,
        Err(GroupError::InvalidResponse(_))
    ));
    assert!(graph
        .registry
        .get("root")
        .unwrap()
        .endpoint_url
        .starts_with(&graph.base));
}

#[tokio::test]
async fn malformed_endorsements_and_membership_require_review() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &[]);
    for body in [
        serde_json::json!({"version":2,"groups":[]}),
        serde_json::json!({"version":1,"groups":["a","a"]}),
        serde_json::json!({"version":1,"groups":[" "]}),
        serde_json::json!({"version":1,"groups":["a\nb"]}),
        serde_json::json!({"version":1,"groups":["x".repeat(129)]}),
        serde_json::json!({"version":1,"groups":(0..33).map(|n|n.to_string()).collect::<Vec<_>>()}),
        serde_json::json!({"version":1,"groups":[{"id":"a","endpoint_url":"https://a.test"}]}),
    ] {
        graph.reply("/root/trust", 200, &body.to_string());
        assert!(matches!(
            graph.check(2).await,
            Err(GroupError::InvalidResponse(_))
        ));
    }
    graph.reply(
        "/root/verify/brst_origin",
        200,
        r#"{"valid":true,"score":5}"#,
    );
    assert!(matches!(
        graph.check(2).await,
        Err(GroupError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn valid_alternate_path_can_succeed_but_outage_cannot_mask_bad_evidence() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["unknown", "offline", "member"]);
    graph.group("offline", false, &[]);
    graph.reply("/offline/verify/brst_origin", 503, "unavailable");
    graph.group("member", true, &[]);
    assert_eq!(
        graph.check(2).await.unwrap().unwrap().groups,
        ["root", "member"]
    );
    graph.group("member", false, &[]);
    assert!(matches!(
        graph.check(2).await,
        Err(GroupError::GroupNotFound(_))
    ));
    graph.group("root", false, &["offline"]);
    assert!(matches!(
        graph.check(2).await,
        Err(GroupError::Unreachable(_))
    ));
}

#[tokio::test]
async fn configured_depth_and_graph_size_have_hard_bounds() {
    let mut graph = Graph::new().await;
    assert!(matches!(graph.check(9).await, Err(GroupError::Other(_))));
    assert!(graph.requests.lock().unwrap().is_empty());
    let children: Vec<_> = (0..32).map(|n| format!("a{n}")).collect();
    let grandchildren: Vec<_> = (0..32).map(|n| format!("b{n}")).collect();
    graph.group(
        "root",
        false,
        &children.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    for id in children.iter().chain(&grandchildren) {
        graph.group(id, false, &[]);
    }
    graph.group(
        "a0",
        false,
        &grandchildren.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert!(matches!(graph.check(3).await, Err(GroupError::Other(_))));
    assert!(graph.requests.lock().unwrap().len() <= MAX_VISITED_GROUPS * 2);
}

#[tokio::test]
async fn overlapping_roots_share_requests_only_within_one_evaluation() {
    let mut graph = Graph::new().await;
    graph.group("root", false, &["leaf"]);
    graph.group("other", false, &["leaf"]);
    graph.group("leaf", true, &[]);
    let client = GroupClient::new().unwrap();
    let evaluation = client.evaluation();
    let (a, b) = futures_util::future::join(
        evaluation.verify_member_through_groups(&graph.registry, "root", "brst_origin", 1),
        evaluation.verify_member_through_groups(&graph.registry, "other", "brst_origin", 1),
    ).await;
    assert!(a.unwrap().is_some());
    assert!(b.unwrap().is_some());
    assert_eq!(graph.requests.lock().unwrap().iter().filter(|p| p.as_str() == "/leaf/verify/brst_origin").count(), 1);
    drop(evaluation);
    graph.group("leaf", false, &[]);
    assert!(client.verify_member_through_groups(&graph.registry, "root", "brst_origin", 1).await.unwrap().is_none());
    assert_eq!(graph.requests.lock().unwrap().iter().filter(|p| p.as_str() == "/leaf/verify/brst_origin").count(), 2);
}
