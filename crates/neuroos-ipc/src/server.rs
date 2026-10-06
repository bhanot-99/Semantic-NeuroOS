//! UDS server: binds a filesystem-path socket and enforces `SO_PEERCRED` on accept.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustix::fs::Mode;
use rustix::io::Errno;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::framing::DEFAULT_MAX_FRAME;
use crate::peercred::{PeerCred, is_allowed, peer_cred};

/// L2: concurrent connections one socket will hold open. Every server in
/// the system spawns a task per accepted connection and none of them had
/// any ceiling, so a peer in the UID allowlist could open connections
/// until the process ran out of fds (and, for C3, until the per-task
/// stacks ate the component's RSS budget). The real fan-in on the busiest
/// socket is single digits -- `storage.sock` has four callers (C5a, C5b,
/// C6, `neuroosctl`), each opening one connection per request -- so this
/// is generous headroom and not a throughput limit.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;

/// L2: how long an accepted connection may sit without sending its next
/// request before the server closes it. Every socket in Architecture.md
/// §5.2 is request/response, and every client either reconnects (AB-10's
/// `connect_retrying`) or opens a fresh connection per call, so closing
/// an idle one costs a reconnect at most. Comfortably longer than
/// healthd's scrape period, the longest legitimate gap between two
/// requests on one connection.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// L16: `0700` on the parent runtime directory and `0600` on the socket
/// file. Both used to be created with whatever the service's umask was,
/// so a `0022` umask left `storage.sock` world-readable -- harmless on
/// its own (`SO_PEERCRED` still rejects a foreign UID, and
/// Architecture.md §5.1 uses filesystem paths precisely so the
/// permissions apply) but it is the only layer below the UID allowlist,
/// and defence in depth is the point of having it at all.
const RUNTIME_DIR_MODE: Mode = Mode::RWXU;
const SOCKET_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

pub struct UdsServerConfig {
    pub path: PathBuf,
    /// UIDs permitted to connect to this socket (Architecture.md §5.2 socket map).
    pub allowed_uids: Vec<u32>,
    pub max_frame: u32,
    /// L2: see [`DEFAULT_MAX_CONNECTIONS`].
    pub max_connections: usize,
    /// L2: see [`DEFAULT_IDLE_TIMEOUT`].
    pub idle_timeout: Duration,
}

impl UdsServerConfig {
    pub fn new(path: impl Into<PathBuf>, allowed_uids: Vec<u32>) -> Self {
        Self {
            path: path.into(),
            allowed_uids,
            max_frame: DEFAULT_MAX_FRAME,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }
}

/// L2: proof that an accepted connection is counted against the server's
/// [`UdsServerConfig::max_connections`] ceiling. The server hands one out
/// with every connection; hold it for as long as the connection lives
/// (move it into the task that serves the connection) and the slot is
/// released when it drops.
#[derive(Debug)]
pub struct ConnectionPermit(#[allow(dead_code)] OwnedSemaphorePermit);

pub struct UdsServer {
    listener: UnixListener,
    allowed_uids: Vec<u32>,
    pub max_frame: u32,
    /// L2: one permit per live connection; `accept` waits here when the
    /// server is already at `max_connections`, which leaves the surplus
    /// peers in the kernel's listen backlog instead of spawning a task
    /// and an fd for each of them.
    connections: Arc<Semaphore>,
    idle_timeout: Duration,
}

impl UdsServer {
    /// Binds the socket, replacing a stale socket file left by a crashed
    /// previous instance (Architecture.md §5.1: filesystem paths, not the
    /// abstract namespace).
    /// L16: the parent directory is created `0700` and the socket file is
    /// chmod'ed to `0600`, regardless of the service's umask.
    pub fn bind(config: UdsServerConfig) -> io::Result<Self> {
        if let Some(parent) = config.path.parent() {
            create_dir_all_private(parent)?;
        }
        match std::fs::remove_file(&config.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&config.path)?;
        // After `bind`, not before: the socket file does not exist until
        // then, and a umask-dependent window is exactly what this closes.
        // A `UnixListener` has no `fchmod` path, so the file is named.
        rustix::fs::chmod(&config.path, SOCKET_MODE)?;
        Ok(Self {
            listener,
            allowed_uids: config.allowed_uids,
            max_frame: config.max_frame,
            connections: Arc::new(Semaphore::new(config.max_connections)),
            idle_timeout: config.idle_timeout,
        })
    }

    /// L2: the per-connection read deadline a serving loop should pass to
    /// [`crate::read_envelope_deadline`] when waiting for the next request
    /// on an already-accepted connection.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub fn local_path(&self) -> Option<PathBuf> {
        self.listener
            .local_addr()
            .ok()?
            .as_pathname()
            .map(Path::to_path_buf)
    }

    /// Accepts one connection. Returns `Ok(None)` (after closing the
    /// socket) for anything that concerns only this one connection -- a
    /// peer whose UID is not in the allowlist, a peer that vanished before
    /// `accept` returned, a momentary fd or memory shortage -- so callers
    /// can loop and keep serving the next peer. `Err` means the listening
    /// socket itself is unusable and the loop should stop (M2: every
    /// server used to die permanently on the first error of either kind).
    ///
    /// L2: waits for a free connection slot before accepting at all, and
    /// returns a [`ConnectionPermit`] the caller must keep alive for as
    /// long as it serves the connection.
    pub async fn accept(&self) -> io::Result<Option<(UnixStream, PeerCred, ConnectionPermit)>> {
        // `Semaphore` is never closed, so the only error variant cannot
        // occur; treating it as a broken listener keeps rules.md §5.1
        // (no `unwrap`) without inventing a second meaning for `Ok(None)`.
        let permit = Arc::clone(&self.connections)
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("connection limiter closed"))?;
        let (stream, _addr) = match self.listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) if is_per_connection_error(&e) => {
                tracing::warn!(error = %e, "accept failed for one connection; still serving");
                if is_resource_exhaustion(&e) {
                    // Retrying immediately would spin at full CPU for as
                    // long as the shortage lasts, which is the whole point
                    // of not treating it as fatal.
                    tokio::time::sleep(EXHAUSTION_BACKOFF).await;
                }
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        // A failed `SO_PEERCRED` lookup says nothing about the listening
        // socket, so it costs this connection and nothing more.
        let cred = match peer_cred(&stream) {
            Ok(cred) => cred,
            Err(e) => {
                tracing::warn!(error = %e, "could not read peer credentials; connection dropped");
                return Ok(None);
            }
        };
        if is_allowed(cred, &self.allowed_uids) {
            Ok(Some((stream, cred, ConnectionPermit(permit))))
        } else {
            tracing::warn!(uid = cred.uid, "rejected peer: uid not in allowlist");
            Ok(None)
        }
    }
}

/// L16: `std::fs::create_dir_all` applies the umask to every directory it
/// creates, so the mode is set explicitly. An existing directory is left
/// alone: the runtime dir is usually `$XDG_RUNTIME_DIR`, which systemd
/// already owns and created `0700`.
fn create_dir_all_private(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dir_all_private(parent)?;
    }
    match rustix::fs::mkdir(dir, RUNTIME_DIR_MODE) {
        Ok(()) => Ok(()),
        // Lost a race with another process, or (for the leading components
        // of an absolute path) it was there all along.
        Err(Errno::EXIST) if dir.is_dir() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// How long `accept` waits out an fd/memory shortage before returning
/// control to the caller's loop.
const EXHAUSTION_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// True for an `accept` error that affects only the connection being
/// accepted, not the listening socket: the loop must keep going. Everything
/// else (`EBADF`, `EINVAL`, `ENOTSOCK`, ...) means this listener is broken
/// for good and the caller should stop.
fn is_per_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionAborted   // ECONNABORTED: peer gave up first
            | io::ErrorKind::Interrupted   // EINTR
            | io::ErrorKind::WouldBlock    // EAGAIN on a spurious readiness wakeup
            | io::ErrorKind::PermissionDenied // EPERM from a packet filter
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::TimedOut
    ) || is_resource_exhaustion(e)
}

/// `EMFILE`/`ENFILE`/`ENOBUFS`/`ENOMEM`: no fds or memory for a new
/// connection right now. Transient, and the one case worth backing off for.
/// `io::Error` exposes the raw errno, so the `rustix` names are compared in
/// that form rather than converted back (rules.md §4.2: `rustix`, not `libc`).
fn is_resource_exhaustion(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(raw)
            if raw == Errno::MFILE.raw_os_error()
                || raw == Errno::NFILE.raw_os_error()
                || raw == Errno::NOBUFS.raw_os_error()
                || raw == Errno::NOMEM.raw_os_error()
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn os(errno: Errno) -> io::Error {
        io::Error::from_raw_os_error(errno.raw_os_error())
    }

    /// M2: before the fix every one of these propagated out of `accept` and
    /// ended the server's loop for the lifetime of the process.
    #[test]
    fn a_doomed_connection_does_not_doom_the_listener() {
        for code in [
            Errno::CONNABORTED,
            Errno::INTR,
            Errno::AGAIN,
            Errno::PERM,
            Errno::CONNRESET,
            Errno::MFILE,
            Errno::NFILE,
            Errno::NOBUFS,
            Errno::NOMEM,
        ] {
            assert!(
                is_per_connection_error(&os(code)),
                "errno {code:?} should not be fatal"
            );
        }
    }

    #[test]
    fn a_broken_listener_is_reported_as_fatal() {
        for code in [Errno::BADF, Errno::INVAL, Errno::NOTSOCK, Errno::FAULT] {
            assert!(
                !is_per_connection_error(&os(code)),
                "errno {code:?} should be fatal"
            );
        }
    }

    #[test]
    fn only_fd_and_memory_shortages_get_a_backoff() {
        assert!(is_resource_exhaustion(&os(Errno::MFILE)));
        assert!(!is_resource_exhaustion(&os(Errno::CONNABORTED)));
    }

    fn my_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// L16: before the fix both were created with whatever the service's
    /// umask happened to be, so a `0022` umask left the socket
    /// world-readable and the runtime dir world-traversable.
    #[tokio::test]
    async fn bind_creates_a_private_runtime_dir_and_socket() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join("neuroos");
        let sock = runtime_dir.join("test.sock");
        let _server = UdsServer::bind(UdsServerConfig::new(&sock, vec![my_uid()])).unwrap();

        let dir_mode = std::fs::metadata(&runtime_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "runtime dir mode {dir_mode:o}");
        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "socket mode {sock_mode:o}");
    }

    /// L16: a pre-existing runtime directory (the normal case --
    /// `$XDG_RUNTIME_DIR` is systemd's) is used as it is, not chmod'ed
    /// out from under its owner.
    #[tokio::test]
    async fn bind_leaves_an_existing_runtime_dir_alone() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join("preexisting");
        std::fs::create_dir(&runtime_dir).unwrap();
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o750)).unwrap();

        let sock = runtime_dir.join("test.sock");
        let _server = UdsServer::bind(UdsServerConfig::new(&sock, vec![my_uid()])).unwrap();

        let dir_mode = std::fs::metadata(&runtime_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o750, "runtime dir mode {dir_mode:o}");
    }

    /// L2: the ceiling is real -- with one slot, a second peer stays in
    /// the listen backlog until the first connection's permit drops, and
    /// is then served. Before the fix `accept` returned immediately for
    /// every peer and the server spawned a task and held an fd per
    /// connection with no bound at all.
    #[tokio::test]
    async fn accept_waits_for_a_free_connection_slot() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("capped.sock");
        let mut config = UdsServerConfig::new(&sock, vec![my_uid()]);
        config.max_connections = 1;
        let server = UdsServer::bind(config).unwrap();

        let _first_client = UnixStream::connect(&sock).await.unwrap();
        let _second_client = UnixStream::connect(&sock).await.unwrap();

        let (_stream, _cred, permit) = server.accept().await.unwrap().unwrap();

        // The one slot is taken, so the second peer cannot be accepted.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), server.accept())
                .await
                .is_err(),
            "accept must block while the server is at max_connections"
        );

        // Releasing the first connection's permit frees the slot.
        drop(permit);
        let accepted = tokio::time::timeout(Duration::from_secs(1), server.accept())
            .await
            .expect("accept must proceed once a slot frees")
            .unwrap();
        assert!(accepted.is_some());
    }

    /// L2: `idle_timeout` is what every serving loop passes to
    /// `read_envelope_deadline`, so it has to survive `bind`.
    #[tokio::test]
    async fn idle_timeout_defaults_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("idle.sock");
        let server = UdsServer::bind(UdsServerConfig::new(&sock, vec![my_uid()])).unwrap();
        assert_eq!(server.idle_timeout(), DEFAULT_IDLE_TIMEOUT);

        let sock2 = dir.path().join("idle2.sock");
        let mut config = UdsServerConfig::new(&sock2, vec![my_uid()]);
        config.idle_timeout = Duration::from_millis(5);
        let server = UdsServer::bind(config).unwrap();
        assert_eq!(server.idle_timeout(), Duration::from_millis(5));
    }
}
