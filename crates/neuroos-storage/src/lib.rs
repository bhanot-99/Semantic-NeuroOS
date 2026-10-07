//! C3 storage core: ingest filter, domain adapters, embedding, LanceDB/SQLite,
//! query APIs, lifecycle jobs. Split from `main.rs` so tests can exercise
//! real logic without a second process (mirrors `neuroos-healthd`/
//! `neuroos-monitor`'s own lib/main split).
// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. The one exception in this crate is
// `Embedder::set_dylib_path` (std::env::set_var is unsafe in edition
// 2024), which carries its own scoped #[allow] and a SAFETY note.
#![cfg_attr(not(test), deny(unsafe_code))]

// C4: shared real-model test fixtures. Not compiled into the daemon.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub mod adapters;
pub mod embed;
pub mod engine;
pub mod ingest;
pub mod lance;
pub mod lifecycle;
pub mod monitor_feed;
pub mod server;
pub mod spool;
pub mod sqlite;

/// Process temp dir for LanceDB's spill files: inside `storage/`, so both
/// the Landlock sandbox and the data's privacy boundary cover it.
pub fn scratch_dir() -> std::path::PathBuf {
    neuroos_common::paths::storage_dir().join("tmp")
}

/// H15 / Architecture.md §8.2's C3 row: the embedding model and ONNX
/// Runtime (read), `storage/` and `backups/` (read-write), C7's spool
/// (read + remove only), and its own sockets in the runtime dir. Nothing
/// else under `$HOME` -- in particular not `~/.config/neuroos/hmac.key`.
pub fn sandbox_policy(config: &neuroos_common::config::StorageConfig) -> neuroos_sandbox::Policy {
    neuroos_sandbox::Policy::baseline()
        .read_only(config.models_dir.clone())
        .read_write(neuroos_common::paths::storage_dir())
        .read_write(neuroos_common::paths::backups_dir())
        .read_write(neuroos_common::paths::runtime_dir())
        .read_remove(config.spool_dir.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn sandbox_policy_matches_the_c3_row() {
        let config = neuroos_common::config::StorageConfig::default();
        let policy = sandbox_policy(&config);
        let storage = neuroos_common::paths::storage_dir();
        assert!(policy.allows_write_to(&storage.join("meta.sqlite3")));
        assert!(policy.allows_write_to(&neuroos_common::paths::backups_dir().join("x")));
        assert!(!policy.allows_write_to(&neuroos_common::paths::hmac_key_file()));
        assert!(!policy.allows_write_to(&config.models_dir.join("bge-small-en-v1.5")));
        assert!(!policy.allows_write_to(&config.spool_dir.join("doc.json")));
        assert_eq!(
            policy.read_remove_paths(),
            std::slice::from_ref(&config.spool_dir)
        );
        assert!(
            !policy
                .read_only_paths()
                .contains(&neuroos_common::paths::config_dir())
        );
    }
}
