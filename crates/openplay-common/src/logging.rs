use tracing_subscriber::{fmt, EnvFilter};

/// Initializes the tracing subscriber.
///
/// Respects `RUST_LOG` env var. Defaults to `info` level.
/// Example: `RUST_LOG=openplay_pipeline=debug,openplay_signaling=trace`
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact()
        .init();
}
