use tracing_subscriber::{fmt, EnvFilter};

const PROTOCOL_TRACE_ENV: &str = "FESTERM_PROTOCOL_TRACE";

pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("festerm=info,warn"));
    let protocol_trace_enabled = std::env::var(PROTOCOL_TRACE_ENV).is_ok_and(|value| value == "1");

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .init();

    if protocol_trace_enabled {
        tracing::warn!(
            target: "festerm::diagnostics",
            "protocol tracing was requested; terminal content tracing is not implemented yet"
        );
    }
}
