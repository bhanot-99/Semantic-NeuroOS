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

/// AB-10's reconnect schedule: exponential from 100 ms, capped at 10 s,
/// each delay jittered down by up to half so clients that lost the same
/// server don't reconnect in lockstep. [`Backoff::reset`] after a
/// successful connection starts the next outage from 100 ms again.
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    pub const INITIAL: Duration = Duration::from_millis(100);
    pub const MAX: Duration = Duration::from_secs(10);

    pub fn new() -> Self {
        Self {
            next: Self::INITIAL,
        }
    }

    /// The delay to wait before the next attempt (then doubles, capped).
    pub fn next_delay(&mut self) -> Duration {
        let base = self.next;
        self.next = (self.next * 2).min(Self::MAX);
        let half = base / 2;
        // Not a secret, just decorrelation: the clock's sub-second noise.
        let noise = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()))
            .unwrap_or(0);
        let half_ns = half.as_nanos().max(1) as u64;
        base - Duration::from_nanos(noise % half_ns)
    }

    pub fn reset(&mut self) {
        self.next = Self::INITIAL;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn backoff_doubles_from_100ms_caps_at_10s_and_jitters_downward() {
        let mut b = Backoff::new();
        let mut base = Backoff::INITIAL;
        for _ in 0..12 {
            let d = b.next_delay();
            assert!(
                d <= base && d >= base / 2,
                "{d:?} outside [{:?}, {base:?}]",
                base / 2
            );
            base = (base * 2).min(Backoff::MAX);
        }
        assert_eq!(base, Backoff::MAX);
        b.reset();
        assert!(b.next_delay() <= Backoff::INITIAL);
    }
}
