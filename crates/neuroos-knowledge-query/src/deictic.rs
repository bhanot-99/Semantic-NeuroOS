//! P5-S01 / FR-KNO-01: resolves "this" to the window that was focused when
//! the user spoke, by querying C3's focus history at `t_speech_start ±
//! 1.5 s` over `storage.sock`. The overlap/tie-break selection itself
//! lives entirely in C3 (`neuroos_storage::sqlite::query_focus_history`,
//! P4-S04, already tested there); this module only calls it and adapts the
//! wire type into a small local struct C5a's prompt assembly can consume.
use neuroos_proto::v1::FocusHistoryRow;

use crate::storage_client::{StorageClient, StorageClientError};

/// Architecture.md §6.1 / §9.1: the deictic-snap window.
pub const DEICTIC_WINDOW_NS: u64 = 1_500_000_000; // 1.5 s

#[derive(Debug, Clone, PartialEq)]
pub struct WindowContext {
    pub app_id: String,
    pub title: String,
    pub pid: u32,
    pub root_pid: u32,
    pub t_start_ns: u64,
    pub t_end_ns: u64,
    pub dwell_ms: u64,
}

impl From<FocusHistoryRow> for WindowContext {
    fn from(r: FocusHistoryRow) -> Self {
        Self {
            app_id: r.app_id,
            title: r.title,
            pid: r.pid,
            root_pid: r.root_pid,
            t_start_ns: r.t_start_ns,
            t_end_ns: r.t_end_ns,
            dwell_ms: r.dwell_ms,
        }
    }
}

/// `None` iff nothing was focused within `t_speech_start ± 1.5 s` (a real,
/// expected case -- e.g. right after login, or a wake word said to an
/// empty desktop). Callers must degrade gracefully, not treat it as an
/// error (rules.md §5.6).
pub async fn snap(
    client: &StorageClient,
    t_speech_start_ns: u64,
) -> Result<Option<WindowContext>, StorageClientError> {
    let row = client
        .query_focus_history(t_speech_start_ns, DEICTIC_WINDOW_NS)
        .await?;
    Ok(row.map(WindowContext::from))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn window_context_maps_every_field_from_the_wire_row() {
        let row = FocusHistoryRow {
            app_id: "org.mozilla.firefox".into(),
            title: "quarterly revenue dashboard".into(),
            pid: 1234,
            root_pid: 1200,
            t_start_ns: 1_000_000_000,
            t_end_ns: 5_000_000_000,
            dwell_ms: 4000,
        };
        let ctx = WindowContext::from(row);
        assert_eq!(ctx.app_id, "org.mozilla.firefox");
        assert_eq!(ctx.title, "quarterly revenue dashboard");
        assert_eq!(ctx.pid, 1234);
        assert_eq!(ctx.root_pid, 1200);
        assert_eq!(ctx.t_start_ns, 1_000_000_000);
        assert_eq!(ctx.t_end_ns, 5_000_000_000);
        assert_eq!(ctx.dwell_ms, 4000);
    }
}
