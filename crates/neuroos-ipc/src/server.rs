//! UDS server: binds a filesystem-path socket and enforces `SO_PEERCRED` on accept.
use std::io;
use std::path::{Path, PathBuf};

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

    /// Accepts one connection. Returns `Ok(None)` (after closing the socket)
    /// if the peer's UID is not in the allowlist, so callers can loop and
    /// keep serving the next peer instead of treating this as fatal.
    pub async fn accept(&self) -> io::Result<Option<(UnixStream, PeerCred)>> {
        let (stream, _addr) = self.listener.accept().await?;
        let cred = peer_cred(&stream)?;
        if is_allowed(cred, &self.allowed_uids) {
            Ok(Some((stream, cred)))
        } else {
            tracing::warn!(uid = cred.uid, "rejected peer: uid not in allowlist");
            Ok(None)
        }
    }
}
