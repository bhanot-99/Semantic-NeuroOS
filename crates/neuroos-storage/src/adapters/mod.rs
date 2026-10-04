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

use crate::ingest::filter::{FilterOutcome, IngestFilter, normalize_title};
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
        Some(Payload::ProcTree(p)) => {
            adapt_proc_tree(conn, p, &outcome, event.observed_at_ns, filter)
        }
        Some(Payload::FileActivity(f)) => {
            adapt_file_activity(conn, f, &outcome, event.observed_at_ns, filter)
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
        Some(Payload::Mpris(m)) => adapt_mpris(conn, m, event.observed_at_ns),
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
            let title = normalize_title(match &w.kind {
                Some(Kind::Opened(o)) => o.title.as_str(),
                _ => filter.last_segment_title().unwrap_or(""),
            });
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
    filter: &mut IngestFilter,
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
        // M13: the same tree is re-sampled every 5s for as long as the
        // build runs, and this text is derived from the tree alone -- so
        // every sample used to produce an identical chunk. The counter
        // above is what records "a build was running at this time"; the
        // chunk only has to be searchable once.
        let text = format!("build job under {root_comm}");
        if filter.accept_chunk(&text, observed_at_ns) {
            chunks.push(PendingChunk {
                family: family_for_domain(DOMAIN_BUILD_JOB),
                entity_id,
                domain: DOMAIN_BUILD_JOB,
                taint: TaintFlags::empty(),
                t_ns: observed_at_ns,
                text,
            });
        }
    }
    Ok(chunks)
}

/// Path components that only ever hold machine-generated churn: a build
/// directory, a package cache, a VCS's own object store. M13: every file
/// event used to become an embedded chunk, so one `cargo build` (or one
/// `git gc`) buried the user's real file activity under thousands of paths
/// that say nothing about what they were doing.
const CHURN_COMPONENTS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".cache",
    ".venv",
    ".mypy_cache",
    ".pytest_cache",
];

/// The files *inside* `.git` that git writes on the user's behalf, rather
/// than as bookkeeping -- the one part of an excluded directory that is
/// real `git_activity` signal.
const GIT_USER_FILES: &[&str] = &["COMMIT_EDITMSG", "MERGE_MSG", "TAG_EDITMSG", "SQUASH_MSG"];

/// Editor and tooling scratch files: an autosave, a swapfile, a lockfile.
fn is_scratch_file(name: &str) -> bool {
    name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".tmp")
        || name.ends_with(".lock")
        || name.starts_with(".#")
        || (name.starts_with(".") && name.ends_with(".kate-swp"))
}

/// Whether `path` is machine churn rather than something the user did
/// (see [`CHURN_COMPONENTS`]).
pub fn is_churn_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if is_scratch_file(name) {
        return true;
    }
    if GIT_USER_FILES.contains(&name) {
        return false;
    }
    path.split('/')
        .any(|component| CHURN_COMPONENTS.contains(&component))
}

fn adapt_file_activity(
    conn: &Connection,
    f: &FileActivityEvent,
    outcome: &FilterOutcome,
    observed_at_ns: u64,
    filter: &mut IngestFilter,
) -> Result<Vec<PendingChunk>, StorageError> {
    let FilterOutcome::Promoted { .. } = outcome else {
        return Ok(Vec::new());
    };
    if is_churn_path(&f.path) {
        return Ok(Vec::new());
    }
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
    // M13: an editor autosaving writes the same path over and over, and
    // the path is the whole chunk, so repeats within the cooldown add
    // nothing but an embedding call. The entity's `last_seen_ns` and the
    // event counter above still move on every write.
    if !filter.accept_chunk(&f.path, observed_at_ns) {
        return Ok(Vec::new());
    }
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

/// MPRIS player names (bus-name segment) that are web browsers.
const BROWSER_PLAYERS: &[&str] = &[
    "brave", "chromium", "chrome", "firefox", "vivaldi", "opera", "edge", "epiphany",
];

/// Counts every MPRIS event, and the first time a titled track is seen
/// playing, records it as a `media_playback` entity plus one searchable
/// chunk naming the player, title and artist (for YouTube in a browser,
/// the artist is the channel). BUG-007: before this, media history never
/// reached search at all.
fn adapt_mpris(
    conn: &Connection,
    m: &MprisEvent,
    observed_at_ns: u64,
) -> Result<Vec<PendingChunk>, StorageError> {
    sqlite::touch_event_counter(
        conn,
        DOMAIN_MEDIA_PLAYBACK,
        &m.player_bus_name,
        observed_at_ns,
        0,
    )?;
    if m.playback_status != "Playing" || m.title.is_empty() {
        return Ok(Vec::new());
    }
    let is_new = !sqlite::entity_exists(conn, DOMAIN_MEDIA_PLAYBACK, &m.title)?;
    let entity_id = sqlite::upsert_entity(
        conn,
        DOMAIN_MEDIA_PLAYBACK,
        "media",
        &m.title,
        TaintFlags::empty(),
        observed_at_ns,
        false,
    )?;
    if !is_new {
        return Ok(Vec::new());
    }
    // "org.mpris.MediaPlayer2.brave.instance6407" -> "brave"
    let player = m
        .player_bus_name
        .trim_start_matches("org.mpris.MediaPlayer2.")
        .split('.')
        .next()
        .unwrap_or_default();
    let mut text = format!("{player} played \"{}\"", m.title);
    if !m.artist.is_empty() {
        // A browser's MPRIS "artist" is the site's uploader: for YouTube,
        // the channel. Saying so lets "which channel" questions match.
        if BROWSER_PLAYERS.contains(&player) {
            text.push_str(&format!(" (channel: {})", m.artist));
        } else {
            text.push_str(&format!(" by {}", m.artist));
        }
    }
    Ok(vec![PendingChunk {
        family: family_for_domain(DOMAIN_MEDIA_PLAYBACK),
        entity_id,
        domain: DOMAIN_MEDIA_PLAYBACK,
        taint: TaintFlags::empty(),
        t_ns: observed_at_ns,
        text,
    }])
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

    fn proc_tree_event(at_ns: u64, root_pid: u32, procs: &[(u32, &str)]) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "proc".into(),
            payload: Some(Payload::ProcTree(ProcessTreeSnapshot {
                root_pid,
                root_pid_known: true,
                processes: procs
                    .iter()
                    .map(|(pid, comm)| ProcessInfo {
                        pid: *pid,
                        ppid: 1,
                        comm: (*comm).to_string(),
                    })
                    .collect(),
            })),
        }
    }

    /// M13: C1 re-samples the process tree every 5 s, so a 10-minute
    /// build produced ~120 identical "build job under make" chunks --
    /// each one a real embedding call, a LanceDB row and an FTS row, all
    /// saying the same thing.
    #[test]
    fn an_ongoing_build_is_embedded_once_not_once_per_sample() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let mut chunks = 0;
        // 5s apart, as C1 samples; the first 3 cross the promotion gate.
        for sample in 0..24u64 {
            chunks += ingest(
                &conn,
                &mut filter,
                &proc_tree_event(sample * 5_000_000_000, 100, &[(100, "make"), (101, "cc1")]),
            )
            .unwrap()
            .len();
        }
        assert_eq!(
            chunks, 1,
            "one chunk for one build, not one per /proc sample"
        );
        // The event counter is still per sample: that is the signal it
        // exists to carry.
        let build_job_count: i64 = conn
            .query_row(
                "SELECT count FROM event_counters WHERE domain = ?1",
                [DOMAIN_BUILD_JOB],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(build_job_count, 22);
    }

    #[test]
    fn a_different_build_tool_is_still_its_own_chunk() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let mut texts = Vec::new();
        for sample in 0..3u64 {
            for (root, tool) in [(100u32, "make"), (200, "ninja")] {
                texts.extend(
                    ingest(
                        &conn,
                        &mut filter,
                        &proc_tree_event(sample * 5_000_000_000, root, &[(root, tool)]),
                    )
                    .unwrap()
                    .into_iter()
                    .map(|c| c.text),
                );
            }
        }
        assert_eq!(texts, vec!["build job under make", "build job under ninja"]);
    }

    /// M13: every file event became a chunk, so a `cargo build` writing
    /// thousands of files under `target/`, or git rewriting `.git/index`,
    /// filled LanceDB with paths that say nothing about what the user was
    /// doing -- and cost an embedding each.
    #[test]
    fn machine_churn_paths_are_not_embedded() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let event = |label: &str, path: &str, at_ns: u64| RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "folders".into(),
            payload: Some(Payload::FileActivity(FileActivityEvent {
                path: path.into(),
                kind: FileActivityKind::Modified as i32,
                watch_label: label.into(),
            })),
        };
        let noisy = [
            "/home/u/p/.git/index",
            "/home/u/p/.git/objects/ab/cdef",
            "/home/u/p/target/debug/build/x.o",
            "/home/u/p/node_modules/left-pad/index.js",
            "/home/u/p/__pycache__/x.pyc",
            "/home/u/notes/draft.md~",
            "/home/u/notes/.draft.md.swp",
        ];
        // Three events first so the watch label is past the promotion gate.
        for i in 0..3u64 {
            ingest(&conn, &mut filter, &event("notes", "/home/u/notes/a.md", i)).unwrap();
        }
        for (i, path) in noisy.iter().enumerate() {
            let chunks = ingest(&conn, &mut filter, &event("git", path, 1_000 + i as u64)).unwrap();
            assert!(chunks.is_empty(), "{path} should not be embedded");
        }
    }

    /// `.git` is also where the *meaningful* git signal lives, so the
    /// files git writes for the user are kept.
    #[test]
    fn a_real_commit_message_under_dot_git_is_still_embedded() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let event = |path: &str, at_ns: u64| RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "folders".into(),
            payload: Some(Payload::FileActivity(FileActivityEvent {
                path: path.into(),
                kind: FileActivityKind::Modified as i32,
                watch_label: "git".into(),
            })),
        };
        for i in 0..3u64 {
            ingest(
                &conn,
                &mut filter,
                &event("/home/u/p/.git/COMMIT_EDITMSG", i),
            )
            .unwrap();
        }
        let chunks = ingest(
            &conn,
            &mut filter,
            &event("/home/u/p/.git/COMMIT_EDITMSG", 10_000_000_000_000),
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
    }

    /// The same path touched over and over (an editor autosaving) is one
    /// chunk per cooldown window, not one per write.
    #[test]
    fn the_same_file_path_is_not_embedded_on_every_write() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let event = |at_ns: u64| RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "folders".into(),
            payload: Some(Payload::FileActivity(FileActivityEvent {
                path: "/home/u/notes/plan.md".into(),
                kind: FileActivityKind::Modified as i32,
                watch_label: "notes".into(),
            })),
        };
        let mut chunks = 0;
        for i in 0..20u64 {
            chunks += ingest(&conn, &mut filter, &event(i * 1_000_000_000))
                .unwrap()
                .len();
        }
        assert_eq!(chunks, 1);
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

    fn mpris(player: &str, status: &str, title: &str, artist: &str, at: u64) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at,
            source: "mpris".into(),
            payload: Some(Payload::Mpris(MprisEvent {
                player_bus_name: format!("org.mpris.MediaPlayer2.{player}"),
                playback_status: status.into(),
                track_id: String::new(),
                title: title.into(),
                artist: artist.into(),
                album: String::new(),
                position_us: 0,
            })),
        }
    }

    /// BUG-007: media history was only ever counted, never searchable, so
    /// "which channel" / "what did I watch" had no evidence at all.
    #[test]
    fn a_newly_playing_track_becomes_one_searchable_chunk() {
        let conn = sqlite::open_in_memory().unwrap();
        let mut filter = IngestFilter::new();
        let ev = |status, at| {
            mpris(
                "brave.instance6407",
                status,
                "IELTS Task 1 Guide",
                "IELTS Advantage",
                at,
            )
        };
        let first = ingest(&conn, &mut filter, &ev("Playing", 1)).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].domain, DOMAIN_MEDIA_PLAYBACK);
        assert_eq!(
            first[0].text,
            "brave played \"IELTS Task 1 Guide\" (channel: IELTS Advantage)"
        );
        // replays, pauses and stops of the same track add nothing new
        assert!(
            ingest(&conn, &mut filter, &ev("Paused", 2))
                .unwrap()
                .is_empty()
        );
        assert!(
            ingest(&conn, &mut filter, &ev("Playing", 3))
                .unwrap()
                .is_empty()
        );
        // no artist: the "by" clause is omitted
        let vlc = ingest(
            &conn,
            &mut filter,
            &mpris("vlc", "Playing", "Show.S01E03", "", 4),
        )
        .unwrap();
        assert_eq!(vlc[0].text, "vlc played \"Show.S01E03\"");
    }

    #[test]
    fn status_glyphs_are_stripped_from_titles() {
        // terminal apps animate a spinner in the title; an editor marks
        // unsaved files with '*': neither is part of what the window is
        assert_eq!(
            normalize_title("◐ Bottom dock revert — COSMIC Terminal"),
            "Bottom dock revert — COSMIC Terminal"
        );
        assert_eq!(normalize_title("✳ Claude Code"), "Claude Code");
        assert_eq!(
            normalize_title("*MASTER_PLAN.md (~/x) - gedit"),
            "MASTER_PLAN.md (~/x) - gedit"
        );
        assert_eq!(normalize_title("~/notes — Files"), "~/notes — Files");
        assert_eq!(normalize_title("(1) Inbox"), "(1) Inbox");
        assert_eq!(normalize_title("◐"), "");
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
