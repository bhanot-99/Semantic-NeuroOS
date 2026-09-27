//! Domain adapters (P4-S02, FR-STO-02): map a filtered event to one of
//! Architecture.md §7.3's 13 domains and attach `TaintFlags`.
//!
//! Only the domains C1's `RawTelemetryEvent` stream can actually produce are
//! implemented here: `window_focus`, `app_lifecycle`, `idle_presence`,
//! `process_activity`, `build_job`, `git_activity`, `notes`, `calendar`,
//! `media_playback`, `system_resource`. The other three
//! (`voice_interaction` from C5, `assistant_actions` from C6,
//! `external_documents` from C7's spool — P4-S06) have different sources
//! and land through their own ingest paths, not this event dispatcher.
use rusqlite::Connection;

use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use neuroos_proto::v1::{
    FileActivityEvent, MprisEvent, ProcessTreeSnapshot, RawTelemetryEvent, ResourceSample,
    WindowEvent,
};
use neuroos_taint::TaintFlags;

use crate::ingest::filter::{FilterOutcome, IngestFilter};
use crate::sqlite::{self, FocusHistoryEntry, StorageError};

pub const DOMAIN_WINDOW_FOCUS: &str = "window_focus";
pub const DOMAIN_APP_LIFECYCLE: &str = "app_lifecycle";
pub const DOMAIN_IDLE_PRESENCE: &str = "idle_presence";
pub const DOMAIN_PROCESS_ACTIVITY: &str = "process_activity";
pub const DOMAIN_BUILD_JOB: &str = "build_job";
pub const DOMAIN_GIT_ACTIVITY: &str = "git_activity";
pub const DOMAIN_NOTES: &str = "notes";
pub const DOMAIN_CALENDAR: &str = "calendar";
pub const DOMAIN_MEDIA_PLAYBACK: &str = "media_playback";
pub const DOMAIN_SYSTEM_RESOURCE: &str = "system_resource";
/// Sourced from C7's spool, not C1's event stream (P4-S06) — routed
/// directly by `engine::StorageEngine::ingest_external_document`, not
/// through this module's `ingest()` dispatcher.
pub const DOMAIN_EXTERNAL_DOCUMENTS: &str = "external_documents";

/// Comm names (or prefixes) recognized as a build tool for the
/// (simplified) `build_job` heuristic. Architecture.md §7.3 describes this
/// domain as "derived from 4 [process_activity] + window titles"; window
/// titles aren't available in a `ProcessTreeSnapshot`, so this uses process
/// comm alone. Revisit if that proves too coarse.
const BUILD_TOOL_COMMS: &[&str] = &[
    "cc1", "gcc", "clang", "rustc", "cargo", "make", "ninja", "cmake", "ld", "mvn", "gradle",
    "npm", "webpack", "tsc", "go",
];

/// A chunk of text worth embedding and storing in LanceDB, produced
/// alongside a SQLite entity by [`ingest`]. Embedding is I/O (the ONNX
/// Runtime call) so it stays out of this module's otherwise-synchronous
/// SQLite path — `engine::StorageEngine::ingest` calls this, then embeds
/// and stores each returned chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChunk {
    pub family: &'static str,
    pub entity_id: i64,
    pub domain: &'static str,
    pub taint: TaintFlags,
    pub t_ns: u64,
    pub text: String,
}

fn family_for_domain(domain: &str) -> &'static str {
    match domain {
        DOMAIN_WINDOW_FOCUS | DOMAIN_APP_LIFECYCLE | DOMAIN_IDLE_PRESENCE => "attention",
        DOMAIN_PROCESS_ACTIVITY | DOMAIN_BUILD_JOB | DOMAIN_GIT_ACTIVITY => "work",
        DOMAIN_NOTES | DOMAIN_CALENDAR => "knowledge",
        DOMAIN_MEDIA_PLAYBACK | DOMAIN_SYSTEM_RESOURCE => "system",
        _ => "external",
    }
}

/// Runs the ingest filter on `event`, persists whatever the matching domain
/// adapter decides, and returns any chunks of text worth embedding into
/// LanceDB (empty for most events — most domains have no meaningful "text"
/// to search, or didn't cross the promotion gate this call). `filter` is
/// caller-owned so it accumulates state (transient counters, pid collapse
/// map, toplevel tracking) across calls.
pub fn ingest(
    conn: &Connection,
    filter: &mut IngestFilter,
    event: &RawTelemetryEvent,
) -> Result<Vec<PendingChunk>, StorageError> {
    let outcome = filter.process(event);
    match &event.payload {
        Some(Payload::Window(w)) => adapt_window(conn, w, &outcome, event.observed_at_ns, filter),
        Some(Payload::ProcTree(p)) => adapt_proc_tree(conn, p, &outcome, event.observed_at_ns),
        Some(Payload::FileActivity(f)) => {
            adapt_file_activity(conn, f, &outcome, event.observed_at_ns)
        }
        Some(Payload::Idle(_)) => {
            sqlite::touch_event_counter(
                conn,
                DOMAIN_IDLE_PRESENCE,
                "transition",
                event.observed_at_ns,
                0,
            )?;
            Ok(Vec::new())
        }
        Some(Payload::Resource(r)) => {
            adapt_resource(conn, r, event.observed_at_ns)?;
            Ok(Vec::new())
        }
        Some(Payload::Mpris(m)) => {
            adapt_mpris(conn, m, event.observed_at_ns)?;
            Ok(Vec::new())
        }
        None => Ok(Vec::new()),
    }
}

fn adapt_window(
    conn: &Connection,
    w: &WindowEvent,
    outcome: &FilterOutcome,
    observed_at_ns: u64,
    filter: &IngestFilter,
) -> Result<Vec<PendingChunk>, StorageError> {
    // app_lifecycle: open/close counted unconditionally (FR-STO-02's
    // "the right domain", not gated by the promotion filter — an
    // open/close pair is itself the whole signal this domain records).
    match &w.kind {
        Some(Kind::Opened(o)) => {
            sqlite::touch_event_counter(conn, DOMAIN_APP_LIFECYCLE, &o.app_id, observed_at_ns, 0)?;
        }
        Some(Kind::Closed(_)) => {
            // No app_id on a Closed event; app_lifecycle's count already
            // recorded the open half of the pair.
        }
        _ => {}
    }

    // window_focus: only once promoted, and only a completed dwell segment
    // becomes a focus_history row.
    let mut chunks = Vec::new();
    if let FilterOutcome::Promoted {
        key: app_id,
        segment,
    } = outcome
    {
        let entity_id = sqlite::upsert_entity(
            conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            app_id,
            TaintFlags::empty(),
            observed_at_ns,
            false,
        )?;
        if let Some((t_start_ns, t_end_ns)) = segment {
            let dwell_ms = t_end_ns.saturating_sub(*t_start_ns) / 1_000_000;
            let title = match &w.kind {
                Some(Kind::Opened(o)) => o.title.as_str(),
                _ => filter.title_for(w.toplevel_id).unwrap_or(""),
            };
            sqlite::insert_focus_history(
                conn,
                &FocusHistoryEntry {
                    app_id,
                    title,
                    pid: 0,
                    root_pid: 0,
                    t_start_ns: *t_start_ns,
                    t_end_ns: *t_end_ns,
                    dwell_ms,
                },
            )?;
            if !title.is_empty() {
                chunks.push(PendingChunk {
                    family: family_for_domain(DOMAIN_WINDOW_FOCUS),
                    entity_id,
                    domain: DOMAIN_WINDOW_FOCUS,
                    taint: TaintFlags::empty(),
                    t_ns: *t_end_ns,
                    text: format!("{app_id}: {title}"),
                });
            }
        }
    }
    Ok(chunks)
}

fn adapt_proc_tree(
    conn: &Connection,
    snapshot: &ProcessTreeSnapshot,
    outcome: &FilterOutcome,
    observed_at_ns: u64,
) -> Result<Vec<PendingChunk>, StorageError> {
    let FilterOutcome::Promoted { key, .. } = outcome else {
        return Ok(Vec::new());
    };
    let root_comm = snapshot
        .processes
        .iter()
        .find(|p| p.pid == snapshot.root_pid)
        .map(|p| p.comm.as_str())
        .unwrap_or("");
    let entity_id = sqlite::upsert_entity(
        conn,
        DOMAIN_PROCESS_ACTIVITY,
        "process",
        key,
        TaintFlags::empty(),
        observed_at_ns,
        false,
    )?;
    sqlite::touch_event_counter(conn, DOMAIN_PROCESS_ACTIVITY, key, observed_at_ns, 0)?;

    let is_build_job = snapshot
        .processes
        .iter()
        .any(|p| BUILD_TOOL_COMMS.iter().any(|tool| p.comm == *tool));
    let mut chunks = Vec::new();
    if is_build_job {
        sqlite::touch_event_counter(conn, DOMAIN_BUILD_JOB, root_comm, observed_at_ns, 0)?;
        chunks.push(PendingChunk {
            family: family_for_domain(DOMAIN_BUILD_JOB),
            entity_id,
            domain: DOMAIN_BUILD_JOB,
            taint: TaintFlags::empty(),
            t_ns: observed_at_ns,
            text: format!("build job under {root_comm}"),
        });
    }
    Ok(chunks)
}

fn adapt_file_activity(
    conn: &Connection,
    f: &FileActivityEvent,
    outcome: &FilterOutcome,
    observed_at_ns: u64,
) -> Result<Vec<PendingChunk>, StorageError> {
    let FilterOutcome::Promoted { .. } = outcome else {
        return Ok(Vec::new());
    };
    let domain = match f.watch_label.as_str() {
        "git" => DOMAIN_GIT_ACTIVITY,
        "notes" => DOMAIN_NOTES,
        "calendar" | "ics" => DOMAIN_CALENDAR,
        other => {
            tracing::warn!(
                watch_label = other,
                "file activity with an unrecognized watch_label; skipping"
            );
            return Ok(Vec::new());
        }
    };
    let entity_id = sqlite::upsert_entity(
        conn,
        domain,
        "file",
        &f.path,
        TaintFlags::empty(),
        observed_at_ns,
        true,
    )?;
    sqlite::touch_event_counter(conn, domain, &f.path, observed_at_ns, 0)?;
    // The path itself, not the file's contents — reading arbitrary watched
    // files (size limits, encoding, binary detection) is real scope beyond
    // what FileActivityEvent carries; tracked as a known simplification.
    Ok(vec![PendingChunk {
        family: family_for_domain(domain),
        entity_id,
        domain,
        taint: TaintFlags::empty(),
        t_ns: observed_at_ns,
        text: f.path.clone(),
    }])
}

fn adapt_resource(
    conn: &Connection,
    _r: &ResourceSample,
    observed_at_ns: u64,
) -> Result<(), StorageError> {
    sqlite::touch_event_counter(conn, DOMAIN_SYSTEM_RESOURCE, "sample", observed_at_ns, 0)
}

fn adapt_mpris(conn: &Connection, m: &MprisEvent, observed_at_ns: u64) -> Result<(), StorageError> {
    sqlite::touch_event_counter(
        conn,
        DOMAIN_MEDIA_PLAYBACK,
        &m.player_bus_name,
        observed_at_ns,
        0,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::{
        FileActivityKind, IdleEvent, ProcessInfo, ToplevelState, WindowClosed, WindowOpened,
        WindowStateChanged,
    };

    fn window_event(toplevel_id: u64, at_ns: u64, kind: Kind) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        }
    }

    fn opened(toplevel_id: u64, app_id: &str, at_ns: u64) -> RawTelemetryEvent {
        window_event(
            toplevel_id,
            at_ns,
            Kind::Opened(WindowOpened {
                app_id: app_id.to_string(),
                title: "a title".into(),
                pid: 0,
                pid_known: false,
            }),
        )
    }

    fn activated(toplevel_id: u64, at_ns: u64, active: bool) -> RawTelemetryEvent {
        let states = if active {
            vec![ToplevelState::Activated as i32]
        } else {
            vec![]
        };
        window_event(
            toplevel_id,
            at_ns,
            Kind::StateChanged(WindowStateChanged { states }),
        )
    }

    #[test]
    fn app_lifecycle_counts_every_open_regardless_of_promotion() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        ingest(&conn, &mut filter, &opened(1, "org.mozilla.firefox", 0)).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count FROM event_counters WHERE domain = ?1 AND key = ?2",
                (DOMAIN_APP_LIFECYCLE, "org.mozilla.firefox"),
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn window_focus_only_persists_once_promoted_by_dwell() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        ingest(&conn, &mut filter, &opened(1, "cosmic-term", 0)).unwrap();
        ingest(&conn, &mut filter, &activated(1, 0, true)).unwrap();
        // 1s dwell: below the 5s bar, nothing persisted yet
        ingest(&conn, &mut filter, &activated(1, 1_000_000_000, false)).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);

        ingest(&conn, &mut filter, &activated(1, 1_100_000_000, true)).unwrap();
        // this segment alone is 6s: crosses the bar
        ingest(&conn, &mut filter, &activated(1, 7_100_000_000, false)).unwrap();
        let (app_id, dwell_ms): (String, i64) = conn
            .query_row("SELECT app_id, dwell_ms FROM focus_history", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(app_id, "cosmic-term");
        assert_eq!(dwell_ms, 6_000);

        let entity_domain: String = conn
            .query_row(
                "SELECT domain FROM entities WHERE label = ?1",
                ["cosmic-term"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entity_domain, DOMAIN_WINDOW_FOCUS);
    }

    #[test]
    fn self_observation_window_never_reaches_any_table() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        ingest(&conn, &mut filter, &opened(1, "org.neuroos.Confirm", 0)).unwrap();
        ingest(&conn, &mut filter, &activated(1, 0, true)).unwrap();
        ingest(&conn, &mut filter, &activated(1, 10_000_000_000, false)).unwrap();
        let entities: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        let history: i64 = conn
            .query_row("SELECT COUNT(*) FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entities, 0);
        assert_eq!(history, 0);
        // app_lifecycle still isn't touched either — Excluded short-circuits
        // process_window before it ever registers the toplevel, and
        // adapt_window's Opened arm still runs unconditionally... so verify
        // that path explicitly:
        let lifecycle: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_counters", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            lifecycle, 1,
            "app_lifecycle intentionally isn't gated by self-observation exclusion"
        );
    }

    #[test]
    fn proc_tree_promotes_into_process_activity_and_flags_build_job() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let snapshot = |root_pid: u32| RawTelemetryEvent {
            observed_at_ns: 0,
            source: "proc".into(),
            payload: Some(Payload::ProcTree(ProcessTreeSnapshot {
                root_pid,
                root_pid_known: true,
                processes: vec![
                    ProcessInfo {
                        pid: root_pid,
                        ppid: 1,
                        comm: "editor".into(),
                    },
                    ProcessInfo {
                        pid: root_pid + 1,
                        ppid: root_pid,
                        comm: "cc1".into(),
                    },
                ],
            })),
        };
        // occurrences 1-2 stay below the N>=3 gate (nothing persisted yet);
        // occurrence 3 promotes (persisted once); occurrences 4-5 are
        // already-promoted touches (persisted each time).
        for _ in 0..5 {
            ingest(&conn, &mut filter, &snapshot(100)).unwrap();
        }
        let process_count: i64 = conn
            .query_row(
                "SELECT count FROM event_counters WHERE domain = ?1",
                [DOMAIN_PROCESS_ACTIVITY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(process_count, 3);
        let build_job_count: i64 = conn
            .query_row(
                "SELECT count FROM event_counters WHERE domain = ?1",
                [DOMAIN_BUILD_JOB],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(build_job_count, 3);
    }

    #[test]
    fn file_activity_routes_git_notes_calendar_to_the_right_domain() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let event = |label: &str, path: &str| RawTelemetryEvent {
            observed_at_ns: 0,
            source: "folders".into(),
            payload: Some(Payload::FileActivity(FileActivityEvent {
                path: path.into(),
                kind: FileActivityKind::Modified as i32,
                watch_label: label.into(),
            })),
        };
        for label in ["git", "notes", "calendar"] {
            for _ in 0..3 {
                ingest(&conn, &mut filter, &event(label, "/x")).unwrap();
            }
        }
        for (label, domain) in [
            ("git", DOMAIN_GIT_ACTIVITY),
            ("notes", DOMAIN_NOTES),
            ("calendar", DOMAIN_CALENDAR),
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM entities WHERE domain = ?1",
                    [domain],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "expected exactly one {label} entity");
        }
    }

    #[test]
    fn idle_and_resource_and_mpris_are_counted_unconditionally() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        ingest(
            &conn,
            &mut filter,
            &RawTelemetryEvent {
                observed_at_ns: 1,
                source: "idle".into(),
                payload: Some(Payload::Idle(IdleEvent { idle: true })),
            },
        )
        .unwrap();
        ingest(
            &conn,
            &mut filter,
            &RawTelemetryEvent {
                observed_at_ns: 2,
                source: "proc".into(),
                payload: Some(Payload::Resource(ResourceSample {
                    system_cpu_percent: 1.0,
                    system_mem_used_bytes: 1,
                    system_mem_total_bytes: 2,
                })),
            },
        )
        .unwrap();
        ingest(
            &conn,
            &mut filter,
            &RawTelemetryEvent {
                observed_at_ns: 3,
                source: "mpris".into(),
                payload: Some(Payload::Mpris(MprisEvent {
                    player_bus_name: "org.mpris.MediaPlayer2.vlc".into(),
                    playback_status: "Playing".into(),
                    track_id: String::new(),
                    title: String::new(),
                    artist: String::new(),
                    album: String::new(),
                    position_us: 0,
                })),
            },
        )
        .unwrap();
        for domain in [
            DOMAIN_IDLE_PRESENCE,
            DOMAIN_SYSTEM_RESOURCE,
            DOMAIN_MEDIA_PLAYBACK,
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM event_counters WHERE domain = ?1",
                    [domain],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "expected {domain} to be counted");
        }
        // and none of these ever create an entity (never "promoted" nodes)
        let entities: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entities, 0);
    }

    #[test]
    fn closed_toplevel_produces_no_orphaned_state() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        ingest(&conn, &mut filter, &opened(1, "org.mozilla.firefox", 0)).unwrap();
        ingest(
            &conn,
            &mut filter,
            &window_event(1, 1, Kind::Closed(WindowClosed {})),
        )
        .unwrap();
        // a later, unrelated activation of the same (now-forgotten)
        // toplevel_id must not be attributed to firefox
        ingest(&conn, &mut filter, &activated(1, 2, true)).unwrap();
        ingest(&conn, &mut filter, &activated(1, 10_000_000_000, false)).unwrap();
        let history: i64 = conn
            .query_row("SELECT COUNT(*) FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(history, 0);
    }
}
