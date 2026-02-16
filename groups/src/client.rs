//! HTTP client for querying group verification endpoints.

use crate::error::GroupError;
use crate::types::MemberStatus;

/// Client for querying group trust endpoints.
///
/// Sends `GET /verify/{wallet_id}` to a group's endpoint and parses the response.
pub struct GroupClient {
    /// HTTP client (reusable connection pool).
    _http_client: reqwest::Client,
}

impl GroupClient {
    pub fn new() -> Self {
        Self {
            _http_client: reqwest::Client::new(),
        }
    }

    /// Query a group to check if a wallet is a valid member.
    ///
    /// `GET {endpoint_url}/verify/{wallet_id}` → MemberStatus
    pub async fn verify_member(
        &self,
        _endpoint_url: &str,
        _wallet_id: &str,
    ) -> Result<MemberStatus, GroupError> {
        todo!("send HTTP GET request, parse JSON response into MemberStatus")
    }

    /// Query multiple groups in parallel for a wallet.
    pub async fn verify_member_multi(
        &self,
        _endpoints: &[(&str, &str)], // (group_id, endpoint_url)
        _wallet_id: &str,
    ) -> Vec<(String, Result<MemberStatus, GroupError>)> {
        todo!("spawn concurrent requests, collect results")
    }
}

impl Default for GroupClient {
    fn default() -> Self {
        Self::new()
    }
}
