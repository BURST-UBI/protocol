//! Structured logging initialisation for the BURST node.
//!
//! Two output formats are supported:
//! - [`LogFormat::Human`] — coloured, human-readable lines (development).
//! - [`LogFormat::Json`] — newline-delimited JSON (production / log aggregation).
//!
//! The filter level can be overridden at runtime via the `RUST_LOG`
//! environment variable.  When `RUST_LOG` is not set, the caller-supplied
//! `level` string is used (e.g. `"info"`, `"debug,burst_node=trace"`).

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Selects the output format for structured logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Pretty-printed, coloured output for local development.
    Human,
    /// Newline-delimited JSON for production and log aggregation pipelines.
    Json,
}

/// Initialise the global tracing subscriber.
///
/// # Panics
///
/// Panics if a global subscriber has already been set (i.e. this function
/// was called twice in the same process).
pub fn init_logging(format: LogFormat, level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    match format {
        LogFormat::Human => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_thread_ids(true),
                )
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_thread_ids(true),
                )
                .init();
        }
    }
}
