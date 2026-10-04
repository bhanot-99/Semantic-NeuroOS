//! Landlock filesystem sandbox (Architecture.md AP-1, §8.2; BUGS.md H15).
//!
//! Every Zone 2/3 service locks itself in right after loading its config
//! and before touching user data or opening sockets: from then on the
//! kernel lets it read only the paths it was given as read-only, and write
//! only the read-write ones, for the rest of its life (a Landlock domain
//! can only ever be narrowed, never lifted). systemd's unit hardening is a
//! second, independent layer on top of this; this one also holds when a
//! service is started by hand, from a test, or under a broken unit.
//!
//! **Fail closed** (rules.md §5.5): [`Policy::restrict_self`] errors unless
//! the kernel enforces the whole policy, and callers exit rather than run
//! unsandboxed. A path that doesn't exist is skipped, never fatal --
//! leaving it out only narrows access.
#![deny(unsafe_code)]
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};

/// Landlock ABI v5 (Linux 6.10): file truncation and device ioctls are
/// covered too. The reference kernel supports it; an older kernel makes
/// `restrict_self` fail closed instead of silently enforcing less.
const ABI_VERSION: ABI = ABI::V5;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("Landlock ruleset could not be built or applied: {0}")]
    Ruleset(#[from] landlock::RulesetError),
    #[error("Landlock is not fully enforced on this kernel ({0}); refusing to run unsandboxed")]
    NotEnforced(&'static str),
}

/// What one process may touch on the filesystem once it calls
/// [`Policy::restrict_self`]. Starts from [`Policy::baseline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    read_only: Vec<PathBuf>,
    read_write: Vec<PathBuf>,
    read_remove: Vec<PathBuf>,
}

impl Policy {
    /// The read-only paths every service needs regardless of its role:
    /// `/proc` (own RSS for the health endpoint, process trees for C1),
    /// CPU topology and cgroup limits (thread-pool sizing), the time zone
    /// database (local times on evidence), system shared libraries (ONNX
    /// Runtime and glibc load some lazily, after the sandbox is in place;
    /// none of them is user data), and `/dev/null`/`/dev/urandom`.
    pub fn baseline() -> Self {
        Self {
            read_only: [
                "/usr/lib",
                "/lib",
                "/lib64",
                "/etc/ld.so.cache",
                "/proc",
                "/sys/devices/system/cpu",
                "/sys/fs/cgroup",
                "/etc/localtime",
                "/usr/share/zoneinfo",
                "/dev/urandom",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
            read_write: vec![PathBuf::from("/dev/null")],
            read_remove: Vec::new(),
        }
    }

    /// Read (and execute) everything beneath `path`.
    pub fn read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only.push(path.into());
        self
    }

    /// Read, create, write, rename and delete beneath `path` (or, for a
    /// file, read/write/truncate that one file).
    pub fn read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_write.push(path.into());
        self
    }

    /// Read beneath `path` and delete files there, nothing else: C3's
    /// handling of C7's spool (ingest, then remove).
    pub fn read_remove(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_remove.push(path.into());
        self
    }

    pub fn read_only_paths(&self) -> &[PathBuf] {
        &self.read_only
    }

    pub fn read_write_paths(&self) -> &[PathBuf] {
        &self.read_write
    }

    pub fn read_remove_paths(&self) -> &[PathBuf] {
        &self.read_remove
    }

    /// Whether `path` is (beneath) a read-write path of this policy.
    pub fn allows_write_to(&self, path: &Path) -> bool {
        self.read_write.iter().any(|p| path.starts_with(p))
    }

    /// Restricts the calling thread -- and every thread and process it
    /// creates afterwards -- to this policy, permanently. Call it from
    /// `main` before spawning the async runtime's worker threads, or from
    /// the one thread that will do all the work.
    pub fn restrict_self(&self) -> Result<(), SandboxError> {
        let read = AccessFs::from_read(ABI_VERSION);
        let status = Ruleset::default()
            .handle_access(AccessFs::from_all(ABI_VERSION))?
            .create()?
            .add_rules(path_beneath_rules(&self.read_only, read))?
            .add_rules(path_beneath_rules(
                &self.read_write,
                AccessFs::from_all(ABI_VERSION),
            ))?
            .add_rules(path_beneath_rules(
                &self.read_remove,
                read | AccessFs::RemoveFile,
            ))?
            .restrict_self()?;
        match status.ruleset {
            RulesetStatus::FullyEnforced => {
                tracing::info!("Landlock sandbox enforced");
                Ok(())
            }
            RulesetStatus::PartiallyEnforced => Err(SandboxError::NotEnforced(
                "only partially, the kernel's Landlock ABI is older than v5",
            )),
            RulesetStatus::NotEnforced => Err(SandboxError::NotEnforced(
                "not at all, Landlock is unavailable or disabled",
            )),
        }
    }
}

/// Creates each read-write directory a service is about to be confined to
/// (Landlock can only grant paths that exist), then restricts the calling
/// thread. Services call this from `main` before building their async
/// runtime, so every worker thread inherits the restriction.
pub fn enter(policy: &Policy, create_dirs: &[PathBuf]) -> Result<(), SandboxError> {
    for dir in create_dirs {
        if let Err(e) = create_private_dir(dir) {
            tracing::warn!(path = %dir.display(), error = %e, "could not create a sandbox directory");
        }
    }
    policy.restrict_self()
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::io::ErrorKind;

    use super::*;

    /// Landlock restricts the calling thread only, so each test runs its
    /// sandboxed half on a fresh thread and leaves the harness unaffected.
    fn sandboxed<T: Send + 'static>(policy: Policy, f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::spawn(move || {
            policy.restrict_self().unwrap();
            f()
        })
        .join()
        .unwrap()
    }

    fn denied<T: std::fmt::Debug>(r: std::io::Result<T>) -> bool {
        matches!(r, Err(e) if e.kind() == ErrorKind::PermissionDenied)
    }

    #[test]
    fn reads_outside_the_policy_are_denied() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(allowed.path().join("a"), "ok").unwrap();
        std::fs::write(outside.path().join("secret"), "no").unwrap();
        let (a, secret) = (allowed.path().join("a"), outside.path().join("secret"));
        let policy = Policy::baseline().read_only(allowed.path());

        let (read_a, read_secret) = sandboxed(policy, move || {
            (std::fs::read_to_string(&a), std::fs::read(&secret))
        });

        assert_eq!(read_a.unwrap(), "ok");
        assert!(denied(read_secret));
    }

    #[test]
    fn read_only_paths_cannot_be_written_or_extended() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "v1").unwrap();
        let new_file = dir.path().join("new");
        let policy = Policy::baseline().read_only(dir.path());

        let (overwrite, create) = sandboxed(policy, move || {
            (std::fs::write(&file, "v2"), std::fs::write(&new_file, "x"))
        });

        assert!(denied(overwrite));
        assert!(denied(create));
    }

    #[test]
    fn read_write_paths_allow_create_write_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        let policy = Policy::baseline().read_write(dir.path());

        let result = sandboxed(policy, move || {
            std::fs::write(&file, "x")?;
            std::fs::read(&file)?;
            std::fs::remove_file(&file)
        });

        result.unwrap();
    }

    #[test]
    fn a_single_writable_file_does_not_open_up_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("graph_view.html");
        std::fs::write(&file, "old").unwrap();
        let sibling = dir.path().join("other");
        let policy = Policy::baseline().read_write(&file);

        let (rewrite, create_sibling) = sandboxed(policy, move || {
            (std::fs::write(&file, "new"), std::fs::write(&sibling, "x"))
        });

        rewrite.unwrap();
        assert!(denied(create_sibling));
    }

    #[test]
    fn read_remove_allows_deleting_but_not_writing() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("doc.json");
        std::fs::write(&doc, "{}").unwrap();
        let other = dir.path().join("other.json");
        let policy = Policy::baseline().read_remove(dir.path());

        let (read, write, remove) = sandboxed(policy, move || {
            (
                std::fs::read(&doc),
                std::fs::write(&other, "x"),
                std::fs::remove_file(&doc),
            )
        });

        read.unwrap();
        assert!(denied(write));
        remove.unwrap();
    }

    #[test]
    fn missing_paths_are_skipped_not_fatal() {
        let policy = Policy::baseline()
            .read_only("/nonexistent/neuroos/models")
            .read_write("/nonexistent/neuroos/runtime");
        sandboxed(policy, || ());
    }

    #[test]
    fn allows_write_to_checks_read_write_ancestry() {
        let policy = Policy::baseline().read_write("/run/user/1000/neuroos");
        assert!(policy.allows_write_to(Path::new("/run/user/1000/neuroos/storage.sock")));
        assert!(!policy.allows_write_to(Path::new("/run/user/1000")));
    }
}
