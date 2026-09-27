//! cgroup v2 resource-usage reader (Architecture.md §6.5, PRD FR-HLT-03).
//! Reads a cgroup's own files directly rather than hardcoding a systemd
//! unit-to-cgroup-path convention, so it's testable against a plain
//! directory of fake cgroup files (see tests below) without a real cgroup
//! or systemd unit.
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CgroupUsage {
    pub memory_current_bytes: u64,
    pub memory_peak_bytes: u64,
}

/// Reads `memory.current` and `memory.peak` from `cgroup_dir`. Missing files
/// (e.g. `memory.peak` on an older kernel) are treated as `0`, not an error —
/// only a genuinely unreadable `memory.current` is fatal, since that's the
/// one figure P1-S02 actually needs.
pub fn read_usage(cgroup_dir: &Path) -> io::Result<CgroupUsage> {
    let memory_current_bytes = read_u64_file(&cgroup_dir.join("memory.current"))?;
    let memory_peak_bytes = read_u64_file(&cgroup_dir.join("memory.peak")).unwrap_or(0);
    Ok(CgroupUsage {
        memory_current_bytes,
        memory_peak_bytes,
    })
}

fn read_u64_file(path: &Path) -> io::Result<u64> {
    let text = std::fs::read_to_string(path)?;
    text.trim().parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not a u64", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn reads_current_and_peak() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "12345\n").unwrap();
        std::fs::write(dir.path().join("memory.peak"), "67890\n").unwrap();
        let usage = read_usage(dir.path()).unwrap();
        assert_eq!(usage.memory_current_bytes, 12345);
        assert_eq!(usage.memory_peak_bytes, 67890);
    }

    #[test]
    fn missing_peak_defaults_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "111\n").unwrap();
        let usage = read_usage(dir.path()).unwrap();
        assert_eq!(usage.memory_current_bytes, 111);
        assert_eq!(usage.memory_peak_bytes, 0);
    }

    #[test]
    fn missing_current_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_usage(dir.path()).is_err());
    }

    #[test]
    fn malformed_current_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "not-a-number\n").unwrap();
        assert!(read_usage(dir.path()).is_err());
    }
}
