//! HTTP client for querying group verification endpoints.

use crate::error::GroupError;
use crate::types::MemberStatus;

use serde::Deserialize;
use std::time::Duration;

/// Default timeout for group verification requests.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default connection timeout.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Client for querying group trust endpoints.
///
/// Sends `GET /verify/{wallet_id}` to a group's endpoint and parses the response.
pub struct GroupClient {
    /// HTTP client (reusable connection pool).
    http_client: reqwest::Client,
}

/// Raw JSON response from a group's verification endpoint.
///
/// The API contract: `GET /verify/{wallet_address}` returns
/// `{"valid": bool, "score": float, "since": timestamp}`.
#[derive(Debug, Deserialize)]
struct VerifyResponse {
    valid: bool,
    score: f64,
    #[serde(default)]
    since: Option<u64>,
}

impl GroupClient {
    /// Create a new GroupClient with default timeout settings.
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self { http_client }
    }

    /// Create a GroupClient with a custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self { http_client }
    }

    /// Query a group to check if a wallet is a valid member.
    ///
    /// `GET {endpoint_url}/verify/{wallet_id}` -> MemberStatus
    pub async fn verify_member(
        &self,
        endpoint_url: &str,
        wallet_id: &str,
    ) -> Result<MemberStatus, GroupError> {
        let url = format!(
            "{}/verify/{}",
            endpoint_url.trim_end_matches('/'),
            wallet_id
        );

        let response = self.http_client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                GroupError::Unreachable(format!("request timed out: {e}"))
            } else if e.is_connect() {
                GroupError::Unreachable(format!("connection failed: {e}"))
            } else {
                GroupError::RequestFailed(e.to_string())
            }
        })?;

        if !response.status().is_success() {
            return Err(GroupError::RequestFailed(format!(
                "HTTP status {}",
                response.status()
            )));
        }

        let verify_resp: VerifyResponse = response.json().await.map_err(|e| {
            GroupError::InvalidResponse(format!("failed to parse verification response: {e}"))
        })?;

        Ok(MemberStatus {
            valid: verify_resp.valid,
            score: verify_resp.score,
            metadata: verify_resp.since.map(|s| serde_json::json!({ "since": s })),
        })
    }

    /// Query multiple groups in parallel for a wallet.
    ///
    /// Each endpoint is queried concurrently via `tokio::spawn`. Results are
    /// collected in the same order as the input. Failed requests are returned
    /// as `Err` values rather than panicking.
    pub async fn verify_member_multi(
        &self,
        endpoints: &[(&str, &str)], // (group_id, endpoint_url)
        wallet_id: &str,
    ) -> Vec<(String, Result<MemberStatus, GroupError>)> {
        let mut handles = Vec::with_capacity(endpoints.len());

        for (group_id, endpoint_url) in endpoints {
            let client = self.http_client.clone();
            let gid = group_id.to_string();
            let url = format!(
                "{}/verify/{}",
                endpoint_url.trim_end_matches('/'),
                wallet_id
            );

            handles.push(tokio::spawn(async move {
                let result = do_verify_request(&client, &url).await;
                (gid, result)
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok((gid, result)) => results.push((gid, result)),
                Err(e) => {
                    results.push((
                        "unknown".to_string(),
                        Err(GroupError::Other(format!("task join error: {e}"))),
                    ));
                }
            }
        }
        results
    }
}

/// Perform a single verification HTTP request.
///
/// Extracted as a standalone function to avoid lifetime issues with
/// `tokio::spawn` and `&self`.
async fn do_verify_request(
    client: &reqwest::Client,
    url: &str,
) -> Result<MemberStatus, GroupError> {
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            GroupError::Unreachable(format!("request timed out: {e}"))
        } else if e.is_connect() {
            GroupError::Unreachable(format!("connection failed: {e}"))
        } else {
            GroupError::RequestFailed(e.to_string())
        }
    })?;

    if !response.status().is_success() {
        return Err(GroupError::RequestFailed(format!(
            "HTTP status {}",
            response.status()
        )));
    }

    let verify_resp: VerifyResponse = response.json().await.map_err(|e| {
        GroupError::InvalidResponse(format!("failed to parse verification response: {e}"))
    })?;

    Ok(MemberStatus {
        valid: verify_resp.valid,
        score: verify_resp.score,
        metadata: verify_resp.since.map(|s| serde_json::json!({ "since": s })),
    })
}

impl Default for GroupClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_client_creation() {
        let client = GroupClient::new();
        // Verify the client is created without panicking
        drop(client);
    }

    #[test]
    fn test_group_client_with_timeout() {
        let client = GroupClient::with_timeout(Duration::from_secs(5));
        drop(client);
    }

    #[test]
    fn test_verify_response_deserialization() {
        let json = r#"{"valid": true, "score": 0.95, "since": 1700000000}"#;
        let resp: VerifyResponse = serde_json::from_str(json).unwrap();
        assert!(resp.valid);
        assert!((resp.score - 0.95).abs() < f64::EPSILON);
        assert_eq!(resp.since, Some(1700000000));
    }

    #[test]
    fn test_verify_response_without_since() {
        let json = r#"{"valid": false, "score": 0.0}"#;
        let resp: VerifyResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.valid);
        assert!((resp.score - 0.0).abs() < f64::EPSILON);
        assert_eq!(resp.since, None);
    }
}
