//! Structured logging initialization via `tracing`.

/// Initialize the tracing subscriber with sensible defaults.
///
/// Checks `RUST_LOG` first, then falls back to `BURST_LOG_LEVEL`.
/// If neither is set, defaults to `info`.
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if let Ok(level) = std::env::var("BURST_LOG_LEVEL") {
        EnvFilter::new(level)
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
