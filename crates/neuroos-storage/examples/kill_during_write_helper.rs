//! FI test helper (phases.md §7.3): opens a real `StorageEngine` at the
//! path given as argv[1] and ingests window-focus events in a tight loop
//! forever. Not a real service entry point -- `tests/fault_injection.rs`
//! spawns this as a child process and SIGKILLs it mid-write, then reopens
//! the same files in the parent to prove crash consistency.
#![allow(clippy::expect_used)] // rules.md §5 is for shipped services; this is a test-only helper meant to be killed
use std::path::PathBuf;

use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use neuroos_proto::v1::{RawTelemetryEvent, ToplevelState, WindowEvent, WindowOpened};
use neuroos_storage::engine::StorageEngine;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sqlite_path = PathBuf::from(&args[1]);
    let lance_dir = PathBuf::from(&args[2]);
    let models_dir = PathBuf::from(&args[3]);
    let onnxruntime_dylib = PathBuf::from(&args[4]);

    let mut engine = StorageEngine::open(&sqlite_path, &lance_dir, &models_dir, &onnxruntime_dylib)
        .await
        .expect("helper: real model + onnxruntime should load");

    let mut toplevel_id = 0u64;
    loop {
        toplevel_id += 1;
        let _ = engine
            .ingest(&RawTelemetryEvent {
                observed_at_ns: toplevel_id,
                source: "wayland_cosmic".into(),
                payload: Some(Payload::Window(WindowEvent {
                    toplevel_id,
                    kind: Some(Kind::Opened(WindowOpened {
                        app_id: format!("app-{toplevel_id}"),
                        title: format!("title-{toplevel_id}"),
                        pid: 0,
                        pid_known: false,
                    })),
                })),
            })
            .await;
        let _ = engine
            .ingest(&RawTelemetryEvent {
                observed_at_ns: toplevel_id,
                source: "wayland_cosmic".into(),
                payload: Some(Payload::Window(WindowEvent {
                    toplevel_id,
                    kind: Some(Kind::StateChanged(neuroos_proto::v1::WindowStateChanged {
                        states: vec![ToplevelState::Activated as i32],
                    })),
                })),
            })
            .await;
    }
}
