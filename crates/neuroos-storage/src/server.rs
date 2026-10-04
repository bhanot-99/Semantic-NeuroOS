//! `storage.sock`: serves `QueryFocusHistory`/`QueryHybridVectorText`/
//! `Forget` to C5a, C5b, C6 and `neuroosctl` (Architecture.md §5.2). AB-1
//! forbids those components linking `neuroos-storage` directly, so this
//! socket is the only way anything outside C3 reaches stored data.
use std::path::PathBuf;
use std::sync::Arc;

use neuroos_health::HealthServer;
use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{
    ActivityItem, ChunkMatch, EdgeRow, EntityRow, Envelope, Error, ErrorCode, FocusHistoryRow,
    ForgetResponse, ListEdgesResponse, ListEntitiesResponse, MaintenanceResponse,
    PruneEdgesResponse, QueryActivityResponse, QueryFocusHistoryResponse, QueryHybridResponse,
    Taint, UpsertEdgeResponse, envelope, forget_request, maintenance_request,
};

/// Architecture.md §7.5: "keep 8".
const DEFAULT_BACKUPS_KEPT: usize = 8;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::engine::StorageEngine;

/// M3: C3's health endpoint reported OK no matter what, because nothing
/// ever called `incr_error`/`set_status`. Every request now lands in a
/// latency histogram under its kind's label and, when it fails, in an
/// error counter under the same label (a stable identifier, never user
/// content -- rules.md R0-6).
fn request_label(body: Option<&envelope::Body>) -> &'static str {
    match body {
        Some(envelope::Body::QueryFocusHistoryRequest(_)) => "query_focus_history",
        Some(envelope::Body::QueryActivityRequest(_)) => "query_activity",
        Some(envelope::Body::QueryHybridRequest(_)) => "query_hybrid",
        Some(envelope::Body::ForgetRequest(_)) => "forget",
        Some(envelope::Body::ListEntitiesRequest(_)) => "list_entities",
        Some(envelope::Body::ListEdgesRequest(_)) => "list_edges",
        Some(envelope::Body::UpsertEdgeRequest(_)) => "upsert_edge",
        Some(envelope::Body::PruneEdgesRequest(_)) => "prune_edges",
        Some(envelope::Body::MaintenanceRequest(_)) => "maintenance",
        _ => "unsupported",
    }
}

/// Counter name for a telemetry event C1 sent that could not be ingested.
pub const INGEST_FAILED: &str = "ingest_failed";
/// Counter name (and degraded reason) for the C1 -> C3 feed being down.
pub const MONITOR_FEED_DOWN: &str = "monitor_feed_disconnected";
/// Counter name for a spool document C7 left that could not be ingested.
pub const SPOOL_FAILED: &str = "spool_ingest_failed";

/// `rusqlite::Connection` (inside `StorageEngine`) is `!Sync` (it uses
/// `RefCell` internally), so a future that holds `&StorageEngine` across an
/// internal `.await` -- as `forget_by_app`/`forget_since` do, via
/// `delete_lance_rows` -- is not `Send`, and `tokio::spawn` requires `Send`.
/// A `LocalSet` + `spawn_local` sidesteps that: every connection still runs
/// on one thread, but that cost nothing real here, since every connection
/// already serializes on the same `Mutex<StorageEngine>` regardless (there
/// was never any genuine cross-thread parallelism to gain by using
/// `tokio::spawn` in the first place).
pub async fn serve(engine: Arc<Mutex<StorageEngine>>, path: PathBuf, allowed_uids: Vec<u32>) {
    serve_with_backups_dir(
        engine,
        path,
        allowed_uids,
        neuroos_common::paths::backups_dir(),
        HealthServer::new(concat!("neuroos-storage v", env!("CARGO_PKG_VERSION"))),
    )
    .await;
}

/// [`serve`], writing `storage backup` snapshots under `backups_dir` and
/// reporting into `health` (the same endpoint C3's health socket serves,
/// so a caller can read back what the requests recorded -- M3).
pub async fn serve_with_backups_dir(
    engine: Arc<Mutex<StorageEngine>>,
    path: PathBuf,
    allowed_uids: Vec<u32>,
    backups_dir: PathBuf,
    health: Arc<HealthServer>,
) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(serve_local(
            engine,
            path,
            allowed_uids,
            Arc::new(backups_dir),
            health,
        ))
        .await;
}

/// Where the running C3 service listens and what it feeds from.
pub struct ServicePaths {
    pub storage_sock: PathBuf,
    pub monitor_sock: PathBuf,
    pub spool_dir: PathBuf,
    pub backups_dir: PathBuf,
}

/// H11: the whole running C3 -- `storage.sock`, the live `monitor.sock`
/// telemetry feed (C1 → C3, Architecture.md §6.2) and the C7 spool watcher
/// (§6.3) -- on one `LocalSet`, sharing one engine.
pub async fn run_service(
    engine: Arc<Mutex<StorageEngine>>,
    paths: ServicePaths,
    allowed_uids: Vec<u32>,
    health: Arc<HealthServer>,
) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            tokio::task::spawn_local(crate::monitor_feed::subscribe_forever(
                Arc::clone(&engine),
                paths.monitor_sock,
                Arc::clone(&health),
            ));
            let spool_engine = Arc::clone(&engine);
            let spool_dir = paths.spool_dir;
            let spool_health = Arc::clone(&health);
            tokio::task::spawn_local(async move {
                crate::spool::watch_forever(&spool_engine, &spool_dir, &spool_health).await;
            });
            serve_local(
                engine,
                paths.storage_sock,
                allowed_uids,
                Arc::new(paths.backups_dir),
                health,
            )
            .await;
        })
        .await;
}

async fn serve_local(
    engine: Arc<Mutex<StorageEngine>>,
    path: PathBuf,
    allowed_uids: Vec<u32>,
    backups_dir: Arc<PathBuf>,
    health: Arc<HealthServer>,
) {
    let server = match UdsServer::bind(UdsServerConfig::new(path, allowed_uids)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind storage.sock");
            return;
        }
    };
    loop {
        match server.accept().await {
            Ok(Some((stream, _cred))) => {
                tokio::task::spawn_local(handle_conn(
                    stream,
                    Arc::clone(&engine),
                    Arc::clone(&backups_dir),
                    Arc::clone(&health),
                ));
            }
            Ok(None) => continue, // this one connection failed; keep serving
            Err(e) => {
                // M2: `accept` reports only an unusable listening socket as
                // an error now, so retrying would spin at full CPU forever.
                tracing::error!(error = %e, "storage.sock listener is unusable; stopped serving");
                return;
            }
        }
    }
}

async fn handle_conn(
    mut stream: UnixStream,
    engine: Arc<Mutex<StorageEngine>>,
    backups_dir: Arc<PathBuf>,
    health: Arc<HealthServer>,
) {
    loop {
        let env = match read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
            Ok(Some(env)) => env,
            Ok(None) => return, // peer closed cleanly
            Err(e) => {
                tracing::debug!(error = %e, "storage.sock read failed");
                return;
            }
        };
        let label = request_label(env.body.as_ref());
        let started = std::time::Instant::now();
        let response = handle_request(env, &engine, &backups_dir).await;
        health.record_latency(label, started.elapsed());
        // M3: one failed request is counted but does not make C3 sick --
        // see `HealthServer::set_status`'s doc comment for the convention.
        if matches!(response.body, Some(envelope::Body::Error(_))) {
            health.incr_error(label);
        }
        if write_envelope(&mut stream, &response, DEFAULT_MAX_FRAME)
            .await
            .is_err()
        {
            return;
        }
    }
}

fn internal_error(message: impl Into<String>) -> envelope::Body {
    envelope::Body::Error(Error {
        code: ErrorCode::Internal as i32,
        message: message.into(),
        retryable: true,
    })
}

async fn handle_request(
    env: Envelope,
    engine: &Arc<Mutex<StorageEngine>>,
    backups_dir: &std::path::Path,
) -> Envelope {
    let now = neuroos_common::now_ns();
    let body = match env.body {
        Some(envelope::Body::QueryFocusHistoryRequest(req)) => {
            let engine = engine.lock().await;
            match engine.query_focus_history(req.t_ns, req.window_ns) {
                Ok(row) => envelope::Body::QueryFocusHistoryResponse(QueryFocusHistoryResponse {
                    row: row.map(|r| FocusHistoryRow {
                        app_id: r.app_id,
                        title: r.title,
                        pid: r.pid,
                        root_pid: r.root_pid,
                        t_start_ns: r.t_start_ns,
                        t_end_ns: r.t_end_ns,
                        dwell_ms: r.dwell_ms,
                    }),
                }),
                // rules.md §5.5: fail closed -- a query error must not read
                // as "no window was focused" to a caller doing a deictic
                // snap for a capability decision.
                Err(e) => internal_error(format!("QueryFocusHistory failed: {e}")),
            }
        }
        Some(envelope::Body::QueryActivityRequest(req)) => {
            let engine = engine.lock().await;
            let item = |r: crate::sqlite::ActivityRow| ActivityItem {
                app_id: r.app_id,
                text: r.text,
                first_ns: r.first_ns,
                last_ns: r.last_ns,
                dwell_ms: r.dwell_ms,
                // M9 / ADR-0012: provenance travels with the item, so C5
                // no longer has to invent `taint: None` for it.
                taint: Some(Taint { flags: r.taint }),
            };
            match engine.query_activity(req.since_ns, req.until_ns, req.limit.max(1) as usize) {
                Ok((windows, media)) => {
                    envelope::Body::QueryActivityResponse(QueryActivityResponse {
                        windows: windows.into_iter().map(item).collect(),
                        media: media.into_iter().map(item).collect(),
                    })
                }
                Err(e) => internal_error(format!("QueryActivity failed: {e}")),
            }
        }
        Some(envelope::Body::QueryHybridRequest(req)) => {
            let mut engine = engine.lock().await;
            match engine
                .query_hybrid(&req.text, req.top_k.max(1) as usize)
                .await
            {
                Ok(matches) => envelope::Body::QueryHybridResponse(QueryHybridResponse {
                    matches: matches
                        .into_iter()
                        .map(|m| ChunkMatch {
                            chunk_id: m.chunk_id,
                            entity_id: m.entity_id,
                            text: m.text,
                            taint: Some(Taint { flags: m.taint }),
                            t_ns: m.t_ns,
                            domain: m.domain,
                            distance: m.distance,
                            keyword_score: m.keyword_score,
                        })
                        .collect(),
                }),
                Err(e) => internal_error(format!("QueryHybridVectorText failed: {e}")),
            }
        }
        Some(envelope::Body::ForgetRequest(req)) => {
            let mut engine = engine.lock().await;
            let result = match req.target {
                Some(forget_request::Target::AppId(app_id)) => engine.forget_by_app(&app_id).await,
                Some(forget_request::Target::SinceNs(since_ns)) => {
                    engine.forget_since(since_ns).await
                }
                None => Ok(0),
            };
            match result {
                Ok(forgotten) => envelope::Body::ForgetResponse(ForgetResponse {
                    forgotten: forgotten as u64,
                }),
                Err(e) => internal_error(format!("Forget failed: {e}")),
            }
        }
        Some(envelope::Body::ListEntitiesRequest(req)) => {
            let engine = engine.lock().await;
            match engine.list_entities(req.since_ns) {
                Ok(rows) => envelope::Body::ListEntitiesResponse(ListEntitiesResponse {
                    entities: rows
                        .into_iter()
                        .map(|e| EntityRow {
                            id: e.id,
                            domain: e.domain,
                            kind: e.kind,
                            label: e.label,
                            taint: e.taint,
                            created_ns: e.created_ns,
                            last_seen_ns: e.last_seen_ns,
                            permanent: e.permanent,
                        })
                        .collect(),
                }),
                Err(e) => internal_error(format!("ListEntities failed: {e}")),
            }
        }
        Some(envelope::Body::ListEdgesRequest(_)) => {
            let engine = engine.lock().await;
            match engine.list_edges() {
                Ok(rows) => envelope::Body::ListEdgesResponse(ListEdgesResponse {
                    edges: rows
                        .into_iter()
                        .map(|e| EdgeRow {
                            src: e.src,
                            dst: e.dst,
                            kind: e.kind,
                            weight: e.weight,
                            reinforced_ns: e.reinforced_ns,
                            hypothesis: e.hypothesis,
                        })
                        .collect(),
                }),
                Err(e) => internal_error(format!("ListEdges failed: {e}")),
            }
        }
        Some(envelope::Body::UpsertEdgeRequest(req)) => {
            let engine = engine.lock().await;
            let result = match req.edge {
                Some(e) => engine.upsert_edge(&crate::sqlite::EdgeRow {
                    src: e.src,
                    dst: e.dst,
                    kind: e.kind,
                    weight: e.weight,
                    reinforced_ns: e.reinforced_ns,
                    hypothesis: e.hypothesis,
                }),
                None => Ok(()),
            };
            match result {
                Ok(()) => envelope::Body::UpsertEdgeResponse(UpsertEdgeResponse { ok: true }),
                Err(e) => internal_error(format!("UpsertEdge failed: {e}")),
            }
        }
        Some(envelope::Body::PruneEdgesRequest(req)) => {
            let engine = engine.lock().await;
            match engine.prune_hypothesis_edges(req.older_than_ns) {
                Ok(pruned) => envelope::Body::PruneEdgesResponse(PruneEdgesResponse { pruned }),
                Err(e) => internal_error(format!("PruneEdges failed: {e}")),
            }
        }
        Some(envelope::Body::MaintenanceRequest(req)) => {
            let mut engine = engine.lock().await;
            match req.job {
                Some(maintenance_request::Job::Gc(_)) => match engine.gc(now).await {
                    Ok(summary) => envelope::Body::MaintenanceResponse(MaintenanceResponse {
                        entities_deleted: summary.entities_deleted as u64,
                        focus_history_deleted: summary.focus_history_deleted as u64,
                        backup_path: String::new(),
                    }),
                    Err(e) => internal_error(format!("GC failed: {e}")),
                },
                Some(maintenance_request::Job::Backup(job)) => {
                    let keep = match job.keep {
                        0 => DEFAULT_BACKUPS_KEPT,
                        n => n as usize,
                    };
                    let label = neuroos_common::time::utc_label(now);
                    match engine.backup(backups_dir, &label, keep).await {
                        Ok(path) => envelope::Body::MaintenanceResponse(MaintenanceResponse {
                            entities_deleted: 0,
                            focus_history_deleted: 0,
                            backup_path: path.display().to_string(),
                        }),
                        Err(e) => internal_error(format!("backup failed: {e}")),
                    }
                }
                None => internal_error("MaintenanceRequest without a job"),
            }
        }
        _ => internal_error("unsupported request on storage.sock"),
    };
    Envelope {
        schema_version: 1,
        trace_id: env.trace_id,
        request_id: env.request_id,
        sent_at_ns: now,
        body: Some(body),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::time::Duration;

    use neuroos_ipc::{connect, read_envelope_deadline, write_envelope_deadline};
    use neuroos_proto::v1::QueryFocusHistoryRequest;

    use super::*;

    fn current_uid() -> u32 {
        // SAFETY: getuid() takes no arguments and cannot fail.
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }

    fn dev_models_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
    }

    fn dev_onnxruntime_dylib() -> std::path::PathBuf {
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
    }

    /// Live proof (P5 prereq): a real `StorageEngine` served over a real
    /// `storage.sock`, queried by a real IPC client end to end -- needs the
    /// real model + onnxruntime fetched, so `#[ignore]`d like this crate's
    /// other real-download-dependent tests (`cargo test -p neuroos-storage
    /// --lib -- --ignored server`).
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn query_focus_history_round_trips_over_a_real_storage_sock_server() {
        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        // Seed one focus session directly through the real ingest path
        // (same as engine.rs's own tests), then serve it over a real
        // socket instead of calling the engine in-process.
        use neuroos_proto::v1::raw_telemetry_event::Payload;
        use neuroos_proto::v1::window_event::Kind;
        use neuroos_proto::v1::{
            RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened, WindowStateChanged,
        };
        let mut engine = engine;
        let window_event = |toplevel_id: u64, at_ns: u64, kind: Kind| RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        };
        engine
            .ingest(&window_event(
                1,
                0,
                Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "test window".into(),
                    pid: 0,
                    pid_known: false,
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                0,
                Kind::StateChanged(WindowStateChanged {
                    states: vec![ToplevelState::Activated as i32],
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                6_000_000_000,
                Kind::StateChanged(WindowStateChanged { states: vec![] }),
            ))
            .await
            .unwrap();

        let sock_dir = tempfile::tempdir().unwrap();
        let sock_path = sock_dir.path().join("storage.sock");
        let my_uid = current_uid();
        // `serve()` never returns; race it against the client so the test
        // itself drives both without a `tokio::spawn` (its future is
        // intentionally `!Send`, see `serve`'s doc comment).
        tokio::select! {
            _ = serve(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![my_uid]) => {
                panic!("storage.sock server exited unexpectedly");
            }
            _ = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let mut stream = connect(&sock_path, Duration::from_secs(1)).await.unwrap();
                let req = Envelope {
                    schema_version: 1,
                    trace_id: "test".into(),
                    request_id: 1,
                    sent_at_ns: neuroos_common::now_ns(),
                    body: Some(envelope::Body::QueryFocusHistoryRequest(
                        QueryFocusHistoryRequest {
                            t_ns: 3_000_000_000,
                            window_ns: 1_500_000_000,
                        },
                    )),
                };
                write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                    .await
                    .unwrap();
                let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                    .await
                    .unwrap()
                    .unwrap();
                match resp.body {
                    Some(envelope::Body::QueryFocusHistoryResponse(r)) => {
                        let row = r.row.expect("a window should have been focused at t=3s");
                        assert_eq!(row.app_id, "org.mozilla.firefox");
                    }
                    other => panic!("unexpected response: {other:?}"),
                }
            } => {}
        }
    }

    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn unsupported_request_gets_an_error_response() {
        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        let sock_dir = tempfile::tempdir().unwrap();
        let sock_path = sock_dir.path().join("storage.sock");
        let my_uid = current_uid();
        tokio::select! {
            _ = serve(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![my_uid]) => {
                panic!("storage.sock server exited unexpectedly");
            }
            _ = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let mut stream = connect(&sock_path, Duration::from_secs(1)).await.unwrap();
                // An IngestRequest is a real message type, just not one this
                // socket's server handles (that's monitor.sock's C3-side path).
                let req = Envelope {
                    schema_version: 1,
                    trace_id: "test".into(),
                    request_id: 1,
                    sent_at_ns: neuroos_common::now_ns(),
                    body: Some(envelope::Body::IngestRequest(
                        neuroos_proto::v1::IngestRequest {
                            source: "x".into(),
                            payload: vec![],
                        },
                    )),
                };
                write_envelope_deadline(&mut stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                    .await
                    .unwrap();
                let resp = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(resp.body, Some(envelope::Body::Error(_))));
            } => {}
        }
    }

    /// FR-KNO-10 (P5-S07 prereq): the Python cold worker's real
    /// read/write/prune surface, proved end to end over a real
    /// `storage.sock` -- not just the pure `sqlite.rs` unit tests.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn graph_rpcs_round_trip_over_a_real_storage_sock_server() {
        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let mut engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        // `edges` has a real `FOREIGN KEY REFERENCES entities(id)` -- seed
        // two real entities via the normal ingest path so UpsertEdge below
        // references ids that actually exist (in real use the cold worker
        // always gets its ids from a prior ListEntities call).
        use neuroos_proto::v1::raw_telemetry_event::Payload;
        use neuroos_proto::v1::window_event::Kind;
        use neuroos_proto::v1::{RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened};
        let window_event = |toplevel_id: u64, at_ns: u64, kind: Kind| RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        };
        for (toplevel_id, app_id) in [
            (1u64, "org.mozilla.firefox"),
            (2u64, "org.gnome.TextEditor"),
        ] {
            engine
                .ingest(&window_event(
                    toplevel_id,
                    0,
                    Kind::Opened(WindowOpened {
                        app_id: app_id.into(),
                        title: "test window".into(),
                        pid: 0,
                        pid_known: false,
                    }),
                ))
                .await
                .unwrap();
            engine
                .ingest(&window_event(
                    toplevel_id,
                    0,
                    Kind::StateChanged(neuroos_proto::v1::WindowStateChanged {
                        states: vec![ToplevelState::Activated as i32],
                    }),
                ))
                .await
                .unwrap();
            engine
                .ingest(&window_event(
                    toplevel_id,
                    6_000_000_000,
                    Kind::StateChanged(neuroos_proto::v1::WindowStateChanged { states: vec![] }),
                ))
                .await
                .unwrap();
        }
        let seeded_entity_ids: Vec<i64> = engine
            .list_entities(0)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            seeded_entity_ids.len(),
            2,
            "both windows must have become entities"
        );

        let sock_dir = tempfile::tempdir().unwrap();
        let sock_path = sock_dir.path().join("storage.sock");
        let my_uid = current_uid();
        tokio::select! {
            _ = serve(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![my_uid]) => {
                panic!("storage.sock server exited unexpectedly");
            }
            _ = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let mut stream = connect(&sock_path, Duration::from_secs(1)).await.unwrap();

                async fn roundtrip(
                    stream: &mut tokio::net::UnixStream,
                    body: envelope::Body,
                ) -> envelope::Body {
                    let req = Envelope {
                        schema_version: 1,
                        trace_id: "test".into(),
                        request_id: 1,
                        sent_at_ns: neuroos_common::now_ns(),
                        body: Some(body),
                    };
                    write_envelope_deadline(stream, &req, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                        .await
                        .unwrap();
                    read_envelope_deadline(stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                        .await
                        .unwrap()
                        .unwrap()
                        .body
                        .unwrap()
                }

                // The two seeded windows must come back over the real
                // socket, not just via the in-process `engine` call used
                // to seed them.
                match roundtrip(
                    &mut stream,
                    envelope::Body::ListEntitiesRequest(neuroos_proto::v1::ListEntitiesRequest {
                        since_ns: 0,
                    }),
                )
                .await
                {
                    envelope::Body::ListEntitiesResponse(r) => assert_eq!(r.entities.len(), 2),
                    other => panic!("unexpected response: {other:?}"),
                }

                // No edges exist yet -- ListEdges must answer empty, not
                // error, before the UpsertEdge below creates one.
                match roundtrip(
                    &mut stream,
                    envelope::Body::ListEdgesRequest(neuroos_proto::v1::ListEdgesRequest {}),
                )
                .await
                {
                    envelope::Body::ListEdgesResponse(r) => assert!(r.edges.is_empty()),
                    other => panic!("unexpected response: {other:?}"),
                }

                match roundtrip(
                    &mut stream,
                    envelope::Body::UpsertEdgeRequest(neuroos_proto::v1::UpsertEdgeRequest {
                        edge: Some(EdgeRow {
                            src: seeded_entity_ids[0],
                            dst: seeded_entity_ids[1],
                            kind: "co_occurs".into(),
                            weight: 1.5,
                            reinforced_ns: 1_000,
                            hypothesis: true,
                        }),
                    }),
                )
                .await
                {
                    envelope::Body::UpsertEdgeResponse(r) => assert!(r.ok),
                    other => panic!("unexpected response: {other:?}"),
                }

                match roundtrip(
                    &mut stream,
                    envelope::Body::ListEdgesRequest(neuroos_proto::v1::ListEdgesRequest {}),
                )
                .await
                {
                    envelope::Body::ListEdgesResponse(r) => {
                        assert_eq!(r.edges.len(), 1);
                        assert_eq!(r.edges[0].weight, 1.5);
                        assert!(r.edges[0].hypothesis);
                    }
                    other => panic!("unexpected response: {other:?}"),
                }

                match roundtrip(
                    &mut stream,
                    envelope::Body::PruneEdgesRequest(neuroos_proto::v1::PruneEdgesRequest {
                        older_than_ns: 2_000,
                    }),
                )
                .await
                {
                    envelope::Body::PruneEdgesResponse(r) => assert_eq!(r.pruned, 1),
                    other => panic!("unexpected response: {other:?}"),
                }

                match roundtrip(
                    &mut stream,
                    envelope::Body::ListEdgesRequest(neuroos_proto::v1::ListEdgesRequest {}),
                )
                .await
                {
                    envelope::Body::ListEdgesResponse(r) => {
                        assert!(r.edges.is_empty(), "pruned edge must be gone");
                    }
                    other => panic!("unexpected response: {other:?}"),
                }
            } => {}
        }
    }

    /// H12 live proof: `MaintenanceRequest` runs the real GC and a real
    /// backup over a real `storage.sock` (ADR-0011).
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn maintenance_gc_and_backup_run_over_a_real_storage_sock() {
        use neuroos_proto::v1::{BackupJob, GcJob, MaintenanceRequest};
        let dir = tempfile::tempdir().unwrap();
        let engine = StorageEngine::open(
            &dir.path().join("meta.sqlite3"),
            &dir.path().join("lance"),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");
        let sock_path = dir.path().join("storage.sock");
        let backups = dir.path().join("backups");

        let call = |job| {
            let sock_path = sock_path.clone();
            async move {
                let mut stream = connect(&sock_path, Duration::from_secs(1)).await.unwrap();
                let req = Envelope {
                    schema_version: 1,
                    trace_id: String::new(),
                    request_id: 1,
                    sent_at_ns: 0,
                    body: Some(envelope::Body::MaintenanceRequest(MaintenanceRequest {
                        job: Some(job),
                    })),
                };
                write_envelope_deadline(
                    &mut stream,
                    &req,
                    DEFAULT_MAX_FRAME,
                    Duration::from_secs(1),
                )
                .await
                .unwrap();
                read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(60))
                    .await
                    .unwrap()
                    .unwrap()
                    .body
            }
        };
        let health = HealthServer::new("neuroos-storage test");
        let observed = Arc::clone(&health);
        tokio::select! {
            _ = serve_with_backups_dir(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![current_uid()], backups.clone(), health) => {
                panic!("storage.sock server exited unexpectedly");
            }
            _ = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let gc = call(maintenance_request::Job::Gc(GcJob {})).await;
                assert!(matches!(gc, Some(envelope::Body::MaintenanceResponse(_))), "{gc:?}");
                let backup = call(maintenance_request::Job::Backup(BackupJob { keep: 0 })).await;
                let Some(envelope::Body::MaintenanceResponse(r)) = backup else {
                    panic!("expected a MaintenanceResponse, got {backup:?}");
                };
                let path = std::path::PathBuf::from(&r.backup_path);
                assert!(path.starts_with(&backups), "{path:?}");
                assert!(path.join("meta.sqlite3").exists());
                // M3: both succeeded, so they are timed but not counted as
                // errors -- C3's endpoint reported a flat OK with no
                // histograms at all before the fix.
                let snap = observed.snapshot();
                assert_eq!(
                    snap.latency_histograms.get("maintenance").map(|h| h.count),
                    Some(2),
                    "{:?}", snap.latency_histograms.keys().collect::<Vec<_>>()
                );
                assert!(snap.error_counters.is_empty(), "{:?}", snap.error_counters);
            } => {}
        }
    }

    /// M3: every request kind the socket serves has a stable label, so its
    /// latency histogram and error counter are findable by name. A kind
    /// missing from `request_label` would silently land in "unsupported".
    #[test]
    fn every_served_request_kind_has_its_own_label() {
        use neuroos_proto::v1::{
            ForgetRequest, ListEdgesRequest, ListEntitiesRequest, MaintenanceRequest,
            PruneEdgesRequest, QueryActivityRequest, QueryFocusHistoryRequest, QueryHybridRequest,
            UpsertEdgeRequest,
        };
        let kinds = [
            envelope::Body::QueryFocusHistoryRequest(QueryFocusHistoryRequest::default()),
            envelope::Body::QueryActivityRequest(QueryActivityRequest::default()),
            envelope::Body::QueryHybridRequest(QueryHybridRequest::default()),
            envelope::Body::ForgetRequest(ForgetRequest::default()),
            envelope::Body::ListEntitiesRequest(ListEntitiesRequest::default()),
            envelope::Body::ListEdgesRequest(ListEdgesRequest::default()),
            envelope::Body::UpsertEdgeRequest(UpsertEdgeRequest::default()),
            envelope::Body::PruneEdgesRequest(PruneEdgesRequest::default()),
            envelope::Body::MaintenanceRequest(MaintenanceRequest::default()),
        ];
        let labels: std::collections::BTreeSet<&str> =
            kinds.iter().map(|b| request_label(Some(b))).collect();
        assert_eq!(labels.len(), kinds.len(), "labels collide: {labels:?}");
        assert!(!labels.contains("unsupported"), "{labels:?}");
        assert_eq!(request_label(None), "unsupported");
    }
}
