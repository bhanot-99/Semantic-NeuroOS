//! `storage.sock`: serves `QueryFocusHistory`/`QueryHybridVectorText`/
//! `Forget` to C5a, C5b, C6 and `neuroosctl` (Architecture.md §5.2). AB-1
//! forbids those components linking `neuroos-storage` directly, so this
//! socket is the only way anything outside C3 reaches stored data.
use std::path::PathBuf;
use std::sync::Arc;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{
    ChunkMatch, Envelope, Error, ErrorCode, FocusHistoryRow, ForgetResponse,
    QueryFocusHistoryResponse, QueryHybridResponse, Taint, envelope, forget_request,
};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::engine::StorageEngine;

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
    let local = tokio::task::LocalSet::new();
    local
        .run_until(serve_local(engine, path, allowed_uids))
        .await;
}

async fn serve_local(engine: Arc<Mutex<StorageEngine>>, path: PathBuf, allowed_uids: Vec<u32>) {
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
                tokio::task::spawn_local(handle_conn(stream, Arc::clone(&engine)));
            }
            Ok(None) => continue, // rejected peer (SO_PEERCRED not in allowlist); keep serving
            Err(e) => {
                tracing::warn!(error = %e, "storage.sock accept failed");
            }
        }
    }
}

async fn handle_conn(mut stream: UnixStream, engine: Arc<Mutex<StorageEngine>>) {
    loop {
        let env = match read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
            Ok(Some(env)) => env,
            Ok(None) => return, // peer closed cleanly
            Err(e) => {
                tracing::debug!(error = %e, "storage.sock read failed");
                return;
            }
        };
        let response = handle_request(env, &engine).await;
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

async fn handle_request(env: Envelope, engine: &Arc<Mutex<StorageEngine>>) -> Envelope {
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
}
