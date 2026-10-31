//! Structured logging initialization via `tracing`.

/// Install the process-wide formatting subscriber.
///
/// Checks `RUST_LOG` first, then falls back to `BURST_LOG_LEVEL`.
/// If neither is set, defaults to `info`. Invalid filters follow EnvFilter
/// parsing behavior; this helper does not report them as configuration errors.
/// Panics if a global subscriber has already been installed.
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
