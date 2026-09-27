//! Standard filesystem locations (Architecture.md §7.1). Every path here is
//! a function, not a constant: they depend on `$HOME`/`$XDG_*`/the real UID,
//! which differ between the reference machine and CI/tests.
use std::path::PathBuf;

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.config/neuroos/`
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("neuroos")
}

/// The config file path: `$NEUROOS_CONFIG` if set (rules.md §4: the only
/// other allowed env var besides `NEUROOS_LOG`), else `~/.config/neuroos/config.toml`.
pub fn config_file() -> PathBuf {
    std::env::var_os("NEUROOS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("config.toml"))
}

/// `~/.config/neuroos/hmac.key`
pub fn hmac_key_file() -> PathBuf {
    config_dir().join("hmac.key")
}

/// `~/.local/share/neuroos/` — all persistent user data.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join("neuroos")
}

/// `~/.local/share/neuroos/soak/`
pub fn soak_dir() -> PathBuf {
    data_dir().join("soak")
}

/// `~/.local/share/neuroos/backups/`
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

/// `~/.local/share/neuroos/storage/`
pub fn storage_dir() -> PathBuf {
    data_dir().join("storage")
}

/// `~/.local/share/neuroos/actions.jsonl`
pub fn actions_log_file() -> PathBuf {
    data_dir().join("actions.jsonl")
}

/// `$XDG_RUNTIME_DIR/neuroos/` = `/run/user/$UID/neuroos/` (Architecture.md
/// §5.2), mode 0700.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", current_uid())))
        .join("neuroos")
}

/// `<runtime_dir>/<component>.health.sock`
pub fn component_health_sock(component: &str) -> PathBuf {
    runtime_dir().join(format!("{component}.health.sock"))
}

/// `<runtime_dir>/healthd.sock`
pub fn healthd_sock() -> PathBuf {
    runtime_dir().join("healthd.sock")
}

/// `<runtime_dir>/monitor.sock` (Architecture.md §5.2: C1 server-push
/// telemetry stream, client C3).
pub fn monitor_sock() -> PathBuf {
    runtime_dir().join("monitor.sock")
}

/// `<runtime_dir>/monitor.control.sock` — request/response control channel
/// for `neuroosctl pause`/`resume`/`status` (FR-PRV-01), kept separate from
/// `monitor.sock`'s server-push event stream so that contract stays a pure
/// push pattern.
pub fn monitor_control_sock() -> PathBuf {
    runtime_dir().join("monitor.control.sock")
}

/// `<runtime_dir>/storage.sock` (Architecture.md §5.2: C3 request/response,
/// clients C5a/C5b/C6/`neuroosctl`).
pub fn storage_sock() -> PathBuf {
    runtime_dir().join("storage.sock")
}

/// `<runtime_dir>/inference.sock` (Architecture.md §5.2: C4 request/response
/// + fd passing, clients C5a/C2).
pub fn inference_sock() -> PathBuf {
    runtime_dir().join("inference.sock")
}

/// `<runtime_dir>/knowledge.sock` (Architecture.md §5.2: C5a request/response
/// + progress stream, clients C2/`neuroosctl ask`).
pub fn knowledge_sock() -> PathBuf {
    runtime_dir().join("knowledge.sock")
}

/// `<runtime_dir>/voice.sock` (Architecture.md §5.2: C2 request/response,
/// client C5a). C2 itself lands in Phase 6; Phase 5 talks to a mock server
/// bound at this same path.
pub fn voice_sock() -> PathBuf {
    runtime_dir().join("voice.sock")
}

/// `<runtime_dir>/kernel.sock` (Architecture.md §5.2: C6 request/response,
/// clients C5a/`neuroosctl`). C6 itself lands in Phase 7; Phase 5 talks to a
/// mock server bound at this same path.
pub fn kernel_sock() -> PathBuf {
    runtime_dir().join("kernel.sock")
}

/// `/opt/neuroos/models/` — read-only, never written by any component.
pub fn models_dir() -> PathBuf {
    PathBuf::from("/opt/neuroos/models")
}

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn config_file_respects_neuroos_config_override() {
        // SAFETY: single-threaded test process; no other thread reads env vars concurrently.
        unsafe {
            std::env::set_var("NEUROOS_CONFIG", "/tmp/neuroos-test-config.toml");
        }
        assert_eq!(
            config_file(),
            PathBuf::from("/tmp/neuroos-test-config.toml")
        );
        unsafe {
            std::env::remove_var("NEUROOS_CONFIG");
        }
    }

    #[test]
    fn component_health_sock_names_match_the_convention() {
        let p = component_health_sock("neuroos-monitor");
        assert_eq!(p.file_name().unwrap(), "neuroos-monitor.health.sock");
    }

    #[test]
    fn current_uid_matches_real_process_uid() {
        // SAFETY: getuid() takes no arguments and cannot fail.
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        assert_eq!(current_uid(), unsafe { getuid() });
    }
}
