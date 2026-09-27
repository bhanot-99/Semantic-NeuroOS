//! Mock `.health.sock` servers for healthd's IT tests (phases.md §4.3:
//! "healthd against 8 mock components ... healthy, slow, crashing,
//! returning malformed frames").
use std::path::Path;
use std::time::Duration;

use neuroos_health::HealthServer;
use tokio::io::AsyncWriteExt;

/// A normal, responsive health server.
pub fn spawn_healthy(sock_path: impl Into<std::path::PathBuf>, allowed_uid: u32, build_info: &str) {
    let health = HealthServer::new(build_info.to_string());
    let sock_path = sock_path.into();
    tokio::spawn(async move {
        let _ = health.serve(sock_path, vec![allowed_uid]).await;
    });
}

/// Accepts a connection but never responds — simulates a hung/overloaded peer.
pub fn spawn_slow(sock_path: impl AsRef<Path>, hang_for: Duration) {
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        if let Ok(listener) = tokio::net::UnixListener::bind(&sock_path)
            && let Ok((_stream, _)) = listener.accept().await
        {
            tokio::time::sleep(hang_for).await;
        }
    });
}

/// Accepts a connection, then immediately closes it without responding —
/// simulates a component that crashed right as healthd connected.
pub fn spawn_crashing(sock_path: impl AsRef<Path>) {
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        if let Ok(listener) = tokio::net::UnixListener::bind(&sock_path)
            && let Ok((stream, _)) = listener.accept().await
        {
            drop(stream);
        }
    });
}

/// Accepts a connection and sends a frame whose length prefix lies about
/// the payload size — healthd must treat this as DOWN, not panic.
pub fn spawn_malformed(sock_path: impl AsRef<Path>) {
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        if let Ok(listener) = tokio::net::UnixListener::bind(&sock_path)
            && let Ok((mut stream, _)) = listener.accept().await
        {
            let _ = stream.write_all(&1_000_000u32.to_le_bytes()).await;
            let _ = stream.write_all(b"short").await;
        }
    });
}
