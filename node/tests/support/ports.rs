/// Probe the same wildcard IPv4 bind used by nodes. This does not reserve a
/// port across processes; retain startup failures rather than silently retrying.
pub fn available_port() -> u16 {
    static ISSUED: std::sync::Mutex<std::collections::BTreeSet<u16>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut issued = ISSUED.lock().unwrap();
    loop {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if issued.insert(port) {
            return port;
        }
    }
}
