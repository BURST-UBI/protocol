//! Live queries to off-chain group membership endpoints.

use crate::{GroupError, GroupRegistry, MemberStatus};
use futures_util::{stream, StreamExt};
use serde::Deserialize;
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 8;

pub struct GroupClient {
    #[cfg(not(target_arch = "wasm32"))]
    http_client: reqwest::Client,
    #[cfg(target_arch = "wasm32")]
    timeout: Duration,
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    valid: bool,
    score: f64,
    #[serde(default)]
    since: Option<u64>,
}

impl GroupClient {
    pub fn new() -> Result<Self, GroupError> {
        Self::with_timeout(Duration::from_secs(10))
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self, GroupError> {
        if timeout.is_zero() {
            return Err(GroupError::Other(
                "group request timeout must be positive".into(),
            ));
        }
        #[cfg(not(target_arch = "wasm32"))]
        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            // Do not disclose the queried wallet to another endpoint through redirects.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| GroupError::RequestFailed(error.to_string()))?;
        Ok(Self {
            #[cfg(not(target_arch = "wasm32"))]
            http_client,
            #[cfg(target_arch = "wasm32")]
            timeout,
        })
    }

    /// Queries live membership; scores are informational, not voting weight.
    pub async fn verify_member(
        &self,
        endpoint_url: &str,
        wallet_id: &str,
    ) -> Result<MemberStatus, GroupError> {
        let mut url = endpoint_url_checked(endpoint_url)?;
        if wallet_id.is_empty() || matches!(wallet_id, "." | "..") {
            return Err(GroupError::RequestFailed(
                "missing or invalid wallet identifier".into(),
            ));
        }
        url.path_segments_mut()
            .map_err(|()| {
                GroupError::RequestFailed("endpoint cannot contain a verification path".into())
            })?
            .pop_if_empty()
            .push("verify")
            .push(wallet_id);
        let body = self.get_bounded(url, MAX_RESPONSE_BYTES).await?;
        let verified: VerifyResponse = serde_json::from_slice(&body)
            .map_err(|error| GroupError::InvalidResponse(error.to_string()))?;
        if !verified.score.is_finite() || !(0.0..=1.0).contains(&verified.score) {
            return Err(GroupError::InvalidResponse(
                "membership score must be between 0 and 1".into(),
            ));
        }
        Ok(MemberStatus {
            valid: verified.valid,
            score: verified.score,
            metadata: verified
                .since
                .map(|since| serde_json::json!({ "since": since })),
        })
    }

    /// Fetch a public web directory (including an HTTP IPFS gateway). Listing a
    /// group does not trust it and fetching the directory discloses no wallet ID.
    pub async fn fetch_directory(&self, url: &str) -> Result<GroupRegistry, GroupError> {
        let body = self
            .get_bounded(endpoint_url_checked(url)?, crate::registry::MAX_DIRECTORY_BYTES)
            .await?;
        GroupRegistry::from_directory(&body)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn get_bounded(
        &self,
        url: reqwest::Url,
        max_bytes: usize,
    ) -> Result<Vec<u8>, GroupError> {
        let mut response = self
            .http_client
            .get(url)
            .header(reqwest::header::CACHE_CONTROL, "no-cache, no-store")
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            if response.status().is_server_error() {
                return Err(GroupError::Unreachable(format!(
                    "HTTP status {}",
                    response.status()
                )));
            }
            return Err(GroupError::RequestFailed(format!(
                "HTTP status {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(GroupError::InvalidResponse(format!(
                "group response exceeds {max_bytes} bytes"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            if chunk.len() > max_bytes - body.len() {
                return Err(GroupError::InvalidResponse(format!(
                    "group response exceeds {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn get_bounded(
        &self,
        url: reqwest::Url,
        max_bytes: usize,
    ) -> Result<Vec<u8>, GroupError> {
        use burst_browser::FetchError;
        burst_browser::fetch_bytes(url.as_str(), None, self.timeout, max_bytes)
            .await
            .map_err(|error| match error {
                FetchError::Timeout | FetchError::Http(500..=599) => {
                    GroupError::Unreachable(error.to_string())
                }
                FetchError::TooLarge(_) | FetchError::Invalid => {
                    GroupError::InvalidResponse(error.to_string())
                }
                // Browsers do not distinguish CORS, redirects and network failure.
                // Ambiguous failures must not trigger automatic protocol fallback.
                _ => GroupError::RequestFailed(error.to_string()),
            })
    }

    /// Bounded concurrent queries, in input order. Dropping this future cancels them.
    pub async fn verify_member_multi(
        &self,
        endpoints: &[(&str, &str)],
        wallet_id: &str,
    ) -> Vec<(String, Result<MemberStatus, GroupError>)> {
        let queries: Vec<_> = endpoints
            .iter()
            .copied()
            .map(|(group_id, endpoint)| async move {
                (
                    group_id.to_string(),
                    self.verify_member(endpoint, wallet_id).await,
                )
            })
            .collect();
        stream::iter(queries)
            .buffered(MAX_CONCURRENT_REQUESTS)
            .collect()
            .await
    }
}

pub(crate) fn endpoint_url_checked(endpoint: &str) -> Result<reqwest::Url, GroupError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| GroupError::RequestFailed(error.to_string()))?;
    if endpoint.len() > 2048
        || !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(GroupError::RequestFailed(
            "expected an HTTP(S) URL up to 2048 bytes without credentials, query or fragment"
                .into(),
        ));
    }
    Ok(url)
}

#[cfg(not(target_arch = "wasm32"))]
fn request_error(error: reqwest::Error) -> GroupError {
    if error.is_timeout() || error.is_connect() {
        GroupError::Unreachable(error.to_string())
    } else {
        GroupError::RequestFailed(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 8192);
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        String::from_utf8(header).unwrap()
    }

    async fn endpoint(body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            // The bounded client may disconnect before an oversized body finishes.
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await;
        });
        url
    }

    #[test]
    fn zero_timeout_is_rejected() {
        assert!(GroupClient::with_timeout(Duration::ZERO).is_err());
    }

    #[tokio::test]
    async fn directory_fetch_is_bounded_and_does_not_query_listed_endpoints() {
        let body = serde_json::json!({"version":1,"groups":[{
            "id":"local","name":"Community","description":"Example listing",
            "endpoint_url":"https://unqueried.invalid/community","member_count":5
        }]})
        .to_string();
        let client = GroupClient::new().unwrap();
        let registry = client.fetch_directory(&endpoint(body).await).await.unwrap();
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.get("local").unwrap().member_count, 5);
        assert!(client
            .fetch_directory(&endpoint(" ".repeat(1024 * 1024 + 1)).await)
            .await
            .is_err());
        assert!(client
            .fetch_directory(&endpoint("{}".into()).await)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn directory_download_cannot_redirect_or_send_a_wallet_identifier() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/directory.json", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(request.starts_with("GET /directory.json HTTP/1.1\r\n"));
            assert!(!request.contains("/verify/"));
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://unqueried.invalid\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        assert!(GroupClient::new()
            .unwrap()
            .fetch_directory(&url)
            .await
            .is_err());
        server.await.unwrap();
    }

    #[test]
    fn test_group_client_creation() {
        assert!(GroupClient::new().is_ok());
    }

    #[test]
    fn test_group_client_with_timeout() {
        assert!(GroupClient::with_timeout(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn test_verify_response_deserialization() {
        let response: VerifyResponse =
            serde_json::from_str(r#"{"valid":true,"score":0.95,"since":1700000000}"#).unwrap();
        assert!(response.valid);
        assert_eq!(response.score, 0.95);
        assert_eq!(response.since, Some(1700000000));
    }

    #[test]
    fn test_verify_response_without_since() {
        let response: VerifyResponse =
            serde_json::from_str(r#"{"valid":false,"score":0}"#).unwrap();
        assert!(!response.valid);
        assert_eq!(response.score, 0.0);
        assert_eq!(response.since, None);
    }

    #[tokio::test]
    async fn redirects_do_not_disclose_wallets_to_another_endpoint() {
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let redirected = format!("http://{}/unexpected", destination.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {redirected}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert!(matches!(
            GroupClient::new()
                .unwrap()
                .verify_member(&url, "brst_fixture")
                .await,
            Err(GroupError::RequestFailed(_))
        ));
        task.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn multi_limits_concurrency_and_cancellation_does_not_start_queued_queries() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (seen, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut sockets = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let seen = seen.clone();
                sockets.spawn(async move {
                    read_request(&mut socket).await;
                    seen.send(()).unwrap();
                    std::future::pending::<()>().await;
                });
            }
        });
        let query = tokio::spawn(async move {
            let client = GroupClient::new().unwrap();
            client
                .verify_member_multi(&vec![("community", url.as_str()); 20], "brst_fixture")
                .await
        });
        for _ in 0..8 {
            tokio::time::timeout(Duration::from_secs(2), requests.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), requests.recv())
                .await
                .is_err()
        );
        query.abort();
        assert!(query.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), requests.recv())
                .await
                .is_err()
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn out_of_range_score_is_not_membership_evidence() {
        for score in ["2", "-0.1", "null", "1e309"] {
            let url = endpoint(format!(r#"{{"valid":true,"score":{score}}}"#)).await;
            assert!(matches!(
                GroupClient::new()
                    .unwrap()
                    .verify_member(&url, "brst_fixture")
                    .await,
                Err(GroupError::InvalidResponse(_))
            ));
        }
    }

    #[tokio::test]
    async fn oversized_response_is_not_membership_evidence() {
        let url = endpoint(format!(
            r#"{{"valid":true,"score":1,"padding":"{}"}}"#,
            "x".repeat(65536)
        ))
        .await;
        assert!(matches!(
            GroupClient::new()
                .unwrap()
                .verify_member(&url, "brst_fixture")
                .await,
            Err(GroupError::InvalidResponse(_))
        ));
    }

    #[tokio::test]
    async fn chunked_body_is_also_bounded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let chunk = "x".repeat(4096);
            for _ in 0..17 {
                if socket
                    .write_all(format!("1000\r\n{chunk}\r\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        assert!(matches!(
            GroupClient::new()
                .unwrap()
                .verify_member(&url, "brst_fixture")
                .await,
            Err(GroupError::InvalidResponse(_))
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn multi_preserves_identity_order_and_uses_the_same_validation() {
        let yes = endpoint(r#"{"valid":true,"score":0.95,"since":1700000000}"#.into()).await;
        let invalid = endpoint(r#"{"valid":true,"score":2}"#.into()).await;
        let no = endpoint(r#"{"valid":false,"score":0}"#.into()).await;
        let results = GroupClient::new()
            .unwrap()
            .verify_member_multi(
                &[("yes", &yes), ("invalid", &invalid), ("no", &no)],
                "brst_fixture",
            )
            .await;
        assert_eq!(
            results
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["yes", "invalid", "no"]
        );
        let member = results[0].1.as_ref().unwrap();
        assert!(member.valid);
        assert_eq!(member.metadata.as_ref().unwrap()["since"], 1700000000);
        assert!(matches!(results[1].1, Err(GroupError::InvalidResponse(_))));
        assert!(!results[2].1.as_ref().unwrap().valid);
    }

    #[tokio::test]
    async fn repeated_queries_are_live_and_wallet_is_one_path_segment() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/community/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for valid in [true, false] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                assert!(
                    request.starts_with("GET /community/verify/brst_a%2Fb%3Fc HTTP/1.1\r\n"),
                    "{request}"
                );
                assert!(request.contains("cache-control: no-cache, no-store"));
                let body = format!(r#"{{"valid":{valid},"score":1}}"#);
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = GroupClient::new().unwrap();
        assert!(
            client
                .verify_member(&url, "brst_a/b?c")
                .await
                .unwrap()
                .valid
        );
        assert!(
            !client
                .verify_member(&url, "brst_a/b?c")
                .await
                .unwrap()
                .valid
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn body_timeout_is_unreachable_not_a_negative_membership() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let client = GroupClient::with_timeout(Duration::from_millis(100)).unwrap();
        assert!(matches!(
            client.verify_member(&url, "brst_fixture").await,
            Err(GroupError::Unreachable(_))
        ));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn invalid_endpoints_and_wallet_identifiers_are_rejected() {
        let client = GroupClient::new().unwrap();
        for url in [
            "file:///tmp/groups",
            "http://user:pass@localhost",
            "https://group.test/?a=b",
            "https://group.test/#a",
        ] {
            assert!(matches!(
                client.verify_member(url, "brst_fixture").await,
                Err(GroupError::RequestFailed(_))
            ));
        }
        for wallet in ["", ".", ".."] {
            assert!(matches!(
                client.verify_member("http://127.0.0.1:1", wallet).await,
                Err(GroupError::RequestFailed(_))
            ));
        }
    }
}
