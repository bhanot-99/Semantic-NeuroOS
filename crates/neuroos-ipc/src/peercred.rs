//! `SO_PEERCRED` peer authentication (Architecture.md §5.1): every server
//! checks the connecting peer's UID/GID against an allowlist for that socket.
use std::io;

use tokio::net::UnixStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<i32>,
}

pub fn peer_cred(stream: &UnixStream) -> io::Result<PeerCred> {
    let cred = stream.peer_cred()?;
    Ok(PeerCred {
        uid: cred.uid(),
        gid: cred.gid(),
        pid: cred.pid(),
    })
}

/// Pure allow/deny decision so the policy is unit-testable without a real
/// second UID on the machine (see `crates/neuroos-ipc` tests).
pub fn is_allowed(cred: PeerCred, allowed_uids: &[u32]) -> bool {
    allowed_uids.contains(&cred.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(uid: u32) -> PeerCred {
        PeerCred {
            uid,
            gid: uid,
            pid: Some(1),
        }
    }

    #[test]
    fn allowed_uid_passes() {
        assert!(is_allowed(cred(1000), &[1000, 1001]));
    }

    #[test]
    fn disallowed_uid_is_rejected() {
        assert!(!is_allowed(cred(1002), &[1000, 1001]));
    }

    #[test]
    fn empty_allowlist_rejects_everyone() {
        assert!(!is_allowed(cred(0), &[]));
    }
}
