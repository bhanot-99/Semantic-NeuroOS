//! UDS client with connect deadlines and reconnect-with-backoff.
use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::Instant;

/// Connects with a hard deadline; no retries.
pub async fn connect(path: &Path, deadline: Duration) -> io::Result<UnixStream> {
    match tokio::time::timeout(deadline, UnixStream::connect(path)).await {
        Ok(res) => res,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "connect deadline exceeded",
        )),
    }
}

#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Give up (return the last error) after this much wall-clock time.
    pub max_elapsed: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        // NFR-REL-01 / P0-S03: reconnect after server restart within 10 s.
        Self {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(2),
            max_elapsed: Duration::from_secs(10),
        }
    }
}

/// Retries connecting with exponential backoff until `policy.max_elapsed`
/// elapses, so callers survive a server restart transparently.
pub async fn connect_with_reconnect(
    path: &Path,
    policy: &ReconnectPolicy,
) -> io::Result<UnixStream> {
    let start = Instant::now();
    let mut backoff = policy.initial_backoff;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if start.elapsed() >= policy.max_elapsed {
                    return Err(e);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(policy.max_backoff);
            }
        }
    }
}
