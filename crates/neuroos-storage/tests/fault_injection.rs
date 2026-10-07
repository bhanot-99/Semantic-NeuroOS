//! phases.md §7.3 FI: "Kill during write → restart → integrity check
//! passes (`PRAGMA integrity_check`, LanceDB open); ... corrupt spool file
//! rejected."
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
use neuroos_storage::test_support::{dev_models_dir, dev_onnxruntime_dylib};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use neuroos_storage::engine::StorageEngine;
use rusqlite::Connection;

fn helper_bin() -> PathBuf {
    // `cargo test` builds examples into the same target dir as the test
    // binary itself (`cargo build --example` isn't needed separately --
    // `cargo test` already builds every example once per profile).
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/examples/kill_during_write_helper")
}

/// Live proof, needs the real embedder model + onnxruntime fetched (`just
/// fetch-models`) and the helper example built (`cargo build --example
/// kill_during_write_helper -p neuroos-storage`, or just running `cargo
/// test` once, which builds every example first).
#[tokio::test]
#[ignore = "needs models fetched into .dev-cache/models and the kill_during_write_helper example built; see doc comment"]
async fn kill_during_write_then_restart_passes_integrity_check() {
    let bin = helper_bin();
    assert!(
        bin.exists(),
        "helper example not built -- run `cargo build --example kill_during_write_helper -p neuroos-storage` first: {}",
        bin.display()
    );

    let sqlite_dir = tempfile::tempdir().unwrap();
    let sqlite_path = sqlite_dir.path().join("meta.sqlite3");
    let lance_dir = tempfile::tempdir().unwrap();

    let mut child = Command::new(&bin)
        .arg(&sqlite_path)
        .arg(lance_dir.path())
        .arg(dev_models_dir())
        .arg(dev_onnxruntime_dylib())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn kill_during_write_helper");

    // Give it time to open the engine (real model load) and get into its
    // ingest loop -- long enough that a kill genuinely lands mid-write
    // (WAL mode, real SQLite + real LanceDB commits), not before anything
    // has touched disk.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // SIGKILL: no graceful shutdown, no chance to flush -- exactly the
    // "kill during write" this FI case is about.
    unsafe {
        libc_kill(child.id() as i32, 9);
    }
    let _ = child.wait();

    // Restart: reopen the same files fresh, as the real service would
    // after a crash + systemd restart.
    let integrity: String = {
        let conn = Connection::open(&sqlite_path).unwrap();
        conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        integrity, "ok",
        "PRAGMA integrity_check must pass after a kill mid-write"
    );

    // LanceDB open must also succeed (the other half of "integrity check
    // passes" -- a corrupted Lance table would fail to open, not just
    // silently miss rows).
    let engine = StorageEngine::open(
        &sqlite_path,
        lance_dir.path(),
        &dev_models_dir(),
        &dev_onnxruntime_dylib(),
    )
    .await;
    assert!(
        engine.is_ok(),
        "StorageEngine::open (incl. LanceDB) must succeed after a kill mid-write: {:?}",
        engine.err()
    );
}

// SAFETY: sending a signal to a PID we ourselves just spawned and own.
unsafe fn libc_kill(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid, sig);
    }
}
