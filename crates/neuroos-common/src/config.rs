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
    fn unknown_top_level_section_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[not_a_real_component]\nfoo = 1\n").unwrap();
        let err = load_config_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
