//! H11 / Architecture.md §6.2: the live C1 → C3 telemetry path. C3 is
//! `monitor.sock`'s client (server-push stream, §5.2): every
//! `RawTelemetryEvent` C1 publishes goes through the same ingest filter and
//! domain adapters the replay/test paths use (`StorageEngine::ingest`).
//! C1 being down or restarting is normal (AB-10): reconnect with jittered
//! exponential backoff, forever.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use neuroos_health::HealthServer;
use neuroos_ipc::{Backoff, DEFAULT_MAX_FRAME, FramingError, connect, read_envelope};
use neuroos_proto::v1::envelope;
use tokio::sync::Mutex;

use crate::engine::StorageEngine;

/// rules.md §5.7's control-call budget for the connect itself.
const CONNECT_DEADLINE: Duration = Duration::from_millis(250);

/// Runs forever: subscribe, ingest until the stream ends, back off,
/// reconnect. Must run on the same `LocalSet` as `server.rs` (the engine's
/// futures are `!Send`, see that module's doc comment).
pub async fn subscribe_forever(
    engine: Arc<Mutex<StorageEngine>>,
    monitor_sock: PathBuf,
    health: Arc<HealthServer>,
) {
    let mut backoff = Backoff::new();
    loop {
        match subscribe_once(&engine, &monitor_sock, &mut backoff, &health).await {
            Ok(()) => tracing::info!("monitor.sock stream ended; reconnecting"),
            Err(e) => tracing::debug!(error = %e, "monitor.sock unavailable; retrying"),
        }
        // M3: C3 with no telemetry feed is a component that cannot do its
        // main job, so it reports DEGRADED for as long as the feed is down
        // (cleared in `subscribe_once` once it is subscribed again).
        health.report_degraded(crate::server::MONITOR_FEED_DOWN);
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// One connection's lifetime. Resets `backoff` once connected, so the next
/// outage starts again from the shortest delay.
async fn subscribe_once(
    engine: &Mutex<StorageEngine>,
    monitor_sock: &std::path::Path,
    backoff: &mut Backoff,
    health: &HealthServer,
) -> Result<(), FramingError> {
    let mut stream = connect(monitor_sock, CONNECT_DEADLINE).await?;
    backoff.reset();
    health.set_status(neuroos_proto::v1::Status::Ok);
    tracing::info!("subscribed to monitor.sock");
    while let Some(env) = read_envelope(&mut stream, DEFAULT_MAX_FRAME).await? {
        let Some(envelope::Body::Telemetry(event)) = env.body else {
            continue;
        };
        // One bad event (e.g. a transient SQLite error) must not stop the
        // feed; the message carries no event content (R0-6).
        if let Err(e) = engine.lock().await.ingest(&event).await {
            tracing::warn!(error = %e, source = %event.source, "telemetry event not ingested");
            health.incr_error(crate::server::INGEST_FAILED);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use neuroos_ipc::{UdsServer, UdsServerConfig, write_envelope};
    use neuroos_proto::v1::raw_telemetry_event::Payload;
    use neuroos_proto::v1::window_event::Kind;
    use neuroos_proto::v1::{
        Envelope, RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened, WindowStateChanged,
    };

    use super::*;

    fn dev_models_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
    }

    /// One promoted focus session (6 s > the 5 s gate) for `app_id`.
    fn session(toplevel_id: u64, app_id: &str, start_ns: u64) -> Vec<Envelope> {
        let window = |at, kind| Envelope {
            schema_version: 1,
            trace_id: String::new(),
            request_id: 0,
            sent_at_ns: at,
            body: Some(envelope::Body::Telemetry(RawTelemetryEvent {
                observed_at_ns: at,
                source: "wayland_cosmic".into(),
                payload: Some(Payload::Window(WindowEvent {
                    toplevel_id,
                    kind: Some(kind),
                })),
            })),
        };
        vec![
            window(
                start_ns,
                Kind::Opened(WindowOpened {
                    app_id: app_id.into(),
                    title: format!("{app_id} window"),
                    pid: 0,
                    pid_known: false,
                }),
            ),
            window(
                start_ns,
                Kind::StateChanged(WindowStateChanged {
                    states: vec![ToplevelState::Activated as i32],
                }),
            ),
            window(
                start_ns + 6_000_000_000,
                Kind::StateChanged(WindowStateChanged { states: vec![] }),
            ),
        ]
    }

    /// A fake C1: serves each batch of envelopes on its own connection,
    /// then closes it (a C1 restart, from the subscriber's side).
    fn spawn_fake_monitor(sock: PathBuf, batches: Vec<Vec<Envelope>>) {
        let uid = rustix::process::getuid().as_raw();
        tokio::task::spawn_local(async move {
            let server = UdsServer::bind(UdsServerConfig::new(sock, vec![uid])).unwrap();
            for batch in batches {
                let (mut stream, _, _permit) = server.accept().await.unwrap().unwrap();
                for env in batch {
                    write_envelope(&mut stream, &env, DEFAULT_MAX_FRAME)
                        .await
                        .unwrap();
                }
            }
            std::future::pending::<()>().await;
        });
    }

    async fn wait_for_focus(engine: &Mutex<StorageEngine>, t_ns: u64) -> Option<String> {
        for _ in 0..200 {
            if let Some(row) = engine.lock().await.query_focus_history(t_ns, 0).unwrap() {
                return Some(row.app_id);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// Live proof: real `StorageEngine` (embedder + LanceDB + SQLite) fed
    /// over a real `monitor.sock`, surviving a C1 restart mid-stream.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models)"]
    async fn events_from_monitor_sock_reach_storage_across_a_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Mutex::new(
            StorageEngine::open(
                &dir.path().join("meta.sqlite3"),
                &dir.path().join("lance"),
                &dev_models_dir(),
                &dev_models_dir()
                    .join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so"),
            )
            .await
            .unwrap(),
        ));
        let sock = dir.path().join("monitor.sock");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                spawn_fake_monitor(
                    sock.clone(),
                    vec![
                        session(1, "org.gnome.TextEditor", 1_000_000_000),
                        session(2, "org.mozilla.firefox", 20_000_000_000),
                    ],
                );
                let health = HealthServer::new("neuroos-storage test");
                tokio::task::spawn_local(subscribe_forever(
                    Arc::clone(&engine),
                    sock,
                    Arc::clone(&health),
                ));

                assert_eq!(
                    wait_for_focus(&engine, 3_000_000_000).await.as_deref(),
                    Some("org.gnome.TextEditor")
                );
                // M3: C3 reports OK while it is subscribed, and counts the
                // feed going down when C1 restarts mid-stream.
                assert_eq!(
                    health.snapshot().status,
                    neuroos_proto::v1::Status::Ok as i32
                );
                assert_eq!(
                    wait_for_focus(&engine, 22_000_000_000).await.as_deref(),
                    Some("org.mozilla.firefox"),
                    "the second C1 connection must be picked up after the first closes"
                );
                assert_eq!(
                    health
                        .snapshot()
                        .error_counters
                        .get(crate::server::MONITOR_FEED_DOWN),
                    Some(&1),
                    "the first connection closing must have been counted"
                );
            })
            .await;
    }
}
