//! healthd core: scrape loop, aggregate model, cgroup reader, soak engine,
//! `healthd.sock` server. Split from `main.rs` so integration tests can
//! drive a real scrape cycle against real mock health servers (phases.md
//! §4.3 IT level) without needing a second process.
// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![deny(unsafe_code)]

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
/// The soak figure for one component: the worst p99 across its latency
/// histograms.
///
/// M4: this used to be `latency_histograms.values().next()` -- whichever
/// histogram a `HashMap` iterator happened to yield first, which differs
/// run to run and even cycle to cycle, so a component with more than one
/// histogram was compared against a baseline taken from a *different*
/// operation. The maximum is both deterministic and the figure a drift
/// alert should be about: the slowest thing the component does.
fn soak_p99_ns(
    histograms: &std::collections::HashMap<String, neuroos_proto::v1::LatencyHistogram>,
) -> u64 {
    histograms
        .values()
        .filter_map(neuroos_health::p99_ns)
        .max()
        .unwrap_or(0)
}

pub async fn scrape_cycle(
    targets: &[targets::Target],
    aggregate: &aggregate::Aggregate,
    soak_engine: Option<&soak::SoakEngine>,
    per_target_timeout: Duration,
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

        let soak_sample = soak_engine.map(|engine| {
            (
                engine,
                soak_p99_ns(&record.latency_histograms),
                record.name.clone(),
                record.rss_bytes,
            )
        });

        // M4: the baseline used to be captured on the *first cycle* for
        // every target at once, so a component that was down (or simply
        // not started) then got a baseline of zeros -- and
        // `check_breaches` never breaches against a zero baseline, so it
        // was excluded from soak monitoring for the rest of the run. It is
        // captured per component instead, on the first cycle that
        // component actually answers.
        let was_reached = aggregate.ever_reached(&record.name);
        aggregate.update(record);

        if let Some((engine, p99, name, rss_bytes)) = soak_sample {
            let reached_now = aggregate.ever_reached(&name);
            if reached_now && (!was_reached || engine.baseline_for(&name).is_none()) {
                engine.set_baseline(
                    &name,
                    soak::Baseline {
                        rss_bytes,
                        p99_ns: p99,
                    },
                );
            } else if reached_now {
                match engine.record(&name, rss_bytes, p99) {
                    Ok(breaches) => {
                        for b in breaches {
                            tracing::warn!(component = %b.component, kind = ?b.kind, pct = b.pct, "soak breach");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "soak CSV write failed"),
                }
            }
        }
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
    loop {
        interval.tick().await;
        // M4: no global "first cycle" flag any more -- `scrape_cycle`
        // captures each component's baseline on the first cycle that
        // component answers.
        scrape_cycle(
            &targets,
            &aggregate,
            soak_engine.as_deref(),
            per_target_timeout,
        )
        .await;
    }
}

/// H15 / Architecture.md §8.2's healthd row: cgroup files (read), its own
/// sockets in the runtime dir, and the soak CSV. Health targets' sockets
// C1: one safe implementation in neuroos-common, re-exported so the
// existing `neuroos_healthd::current_uid` call sites keep working.
pub use neuroos_common::current_uid;

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    /// M4: the soak p99 came from `latency_histograms.values().next()` --
    /// a `HashMap` iterator, so which operation it measured varied between
    /// cycles and a multi-histogram component drifted against a baseline
    /// taken from something else entirely.
    #[test]
    fn the_soak_p99_is_the_worst_histogram_not_an_arbitrary_one() {
        let mut histograms = std::collections::HashMap::new();
        let fast = neuroos_health::Histogram::new();
        fast.record(Duration::from_micros(100));
        let slow = neuroos_health::Histogram::new();
        slow.record(Duration::from_millis(250));
        histograms.insert("fast".to_string(), fast.to_proto());
        histograms.insert("slow".to_string(), slow.to_proto());

        let picked = soak_p99_ns(&histograms);
        // Deterministic across insertion orders, and it is the slow one.
        for _ in 0..20 {
            assert_eq!(soak_p99_ns(&histograms), picked);
        }
        assert!(
            picked >= Duration::from_millis(200).as_nanos() as u64,
            "{picked}"
        );
    }

    #[test]
    fn no_histograms_is_a_zero_p99_not_a_panic() {
        assert_eq!(soak_p99_ns(&std::collections::HashMap::new()), 0);
    }

    /// M4: a target that was unreachable on the first cycle got a baseline
    /// of zeros, and `check_breaches` never breaches against a zero
    /// baseline -- so it was silently excluded from soak monitoring for
    /// the whole run. The baseline is per component, captured on the first
    /// cycle it answers.
    #[tokio::test]
    async fn a_target_that_is_down_at_first_gets_its_baseline_when_it_comes_up() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("late.health.sock");
        let targets = vec![targets::Target {
            name: "late".into(),
            socket: sock.clone(),
            budget_bytes: 100 * 1024 * 1024,
            cgroup_path: None,
        }];
        let aggregate = aggregate::Aggregate::new(&targets);
        let engine = soak::SoakEngine::new(dir.path().join("soak.csv"));

        // Cycle 1: nothing is listening.
        scrape_cycle(
            &targets,
            &aggregate,
            Some(&engine),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(
            aggregate.snapshot()[0].status,
            neuroos_proto::v1::Status::Unknown
        );
        assert_eq!(
            engine.baseline_for("late"),
            None,
            "a baseline of zeros would exclude it from soak monitoring forever"
        );

        // It starts, reporting a real RSS.
        let health = neuroos_health::HealthServer::new("late v0");
        health.record_latency("op", Duration::from_millis(5));
        tokio::spawn(neuroos_health::HealthServer::serve(
            Arc::clone(&health),
            sock,
            vec![current_uid()],
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;

        scrape_cycle(
            &targets,
            &aggregate,
            Some(&engine),
            Duration::from_millis(500),
        )
        .await;
        let baseline = engine
            .baseline_for("late")
            .expect("the first answered cycle must capture the baseline");
        assert!(baseline.rss_bytes > 0, "{baseline:?}");
        assert!(baseline.p99_ns > 0, "{baseline:?}");
    }

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
