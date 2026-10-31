use axum::http::{header, HeaderMap, StatusCode};
use std::{net::IpAddr, sync::Arc};

pub(crate) const MAX_BATCH: usize = 32;
pub(crate) const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Security {
    token_hash: Option<[u8; 32]>,
    pub requests: Arc<tokio::sync::Semaphore>,
}

impl Security {
    pub fn new(token: Option<&str>) -> Result<Self, String> {
        let token_hash = token
            .map(|token| {
                if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(
                        "BURST_RPC_ADMIN_TOKEN must contain 64 hexadecimal characters".to_string(),
                    );
                }
                Ok(burst_crypto::blake2b_256(token.as_bytes()))
            })
            .transpose()?;
        Ok(Self {
            token_hash,
            requests: Arc::new(tokio::sync::Semaphore::new(32)),
        })
    }

    /// Check batch size and require the admin bearer token for `genesis_endorse`.
    /// Other actions pass this check without a token; handlers validate their own
    /// inputs and permissions. One restricted item requires authorization for the batch.
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        body: &serde_json::Value,
    ) -> Result<usize, StatusCode> {
        let count = match body.as_array() {
            Some(items) if items.is_empty() || items.len() > MAX_BATCH => {
                return Err(StatusCode::BAD_REQUEST)
            }
            Some(items) => items.len(),
            None => 1,
        };
        let requires_admin = |item: &serde_json::Value| {
            item.get("action").and_then(|v| v.as_str()) == Some("genesis_endorse")
        };
        if requires_admin(body)
            || body
                .as_array()
                .is_some_and(|items| items.iter().any(requires_admin))
        {
            let supplied = headers
                .get(header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "));
            let valid = self
                .token_hash
                .zip(supplied)
                .is_some_and(|(expected, token)| {
                    token.len() == 64 && burst_crypto::blake2b_256(token.as_bytes()) == expected
                });
            if !valid {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        Ok(count)
    }
}

pub(crate) fn authority_bind_allowed(bind: &str, has_key: bool) -> bool {
    !has_key
        || bind.parse::<IpAddr>().is_ok_and(|ip| match ip {
            IpAddr::V4(ip) => ip.is_loopback(),
            IpAddr::V6(ip) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn authority_is_denied_by_default_and_checked_inside_batches() {
        let body = json!([{"action":"node_info"},{"action":"genesis_endorse"}]);
        let token = "ab".repeat(32);
        let security = Security::new(Some(&token)).unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(
            security.authorize(&headers, &body),
            Err(StatusCode::UNAUTHORIZED)
        );
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", "cd".repeat(32)).parse().unwrap(),
        );
        assert_eq!(
            security.authorize(&headers, &body),
            Err(StatusCode::UNAUTHORIZED)
        );
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert_eq!(security.authorize(&headers, &body), Ok(2));
        assert_eq!(
            Security::new(None).unwrap().authorize(&headers, &body),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            security.authorize(&HeaderMap::new(), &json!({"action":"process"})),
            Ok(1)
        );
    }

    #[test]
    fn rejects_oversized_batches_and_public_authority_bindings() {
        let security = Security::new(None).unwrap();
        assert!(security.authorize(&HeaderMap::new(), &json!([])).is_err());
        assert!(security
            .authorize(
                &HeaderMap::new(),
                &json!(vec![json!({"action":"node_info"}); MAX_BATCH + 1])
            )
            .is_err());
        assert_eq!(
            security.authorize(
                &HeaderMap::new(),
                &json!(vec![json!({"action":"node_info"}); MAX_BATCH])
            ),
            Ok(MAX_BATCH)
        );
        for address in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            assert!(authority_bind_allowed(address, true));
        }
        for address in ["0.0.0.0", "::", "192.168.1.4", "localhost"] {
            assert!(!authority_bind_allowed(address, true));
        }
        assert!(authority_bind_allowed("0.0.0.0", false));
        assert!(Security::new(Some("short")).is_err());
    }
}
