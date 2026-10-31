//! Desktop shell navigation policy, separate from response transport.

/// Allow bundled shell URLs or the configured loopback development port in debug.
/// This checks parsed scheme, host, port and credentials; it permits any path,
/// query or fragment on those origins. It does not authorize RPC destinations.
pub fn allowed_app_navigation(url: &reqwest::Url, debug: bool, dev_port: u16) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match (url.scheme(), url.host_str(), url.port()) {
        ("tauri", Some("localhost"), None) => true,
        ("http" | "https", Some("tauri.localhost"), None) => true,
        ("http", Some("localhost" | "127.0.0.1"), Some(port)) => debug && port == dev_port,
        _ => false,
    }
}
