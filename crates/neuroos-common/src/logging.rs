//! Logging init (`tracing`, `rules.md` §4: never `println!`/`eprintln!` in
//! services). Reads `$NEUROOS_LOG` for the level filter (rules.md §4: the
//! only other allowed env var besides `NEUROOS_CONFIG`), defaulting to
//! `info`. Logs to the systemd journal when running under one (detected via
//! `$JOURNAL_STREAM`, set by systemd for every unit's stdout/stderr), else
//! to stderr.
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};

const DEFAULT_LEVEL: &str = "info";

fn filter() -> EnvFilter {
    EnvFilter::try_from_env("NEUROOS_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_LEVEL))
}

/// Initializes the global `tracing` subscriber. Call once, at the start of
/// `main`. Returns `Err` if a subscriber is already installed (e.g. called
/// twice) — callers should treat that as a startup invariant violation.
pub fn init_logging() -> Result<(), TryInitError> {
    let running_under_systemd = std::env::var_os("JOURNAL_STREAM").is_some();

    if running_under_systemd && let Ok(journald) = tracing_journald::layer() {
        return tracing_subscriber::registry()
            .with(filter())
            .with(journald)
            .try_init();
    }

    tracing_subscriber::registry()
        .with(filter())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()
}
