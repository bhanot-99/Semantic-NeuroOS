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

/// M20: the schedule itself is [`Backoff`]'s (AB-10: 100 ms doubling to a
/// 10 s cap, jittered), so this carries only how long the caller is
/// willing to keep trying. It used to hold its own schedule, capped at
/// 2 s with no jitter at all.
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Give up (return the last error) after this much wall-clock time.
    pub max_elapsed: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        // NFR-REL-01 / P0-S03: reconnect after server restart within 10 s.
        Self {
            max_elapsed: Duration::from_secs(10),
        }
    }
}

/// Retries connecting on AB-10's jittered schedule until
/// `policy.max_elapsed` elapses, so a caller that holds a long-lived
/// connection survives a server restart transparently.
pub async fn connect_with_reconnect(
    path: &Path,
    policy: &ReconnectPolicy,
) -> io::Result<UnixStream> {
    connect_retrying_inner(path, policy.max_elapsed, RetryWhat::Anything).await
}

/// AB-10 inside one request's own deadline: like [`connect`], but a server
/// that is *restarting* is retried on the jittered [`Backoff`] schedule
/// until `deadline` is spent, instead of failing the request outright.
///
/// M20: a socket file that does not exist is not a restart -- it is a
/// component that was never installed or started (C2, until Phase 6) --
/// and is reported immediately. Retrying it would spend the caller's
/// whole budget on every call, which on C5a's best-effort preamble path
/// (H9) means adding that budget to every single `ask`. This is the same
/// transient-versus-fatal split M2 made for `accept()`.
pub async fn connect_retrying(path: &Path, deadline: Duration) -> io::Result<UnixStream> {
    connect_retrying_inner(path, deadline, RetryWhat::TransientOnly).await
}

enum RetryWhat {
    /// Keep trying whatever the error, until the budget is spent: the
    /// caller is waiting for a server it knows should be there.
    Anything,
    /// Only errors that mean "the socket is there, nobody is listening
    /// right now".
    TransientOnly,
}

/// `ECONNREFUSED`: the socket file exists but has no listener -- a server
/// between `bind`s. `EAGAIN`/`ECONNABORTED`/`EINTR`: the listen backlog is
/// momentarily full, or the connect was interrupted. Anything else
/// (notably `ENOENT`) is reported as it is.
fn connect_error_is_transient(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    )
}

async fn connect_retrying_inner(
    path: &Path,
    budget: Duration,
    retry: RetryWhat,
) -> io::Result<UnixStream> {
    let start = Instant::now();
    let mut backoff = Backoff::new();
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if matches!(retry, RetryWhat::TransientOnly) && !connect_error_is_transient(&e) {
                    return Err(e);
                }
                let spent = start.elapsed();
                if spent >= budget {
                    return Err(e);
                }
                // Never sleep past the caller's own budget: the deadline
                // is the ceiling (rules.md §5.7), not the schedule.
                let delay = backoff.next_delay().min(budget - spent);
                tokio::time::sleep(delay).await;
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

    /// M20: a socket that does not exist is a service that is not
    /// installed or has never started (C2 through Phase 5), not one
    /// restarting. Retrying it burns the caller's whole deadline for
    /// nothing -- and C5a's preamble call is best-effort on exactly this
    /// path (H9), so it would have added its full budget to every ask.
    #[tokio::test]
    async fn a_missing_socket_fails_immediately_instead_of_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-existed.sock");
        let t = std::time::Instant::now();
        let err = connect_retrying(&missing, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "took {:?}; a missing socket must not be retried",
            t.elapsed()
        );
    }

    /// AB-10: a server that is restarting leaves its socket file behind,
    /// so connecting gets `ECONNREFUSED` -- that *is* worth retrying,
    /// within whatever deadline the caller gave.
    #[tokio::test]
    async fn a_restarting_server_is_retried_within_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("restarting.sock");
        // Bind and drop: the file stays, with nothing listening on it.
        drop(tokio::net::UnixListener::bind(&sock).unwrap());

        let late = sock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::remove_file(&late).ok();
            let listener = tokio::net::UnixListener::bind(&late).unwrap();
            let _ = listener.accept().await;
            // held open until the test ends
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        connect_retrying(&sock, Duration::from_secs(3))
            .await
            .expect("must reconnect once the server is back");
    }

    #[tokio::test]
    async fn a_server_that_never_comes_back_gives_up_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("gone.sock");
        drop(tokio::net::UnixListener::bind(&sock).unwrap());
        let t = std::time::Instant::now();
        assert!(
            connect_retrying(&sock, Duration::from_millis(400))
                .await
                .is_err()
        );
        let elapsed = t.elapsed();
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(3),
            "gave up after {elapsed:?}"
        );
    }

    /// M20: `ReconnectPolicy` capped its backoff at 2 s with no jitter,
    /// where AB-10 says 100 ms doubling to a 10 s cap, jittered. It now
    /// drives the same [`Backoff`] every other reconnecting caller uses.
    #[test]
    fn the_reconnect_policy_matches_ab_10s_schedule() {
        let policy = ReconnectPolicy::default();
        assert_eq!(policy.max_elapsed, Duration::from_secs(10));
    }
}

#[cfg(test)]
mod backoff_tests {
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
