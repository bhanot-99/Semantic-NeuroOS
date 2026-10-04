//! healthd core: scrape loop, aggregate model, cgroup reader, soak engine,
//! `healthd.sock` server. Split from `main.rs` so integration tests can
//! drive a real scrape cycle against real mock health servers (phases.md
//! §4.3 IT level) without needing a second process.
pub mod aggregate;
pub mod cgroup;
pub mod scrape;
pub mod server;
pub mod soak;
pub mod targets;

use std::sync::Arc;
use std::time::Duration;

/// Runs one scrape cycle against every target, updating `aggregate` (and
/// `soak_engine`, if given) in place. `capture_baseline` should be `true`
/// only for the very first cycle.
pub async fn scrape_cycle(
    targets: &[targets::Target],
    aggregate: &aggregate::Aggregate,
    soak_engine: Option<&soak::SoakEngine>,
    per_target_timeout: Duration,
    capture_baseline: bool,
) {
    let handles: Vec<_> = targets
        .iter()
        .cloned()
        .map(|target| {
            tokio::spawn(async move { scrape::scrape_one(&target, per_target_timeout).await })
        })
        .collect();

    for handle in handles {
        let record = match handle.await {
            Ok(record) => record,
            Err(e) => {
                tracing::warn!(error = %e, "scrape task panicked");
                continue;
            }
        };

        if let Some(engine) = soak_engine {
            let p99 = record
                .latency_histograms
                .values()
                .next()
                .and_then(neuroos_health::p99_ns)
                .unwrap_or(0);
            if capture_baseline {
                engine.set_baseline(
                    &record.name,
                    soak::Baseline {
                        rss_bytes: record.rss_bytes,
                        p99_ns: p99,
                    },
                );
            } else {
                match engine.record(&record.name, record.rss_bytes, p99) {
                    Ok(breaches) => {
                        for b in breaches {
                            tracing::warn!(component = %b.component, kind = ?b.kind, pct = b.pct, "soak breach");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "soak CSV write failed"),
                }
            }
        }

        aggregate.update(record);
    }
}

/// Ticks `scrape_cycle` forever on `poll_interval`. Runs until the process
/// is killed — this is healthd's actual `main` loop.
pub async fn run_forever(
    targets: Vec<targets::Target>,
    aggregate: Arc<aggregate::Aggregate>,
    soak_engine: Option<Arc<soak::SoakEngine>>,
    poll_interval: Duration,
    per_target_timeout: Duration,
) {
    let mut interval = tokio::time::interval(poll_interval);
    let mut baseline_captured = false;
    loop {
        interval.tick().await;
        scrape_cycle(
            &targets,
            &aggregate,
            soak_engine.as_deref(),
            per_target_timeout,
            !baseline_captured,
        )
        .await;
        baseline_captured = true;
    }
}

/// H15 / Architecture.md §8.2's healthd row: cgroup files (read), its own
/// sockets in the runtime dir, and the soak CSV. Health targets' sockets
/// are only connected to, which Landlock does not restrict.
pub fn sandbox_policy(targets: &[targets::Target]) -> neuroos_sandbox::Policy {
    let mut policy = neuroos_sandbox::Policy::baseline()
        .read_write(neuroos_common::paths::runtime_dir())
        .read_write(neuroos_common::paths::soak_dir());
    for cgroup in targets.iter().filter_map(|t| t.cgroup_path.as_ref()) {
        policy = policy.read_only(cgroup.clone());
    }
    policy
}

pub fn current_uid() -> u32 {
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
    fn sandbox_policy_writes_only_the_runtime_and_soak_dirs() {
        let mut t = targets::default_targets();
        t[0].cgroup_path = Some("/sys/fs/cgroup/user.slice/x".into());
        let policy = sandbox_policy(&t);
        let paths = neuroos_common::paths::data_dir();
        assert!(
            policy.allows_write_to(&neuroos_common::paths::soak_dir().join("healthd-soak.csv"))
        );
        assert!(policy.allows_write_to(&neuroos_common::paths::healthd_sock()));
        assert!(!policy.allows_write_to(&paths.join("storage/meta.sqlite3")));
        assert!(
            policy
                .read_only_paths()
                .contains(&"/sys/fs/cgroup/user.slice/x".into())
        );
    }
}
