//! Default health-check target registry: every component's `.health.sock`
//! path and memory budget, per Architecture.md §5.2 (socket map) and
//! PRD.md §6.2 (hard-cap RSS per component). `config.toml`'s
//! `[healthd].extra_targets` (`neuroos_common::HealthdConfig`) can add more
//! (e.g. for a component this registry doesn't yet know about); it cannot
//! override one of these by name — see `merge_targets`.
use std::path::PathBuf;

use neuroos_common::TargetConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub socket: PathBuf,
    pub budget_bytes: u64,
    /// cgroup v2 directory for `memory.current`/`memory.peak` (PRD
    /// FR-HLT-03). `None` until a real cgroup path is known — see the
    /// `cgroup_path` doc comment on `neuroos_common::TargetConfig`.
    pub cgroup_path: Option<PathBuf>,
}

/// Hard-cap RSS in MiB, PRD.md §6.2. `neuroos-fetcher` uses a separate
/// runtime dir (`/run/neuroos-fetcher/`, Architecture.md §7.1) since it
/// runs under its own dedicated UID, not the desktop user's.
const HARD_CAP_MIB: &[(&str, u64)] = &[
    ("neuroos-monitor", 40),
    ("neuroos-voice", 520),
    ("neuroos-storage", 300),
    ("neuroos-inference", 1900),
    ("neuroos-knowledge-query", 120),
    ("neuroos-knowledge-background", 120),
    ("neuroos-kernel", 64),
    ("neuroos-confirm", 64),
    ("neuroos-fetcher", 32),
];

pub fn default_targets() -> Vec<Target> {
    let runtime = neuroos_common::paths::runtime_dir();
    HARD_CAP_MIB
        .iter()
        .map(|&(name, mib)| {
            let socket = if name == "neuroos-fetcher" {
                PathBuf::from("/run/neuroos-fetcher/health.sock")
            } else {
                runtime.join(format!("{name}.health.sock"))
            };
            Target { name: name.to_string(), socket, budget_bytes: mib * 1024 * 1024, cgroup_path: None }
        })
        .collect()
}

/// Adds `extra` targets (from config) that aren't already in `base` by
/// name; a name collision keeps the built-in entry rather than letting
/// config silently redefine a known component's budget/socket.
pub fn merge_targets(mut base: Vec<Target>, extra: &[TargetConfig]) -> Vec<Target> {
    for t in extra {
        if base.iter().any(|b| b.name == t.name) {
            continue;
        }
        base.push(Target {
            name: t.name.clone(),
            socket: t.socket.clone(),
            budget_bytes: t.budget_bytes,
            cgroup_path: t.cgroup_path.clone(),
        });
    }
    base
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn default_targets_cover_every_component() {
        let targets = default_targets();
        assert_eq!(targets.len(), HARD_CAP_MIB.len());
        assert!(targets.iter().any(|t| t.name == "neuroos-inference" && t.budget_bytes == 1900 * 1024 * 1024));
    }

    #[test]
    fn fetcher_uses_its_own_runtime_dir() {
        let targets = default_targets();
        let fetcher = targets.iter().find(|t| t.name == "neuroos-fetcher").unwrap();
        assert_eq!(fetcher.socket, PathBuf::from("/run/neuroos-fetcher/health.sock"));
    }

    #[test]
    fn extra_targets_are_appended() {
        let base = default_targets();
        let extra = vec![TargetConfig {
            name: "neuroos-custom".into(),
            socket: PathBuf::from("/tmp/custom.sock"),
            budget_bytes: 1024, cgroup_path: None,
        }];
        let merged = merge_targets(base.clone(), &extra);
        assert_eq!(merged.len(), base.len() + 1);
        assert!(merged.iter().any(|t| t.name == "neuroos-custom"));
    }

    #[test]
    fn extra_target_cannot_override_a_known_name() {
        let base = default_targets();
        let extra =
            vec![TargetConfig { name: "neuroos-monitor".into(), socket: PathBuf::from("/tmp/evil.sock"), budget_bytes: 1, cgroup_path: None }];
        let merged = merge_targets(base.clone(), &extra);
        assert_eq!(merged.len(), base.len());
        let monitor = merged.iter().find(|t| t.name == "neuroos-monitor").unwrap();
        assert_ne!(monitor.socket, PathBuf::from("/tmp/evil.sock"));
    }
}
