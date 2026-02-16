//! Structured logging initialization via `tracing`.

/// Initialize the tracing subscriber with sensible defaults.
///
/// Respects the `RUST_LOG` environment variable for filtering.
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
}
