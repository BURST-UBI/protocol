//! Size-bounded JSON responses for native RPC clients.

pub const MAX_RPC_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub mod navigation;
pub use navigation::allowed_app_navigation;

/// Read at most 16 MiB of response bytes, then parse JSON. The parsed value and
/// HTTP buffers can use additional memory. The caller owns status checks,
/// timeouts, redirect policy and validation of the RPC response shape.
pub async fn read_json(response: reqwest::Response) -> Result<serde_json::Value, String> {
    read_json_limit(response, MAX_RPC_RESPONSE_BYTES).await
}

async fn read_json_limit(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<serde_json::Value, String> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("RPC response exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("RPC response failed: {e}"))?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("RPC response exceeds byte limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid JSON response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn remote_and_lookalike_origins_cannot_navigate_the_app() {
        for url in [
            "tauri://localhost/index.html",
            "http://tauri.localhost/settings",
            "https://tauri.localhost/",
        ] {
            assert!(allowed_app_navigation(&url.parse().unwrap(), false, 1430));
        }
        for url in [
            "https://tauri.evil.test/",
            "http://tauri.localhost.evil.test/",
            "http://tauri.localhost:8888/",
            "https://evil.test/",
            "file:///etc/passwd",
            "http://user@tauri.localhost/",
        ] {
            assert!(!allowed_app_navigation(&url.parse().unwrap(), true, 1430));
        }
        let dev = "http://localhost:1430/".parse().unwrap();
        assert!(allowed_app_navigation(&dev, true, 1430));
        assert!(!allowed_app_navigation(&dev, false, 1430));
        assert!(!allowed_app_navigation(&dev, true, 1420));
    }

    async fn response(headers: &str, body: &str) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reply = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n{body}");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            let _ = socket.write_all(reply.as_bytes()).await;
        });
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn accepts_exact_limit_and_rejects_oversized_fixed_and_chunked_bodies() {
        let good = response("Content-Length: 7\r\n", "{\"a\":1}").await;
        assert_eq!(read_json_limit(good, 7).await.unwrap()["a"], 1);
        let oversized = response("Content-Length: 7\r\n", "{\"a\":1}").await;
        assert!(read_json_limit(oversized, 6)
            .await
            .unwrap_err()
            .contains("byte limit"));
        let chunked = response(
            "Transfer-Encoding: chunked\r\n",
            "4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n",
        )
        .await;
        assert!(read_json_limit(chunked, 6)
            .await
            .unwrap_err()
            .contains("byte limit"));
        let no_length = response("", "{\"a\":1}").await;
        assert!(read_json_limit(no_length, 6)
            .await
            .unwrap_err()
            .contains("byte limit"));
    }
}
