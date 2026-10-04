//! UDS server: binds a filesystem-path socket and enforces `SO_PEERCRED` on accept.
use std::io;
use std::path::{Path, PathBuf};

use rustix::io::Errno;
use tokio::net::{UnixListener, UnixStream};

use crate::framing::DEFAULT_MAX_FRAME;
use crate::peercred::{PeerCred, is_allowed, peer_cred};

pub struct UdsServerConfig {
    pub path: PathBuf,
    /// UIDs permitted to connect to this socket (Architecture.md §5.2 socket map).
    pub allowed_uids: Vec<u32>,
    pub max_frame: u32,
}

impl UdsServerConfig {
    pub fn new(path: impl Into<PathBuf>, allowed_uids: Vec<u32>) -> Self {
        Self {
            path: path.into(),
            allowed_uids,
            max_frame: DEFAULT_MAX_FRAME,
        }
    }
}

pub struct UdsServer {
    listener: UnixListener,
    allowed_uids: Vec<u32>,
    pub max_frame: u32,
}

impl UdsServer {
    /// Binds the socket, replacing a stale socket file left by a crashed
    /// previous instance (Architecture.md §5.1: filesystem paths, not the
    /// abstract namespace).
    pub fn bind(config: UdsServerConfig) -> io::Result<Self> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::remove_file(&config.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&config.path)?;
        Ok(Self {
            listener,
            allowed_uids: config.allowed_uids,
            max_frame: config.max_frame,
        })
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
    pub async fn accept(&self) -> io::Result<Option<(UnixStream, PeerCred)>> {
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
            Ok(Some((stream, cred)))
        } else {
            tracing::warn!(uid = cred.uid, "rejected peer: uid not in allowlist");
            Ok(None)
        }
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
}
