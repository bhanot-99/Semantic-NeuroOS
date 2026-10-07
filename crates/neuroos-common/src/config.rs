//! Strict config loading (`rules.md` §4: one `config.toml`, validated at
//! start, fail fast on unknown keys — every struct here is
//! `#[serde(deny_unknown_fields)]`). Each component adds its own section as
//! it's built. `[inference]` (Phase 2) is declared here even though no Rust
//! binary reads it yet, purely so this strict parser doesn't reject a
//! shared config.toml that also has an `[inference]` table for
//! `neuroos-inference` (C++, its own `toml11`-based reader) — see
//! `cpp/neuroos-inference/src/config.hpp`, which must be kept field-for-field
//! in sync with `InferenceConfig` below.
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub healthd: HealthdConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

/// `[inference]` (Phase 2, C++ `neuroos-inference`): see the module doc
/// comment above — no Rust code reads this yet, it exists only so the
/// shared config.toml's `[inference]` table doesn't fail this strict parser.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InferenceConfig {
    #[serde(default)]
    pub model_path: Option<PathBuf>,
    /// models/manifest.toml's sha256 for the model file (defense-in-depth
    /// beyond scripts/fetch-models.sh's own fetch-time verification; see
    /// cpp/neuroos-inference/src/config.hpp).
    #[serde(default)]
    pub model_sha256: Option<String>,
    #[serde(default = "default_inference_threads")]
    pub threads: u32,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            model_path: None,
            model_sha256: None,
            threads: default_inference_threads(),
            max_context_tokens: default_max_context_tokens(),
        }
    }
}

fn default_inference_threads() -> u32 {
    8
}

fn default_max_context_tokens() -> u32 {
    512
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HealthdConfig {
    #[serde(default = "default_poll_interval_s")]
    pub poll_interval_s: u64,
    #[serde(default = "default_per_target_timeout_s")]
    pub per_target_timeout_s: u64,
    /// Additional health-check targets beyond the built-in default registry
    /// (`crate::healthd_targets::default_targets`, defined by
    /// `neuroos-healthd` itself since it owns the component catalog).
    #[serde(default)]
    pub extra_targets: Vec<TargetConfig>,
}

impl Default for HealthdConfig {
    fn default() -> Self {
        Self {
            poll_interval_s: default_poll_interval_s(),
            per_target_timeout_s: default_per_target_timeout_s(),
            extra_targets: Vec::new(),
        }
    }
}

fn default_poll_interval_s() -> u64 {
    30
}

fn default_per_target_timeout_s() -> u64 {
    1
}

/// `[monitor]` (Phase 3, C1 `neuroos-monitor`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    /// FR-MON-02: `ext_idle_notify_v1` timeout before "idle" fires.
    #[serde(default = "default_idle_timeout_s")]
    pub idle_timeout_s: u32,
    /// FR-MON-04: system CPU/memory sampling period.
    #[serde(default = "default_resource_sample_interval_s")]
    pub resource_sample_interval_s: u32,
    /// FR-MON-08/FR-PRV-02: app_ids never emitted, regardless of pause
    /// state. Merged with (not replacing) `default_excluded_app_ids()`
    /// unless `exclude_defaults` is set false.
    #[serde(default)]
    pub excluded_app_ids: Vec<String>,
    /// Set false only to disable the built-in password-manager defaults
    /// (e.g. an isolated test fixture that wants an empty list).
    #[serde(default = "default_true")]
    pub exclude_defaults: bool,
    /// Event bus capacity (`RawTelemetryEvent`s buffered per subscriber
    /// before the oldest is dropped and the drop counter increments).
    #[serde(default = "default_bus_capacity")]
    pub bus_capacity: usize,
    /// FR-MON-09 (P1): folders watched for file activity, keyed by a label
    /// that becomes `FileActivityEvent.watch_label` (e.g. "git", "notes").
    #[serde(default)]
    pub folders: Vec<FolderWatch>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            idle_timeout_s: default_idle_timeout_s(),
            resource_sample_interval_s: default_resource_sample_interval_s(),
            excluded_app_ids: Vec::new(),
            exclude_defaults: default_true(),
            bus_capacity: default_bus_capacity(),
            folders: Vec::new(),
        }
    }
}

impl MonitorConfig {
    /// The full effective exclusion list: configured `excluded_app_ids`
    /// plus, unless disabled, the built-in defaults (FR-PRV-02: "Defaults
    /// include common password managers").
    pub fn effective_excluded_app_ids(&self) -> Vec<String> {
        let mut ids = self.excluded_app_ids.clone();
        if self.exclude_defaults {
            ids.extend(default_excluded_app_ids().iter().map(|s| s.to_string()));
        }
        ids
    }
}

/// Common Linux password-manager `app_id`s (desktop-entry / `WM_CLASS`
/// style ids), FR-PRV-02's "sane defaults". Not exhaustive — an operator
/// adds their own via `excluded_app_ids`.
pub fn default_excluded_app_ids() -> &'static [&'static str] {
    &[
        "org.keepassxc.KeePassXC",
        "com.bitwarden.desktop",
        "1password",
        "com.1password.1Password",
        "org.gnome.World.Secrets",
        "org.qtpass.QtPass",
        "keepass",
        "keepassx",
        "keepassxc",
    ]
}

fn default_idle_timeout_s() -> u32 {
    60
}

fn default_resource_sample_interval_s() -> u32 {
    5
}

fn default_bus_capacity() -> usize {
    4096
}

fn default_true() -> bool {
    true
}

/// `[storage]` (Phase 4/5, C3 `neuroos-storage`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Architecture.md §8.2's fixed Landlock read-only path for C3's
    /// embedding model + ONNX Runtime. Overridable for dev/test (e.g.
    /// `.dev-cache/models`, since `/opt/neuroos/models` needs root to
    /// populate and isn't present on this dev machine).
    #[serde(default = "default_models_dir")]
    pub models_dir: PathBuf,
    /// Where C7 drops fetched documents (Architecture.md §6.3/§7.1);
    /// C3 ingests and removes them.
    #[serde(default = "default_spool_dir")]
    pub spool_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            models_dir: default_models_dir(),
            spool_dir: default_spool_dir(),
        }
    }
}

fn default_spool_dir() -> PathBuf {
    PathBuf::from("/var/spool/neuroos-fetcher")
}

fn default_models_dir() -> PathBuf {
    PathBuf::from("/opt/neuroos/models")
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FolderWatch {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub name: String,
    pub socket: PathBuf,
    pub budget_bytes: u64,
    /// cgroup v2 directory to read `memory.current`/`memory.peak` from
    /// (PRD FR-HLT-03). Optional: the built-in default targets don't set
    /// this (the real systemd-unit-to-cgroup-path mapping depends on
    /// ADR-0002's user-unit rework landing; see memory.md tech debt), but
    /// a config override can supply it once a target's real cgroup is known.
    #[serde(default)]
    pub cgroup_path: Option<PathBuf>,
}

/// Loads from `crate::paths::config_file()` (`$NEUROOS_CONFIG` or
/// `~/.config/neuroos/config.toml`). A missing file is not an error — it
/// means "use every default" (reasonable before `scripts/install.sh` has
/// ever written one); a malformed one is.
pub fn load_config() -> Result<Config, ConfigError> {
    load_config_from(&crate::paths::config_file())
}

pub fn load_config_from(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = load_config_from(Path::new("/nonexistent/path/config.toml")).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.healthd.poll_interval_s, 30);
    }

    /// C7: `config/config.example.toml` used to document only `[monitor]`,
    /// while the parser accepted four sections. This keeps the example honest:
    /// `deny_unknown_fields` means any key the example gains that the structs
    /// do not have fails here, and the assertions below catch an example that
    /// drifts away from the documented defaults.
    #[test]
    fn the_example_config_parses_and_matches_the_documented_defaults() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/config.example.toml")
            .canonicalize()
            .expect("config/config.example.toml must exist");
        let cfg = load_config_from(&example).expect("the example config must parse");

        assert_eq!(cfg.healthd.poll_interval_s, 30);
        assert_eq!(cfg.healthd.per_target_timeout_s, 1);
        assert_eq!(cfg.inference.threads, 8);
        assert_eq!(cfg.inference.max_context_tokens, 512);
        assert_eq!(cfg.monitor.idle_timeout_s, 60);
        assert_eq!(cfg.monitor.bus_capacity, 4096);
        assert_eq!(cfg.storage.models_dir, default_models_dir());
        assert_eq!(cfg.storage.spool_dir, default_spool_dir());
    }

    #[test]
    fn parses_a_minimal_healthd_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[healthd]\npoll_interval_s = 10\n").unwrap();
        let cfg = load_config_from(&path).unwrap();
        assert_eq!(cfg.healthd.poll_interval_s, 10);
        assert_eq!(cfg.healthd.per_target_timeout_s, 1); // still default
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[healthd]\nnot_a_real_field = 1\n").unwrap();
        let err = load_config_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_an_inference_section_alongside_healthd() {
        // Proves the shared config.toml can carry both [healthd] (Rust) and
        // [inference] (C++, cpp/neuroos-inference/src/config.cpp) without
        // this strict parser rejecting it — see this module's doc comment.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[healthd]\npoll_interval_s = 10\n[inference]\nthreads = 4\nmodel_path = \"/tmp/model.gguf\"\n",
        )
        .unwrap();
        let cfg = load_config_from(&path).unwrap();
        assert_eq!(cfg.healthd.poll_interval_s, 10);
        assert_eq!(cfg.inference.threads, 4);
        assert_eq!(
            cfg.inference.model_path,
            Some(PathBuf::from("/tmp/model.gguf"))
        );
        assert_eq!(cfg.inference.max_context_tokens, 512); // still default
    }

    #[test]
    fn monitor_defaults_include_password_managers_and_merge_with_config() {
        let cfg = MonitorConfig {
            excluded_app_ids: vec!["org.mozilla.firefox".into()],
            ..MonitorConfig::default()
        };
        let effective = cfg.effective_excluded_app_ids();
        assert!(effective.contains(&"org.mozilla.firefox".to_string()));
        assert!(effective.contains(&"org.keepassxc.KeePassXC".to_string()));
    }

    #[test]
    fn monitor_exclude_defaults_false_drops_built_ins() {
        let cfg = MonitorConfig {
            exclude_defaults: false,
            ..MonitorConfig::default()
        };
        assert!(cfg.effective_excluded_app_ids().is_empty());
    }

    #[test]
    fn parses_a_monitor_section_with_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[monitor]\nidle_timeout_s = 30\n[[monitor.folders]]\nlabel = \"git\"\npath = \"/home/u/proj\"\n",
        )
        .unwrap();
        let cfg = load_config_from(&path).unwrap();
        assert_eq!(cfg.monitor.idle_timeout_s, 30);
        assert_eq!(cfg.monitor.folders.len(), 1);
        assert_eq!(cfg.monitor.folders[0].label, "git");
    }

    #[test]
    fn storage_models_dir_defaults_to_the_fixed_opt_path() {
        let cfg = Config::default();
        assert_eq!(cfg.storage.models_dir, PathBuf::from("/opt/neuroos/models"));
    }

    #[test]
    fn parses_a_storage_section_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[storage]\nmodels_dir = \"/tmp/dev-models\"\n").unwrap();
        let cfg = load_config_from(&path).unwrap();
        assert_eq!(cfg.storage.models_dir, PathBuf::from("/tmp/dev-models"));
    }

    #[test]
    fn unknown_top_level_section_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[not_a_real_component]\nfoo = 1\n").unwrap();
        let err = load_config_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
