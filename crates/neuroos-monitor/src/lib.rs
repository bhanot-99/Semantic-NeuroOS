//! C1 monitor core: sensors, event bus, privacy layer, record/replay dump
//! format. Split from `main.rs` so integration tests can drive real sensor
//! and bus logic without needing a second process (mirrors
//! `neuroos-healthd`'s split).
// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![deny(unsafe_code)]

// C1: one safe implementation in neuroos-common, re-exported so the
// existing `crate::current_uid` / `neuroos_monitor::current_uid` call
// sites keep working.
pub use neuroos_common::current_uid;

pub mod anonymise;
pub mod bus;
pub mod control;
pub mod dump;
pub mod privacy;
pub mod sensor_health;
pub mod sensors;
pub mod stream;

/// H15 / Architecture.md §8.2's C1 row: `/proc` and the configured watch
/// folders (read), its own sockets in the runtime dir, and -- only when
/// recording -- the one dump file. Never `~/.local/share/neuroos`.
/// Wayland and D-Bus sockets are only connected to, which Landlock does
/// not restrict.
pub fn sandbox_policy(
    config: &neuroos_common::config::MonitorConfig,
    record: Option<&std::path::Path>,
) -> neuroos_sandbox::Policy {
    let mut policy =
        neuroos_sandbox::Policy::baseline().read_write(neuroos_common::paths::runtime_dir());
    for folder in &config.folders {
        policy = policy.read_only(folder.path.clone());
    }
    if let Some(path) = record {
        policy = policy.read_write(path.to_path_buf());
    }
    policy
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn sandbox_policy_reads_watch_folders_and_never_writes_user_data() {
        let config = neuroos_common::config::MonitorConfig {
            folders: vec![neuroos_common::config::FolderWatch {
                label: "notes".into(),
                path: "/home/u/Notes".into(),
            }],
            ..Default::default()
        };
        let policy = sandbox_policy(&config, None);
        assert!(policy.read_only_paths().contains(&"/home/u/Notes".into()));
        assert!(!policy.allows_write_to(std::path::Path::new("/home/u/Notes/todo.md")));
        assert!(policy.allows_write_to(&neuroos_common::paths::monitor_sock()));
        assert!(!policy.allows_write_to(&neuroos_common::paths::storage_dir()));

        let recording = sandbox_policy(&config, Some(std::path::Path::new("/tmp/dump.bin")));
        assert!(recording.allows_write_to(std::path::Path::new("/tmp/dump.bin")));
        assert!(!recording.allows_write_to(std::path::Path::new("/tmp/other")));
    }
}
