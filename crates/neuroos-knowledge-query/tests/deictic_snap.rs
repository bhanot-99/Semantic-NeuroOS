//! P5-S01 / FR-KNO-01: deictic snap against a real `storage.sock` server
//! backed by a real `StorageEngine`, over the real IPC boundary. AB-1
//! forbids `neuroos-knowledge-query`'s production code from linking
//! `neuroos-storage` -- this crate-level integration test is allowed to,
//! as a `[dev-dependencies]`-only entry (see Cargo.toml), matching the
//! precedent `neuroosctl` already set for `neuroos-healthd`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use std::sync::Arc;
use std::time::Duration;

use neuroos_knowledge_query::deictic;
use neuroos_knowledge_query::storage_client::StorageClient;
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use neuroos_proto::v1::{
    RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened, WindowStateChanged,
};
use neuroos_storage::engine::StorageEngine;
use tokio::sync::Mutex;

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

/// Live proof: a real focus session ingested by a real `StorageEngine`,
/// served over a real `storage.sock`, resolved by C5a's real deictic-snap
/// client -- needs the real model + onnxruntime fetched, so `#[ignore]`d
/// like this workspace's other real-download-dependent tests (`cargo test
/// -p neuroos-knowledge-query -- --ignored`).
#[tokio::test]
#[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
async fn snap_resolves_to_the_window_focused_when_the_user_spoke() {
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

    engine
        .ingest(&window_event(
            1,
            0,
            Kind::Opened(WindowOpened {
                app_id: "org.mozilla.firefox".into(),
                title: "quarterly revenue dashboard".into(),
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
    // `serve()`'s future is intentionally `!Send` (see its doc comment in
    // neuroos-storage), so it can't go through `tokio::spawn`; race it
    // against the client instead, both `.await`ed directly in this test.
    tokio::select! {
        _ = neuroos_storage::server::serve(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![my_uid]) => {
            panic!("storage.sock server exited unexpectedly");
        }
        _ = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let client = StorageClient::new(sock_path);

            // Spoke at t=3s, mid-session (the session spans 0..6s) -> resolves.
            let ctx = deictic::snap(&client, 3_000_000_000)
                .await
                .unwrap()
                .expect("a window should have been focused at t=3s");
            assert_eq!(ctx.app_id, "org.mozilla.firefox");
            assert_eq!(ctx.title, "quarterly revenue dashboard");

            // Spoke long after the session ended, outside the 1.5s window -> None,
            // not an error (rules.md §5.6: fail soft).
            let ctx2 = deictic::snap(&client, 20_000_000_000).await.unwrap();
            assert!(ctx2.is_none());
        } => {}
    }
}

/// KPI-2 (PRD.md §8, phases.md §8.3 AT): "Deictic snap accuracy >= 95%
/// correct window" over a fixture suite. 5 independent focus sessions (each
/// its own 100s time slot, far enough apart that no scenario's ±1.5s query
/// window can ever reach a neighbour's session) x 6 query points each,
/// covering the real selection rule C3 implements (contains-the-instant
/// first, then nearest-edge within the window, else `None` -- see
/// memory.md's tech debt entry on the PRD-vs-implementation "longest
/// overlap" wording mismatch, which this suite intentionally scores against
/// the *implemented*, already-accepted behaviour, not the PRD's original
/// wording).
#[tokio::test]
#[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
async fn deictic_snap_fixture_suite_meets_kpi_2() {
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

    const SLOT_NS: u64 = 100_000_000_000; // 100s between scenarios' time slots
    const DWELL_NS: u64 = 6_000_000_000; // 6s: safely over the >5s promotion threshold
    const SCENARIOS: u64 = 5;

    for i in 0..SCENARIOS {
        let toplevel_id = 100 + i;
        let base = (i + 1) * SLOT_NS; // +1 so scenario 0's "far_outside_before" never underflows into its own window
        let app_id = format!("app-{i}");
        engine
            .ingest(&window_event(
                toplevel_id,
                base,
                Kind::Opened(WindowOpened {
                    app_id: app_id.clone(),
                    title: format!("title-{i}"),
                    pid: 0,
                    pid_known: false,
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                toplevel_id,
                base,
                Kind::StateChanged(WindowStateChanged {
                    states: vec![ToplevelState::Activated as i32],
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                toplevel_id,
                base + DWELL_NS,
                Kind::StateChanged(WindowStateChanged { states: vec![] }),
            ))
            .await
            .unwrap();
    }

    let sock_dir = tempfile::tempdir().unwrap();
    let sock_path = sock_dir.path().join("storage.sock");
    let my_uid = current_uid();
    tokio::select! {
        _ = neuroos_storage::server::serve(Arc::new(Mutex::new(engine)), sock_path.clone(), vec![my_uid]) => {
            panic!("storage.sock server exited unexpectedly");
        }
        _ = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let client = StorageClient::new(sock_path);

            let mut total = 0u32;
            let mut correct = 0u32;
            for i in 0..SCENARIOS {
                let base = (i + 1) * SLOT_NS; // +1 so scenario 0's "far_outside_before" never underflows into its own window
                let expected_app_id = format!("app-{i}");
                let cases: [(&str, u64, Option<&str>); 6] = [
                    ("inside", base + 3_000_000_000, Some(expected_app_id.as_str())),
                    ("boundary_start", base, Some(expected_app_id.as_str())),
                    ("boundary_end", base + DWELL_NS, Some(expected_app_id.as_str())),
                    ("near_end_nearest_edge", base + DWELL_NS + 1_000_000_000, Some(expected_app_id.as_str())),
                    ("far_outside_after", base + DWELL_NS + 2_000_000_000, None),
                    ("far_outside_before", base.saturating_sub(5_000_000_000), None),
                ];
                for (case_name, t_ns, expected) in cases {
                    total += 1;
                    let ctx = deictic::snap(&client, t_ns).await.unwrap();
                    let got = ctx.as_ref().map(|c| c.app_id.as_str());
                    if got == expected {
                        correct += 1;
                    } else {
                        eprintln!(
                            "KPI-2 mismatch: scenario {i} case {case_name} t_ns={t_ns}: expected {expected:?}, got {got:?}"
                        );
                    }
                }
            }

            let accuracy = f64::from(correct) / f64::from(total);
            assert!(
                accuracy >= 0.95,
                "KPI-2: deictic snap accuracy must be >= 95%, measured {:.1}% ({correct}/{total})",
                accuracy * 100.0
            );
        } => {}
    }
}
