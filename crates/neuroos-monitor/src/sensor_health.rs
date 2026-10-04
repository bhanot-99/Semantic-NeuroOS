//! M3: which sensors are currently down, reported through C1's health
//! endpoint.
//!
//! Every sensor is a `run_forever` reconnect loop whose `run_once` returns
//! only when the sensor has stopped (a Wayland compositor restart, a lost
//! D-Bus connection, an inotify watch that went away). Before M3 those
//! stops were logged and nothing else: `neuroosctl status` showed C1 as OK
//! with no sensors running at all. The loop now tells this type when it
//! drops into backoff and when it starts again, so the component reports
//! `DEGRADED` for exactly as long as at least one sensor is down, and the
//! per-sensor error counters show which one and how often.
use std::collections::BTreeSet;
use std::fmt::Display;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use neuroos_health::HealthServer;
use neuroos_ipc::Backoff;
use neuroos_proto::v1::Status;
// `tokio::time::Instant` so `STABLE_RUN` is measured on the same clock the
// sleeps use, which `tokio::time::pause` can drive in tests (rules.md §7.6
// prefers a fake clock to sleeping for real).
use tokio::time::Instant;

/// Recovers rather than panics on a poisoned mutex (rules.md §5).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub struct SensorHealth {
    inner: Arc<Inner>,
}

struct Inner {
    health: Arc<HealthServer>,
    /// Sensor names currently in their reconnect backoff. A `BTreeSet` so
    /// the set is small, ordered and cheap to compare in tests.
    down: Mutex<BTreeSet<&'static str>>,
}

impl SensorHealth {
    pub fn new(health: Arc<HealthServer>) -> Self {
        Self {
            inner: Arc::new(Inner {
                health,
                down: Mutex::new(BTreeSet::new()),
            }),
        }
    }

    /// `sensor` has stopped and is about to wait out its backoff: count it
    /// and report C1 degraded until it is running again.
    pub fn report_stopped(&self, sensor: &'static str) {
        lock(&self.inner.down).insert(sensor);
        self.inner.health.report_degraded(&error_counter(sensor));
    }

    /// `sensor` is (re)entering its run loop. C1 returns to `OK` once no
    /// sensor is left in backoff.
    pub fn report_started(&self, sensor: &'static str) {
        let still_down = {
            let mut down = lock(&self.inner.down);
            down.remove(sensor);
            !down.is_empty()
        };
        self.inner.health.set_status(if still_down {
            Status::Degraded
        } else {
            Status::Ok
        });
    }

    /// A sensor that is configured off (no watch folders, say) is not a
    /// failure, so it never enters the loop and never appears here.
    #[cfg(test)]
    fn down(&self) -> BTreeSet<&'static str> {
        lock(&self.inner.down).clone()
    }
}

/// `error_counters` key for `sensor`. A stable identifier, never user
/// content (rules.md R0-6).
fn error_counter(sensor: &str) -> String {
    format!("sensor_{sensor}_stopped")
}

/// How long one attempt must have lasted for the sensor to count as having
/// genuinely run, rather than as a continuation of the same outage. A
/// failure after that long resets the backoff (M5: every sensor's
/// `backoff_ms` grew to its 10 s cap on the first few restarts and stayed
/// there for the life of the process, so a compositor restart hours later
/// cost a 10 s reconnect for no reason).
const STABLE_RUN: Duration = Duration::from_secs(30);

/// Runs `attempt` forever, reconnecting with jittered backoff and keeping
/// `health` in step with whether the sensor is up (M3) -- the shape every
/// C1 sensor's `run_forever` had copy-pasted, minus the health reporting
/// and the backoff reset.
///
/// `attempt` resolves only when the sensor has stopped; `Ok(())` is a
/// clean stop (a closed bus or channel) and is retried exactly like an
/// error, which is also why no `run_forever` needs `unreachable!()` any
/// more (rules.md §5.1).
pub async fn supervise<F, Fut, E>(sensor: &'static str, health: SensorHealth, mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: Display,
{
    let mut backoff = Backoff::new();
    loop {
        health.report_started(sensor);
        let started = Instant::now();
        match attempt().await {
            Ok(()) => tracing::warn!(sensor, "sensor stopped; reconnecting"),
            Err(e) => tracing::warn!(sensor, error = %e, "sensor stopped; reconnecting"),
        }
        if started.elapsed() >= STABLE_RUN {
            backoff.reset();
        }
        health.report_stopped(sensor);
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn fixture() -> (Arc<HealthServer>, SensorHealth) {
        let health = HealthServer::new("neuroos-monitor test");
        (Arc::clone(&health), SensorHealth::new(health))
    }

    #[test]
    fn a_stopped_sensor_degrades_the_component_and_counts() {
        let (health, sensors) = fixture();
        sensors.report_stopped("mpris");
        let snap = health.snapshot();
        assert_eq!(snap.status, Status::Degraded as i32);
        assert_eq!(snap.error_counters.get("sensor_mpris_stopped"), Some(&1));
    }

    #[test]
    fn the_component_recovers_only_once_every_sensor_is_back() {
        let (health, sensors) = fixture();
        sensors.report_stopped("mpris");
        sensors.report_stopped("idle");
        sensors.report_started("mpris");
        assert_eq!(
            health.snapshot().status,
            Status::Degraded as i32,
            "idle is still down"
        );
        sensors.report_started("idle");
        assert_eq!(health.snapshot().status, Status::Ok as i32);
        assert!(sensors.down().is_empty());
    }

    #[test]
    fn repeated_stops_of_one_sensor_accumulate_in_its_counter() {
        let (health, sensors) = fixture();
        for _ in 0..3 {
            sensors.report_stopped("wayland_cosmic");
            sensors.report_started("wayland_cosmic");
        }
        let snap = health.snapshot();
        assert_eq!(
            snap.error_counters.get("sensor_wayland_cosmic_stopped"),
            Some(&3)
        );
        assert_eq!(snap.status, Status::Ok as i32, "it came back each time");
    }

    /// M5: the backoff used to double to its 10 s cap and stay there for
    /// the life of the process, so a sensor that had been running happily
    /// for hours still waited the full 10 s to reconnect.
    #[tokio::test(start_paused = true)]
    async fn a_sensor_that_ran_a_long_time_reconnects_promptly_again() {
        let (_health, sensors) = fixture();
        // Each attempt records when it started and how long it then ran,
        // so the gap the supervisor waited between two attempts is
        // `start[n] - start[n-1] - ran[n-1]`.
        let log: Arc<Mutex<Vec<(Instant, Duration)>>> = Arc::new(Mutex::new(Vec::new()));
        let run_for = Arc::new(Mutex::new(Duration::ZERO));

        let attempts = Arc::clone(&log);
        let duration = Arc::clone(&run_for);
        let task = tokio::spawn(async move {
            supervise("test", sensors, move || {
                let attempts = Arc::clone(&attempts);
                let duration = Arc::clone(&duration);
                async move {
                    let run = *lock(&duration);
                    lock(&attempts).push((Instant::now(), run));
                    tokio::time::sleep(run).await;
                    Err::<(), &str>("stopped")
                }
            })
            .await;
        });

        // Instant failures in a row: the backoff climbs to its cap.
        tokio::time::sleep(Duration::from_secs(120)).await;
        let early = gaps(&lock(&log));
        assert!(
            early.len() >= 5,
            "expected several quick retries: {early:?}"
        );
        assert!(
            early.last().copied().unwrap_or_default() > Backoff::INITIAL * 8,
            "backoff should have climbed: {early:?}"
        );

        // Now one attempt stays up well past STABLE_RUN before failing.
        *lock(&run_for) = STABLE_RUN + Duration::from_secs(1);
        let before = lock(&log).len();
        tokio::time::sleep(STABLE_RUN * 2).await;
        let after = gaps(&lock(&log));
        // `before` attempts were already logged, so the long-running one is
        // attempt index `before` and `after[before]` is the wait that
        // followed it -- the one the reset should have shortened.
        assert!(
            after.len() > before,
            "the stable run must have ended and been retried"
        );
        assert!(
            after[before] <= Backoff::INITIAL,
            "backoff did not reset after a stable run: {:?}",
            after[before]
        );
        task.abort();
    }

    /// The wait the supervisor inserted before each attempt after the first.
    fn gaps(log: &[(Instant, Duration)]) -> Vec<Duration> {
        log.windows(2)
            .map(|w| w[1].0.duration_since(w[0].0).saturating_sub(w[0].1))
            .collect()
    }

    /// M3: while the supervised sensor is down the component is DEGRADED,
    /// and it is OK again while the sensor is running.
    #[tokio::test(start_paused = true)]
    async fn supervise_tracks_the_sensor_through_a_restart() {
        let (health, sensors) = fixture();
        let task = tokio::spawn(async move {
            supervise("test", sensors, || async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Err::<(), &str>("bus closed")
            })
            .await;
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            health.snapshot().status,
            Status::Ok as i32,
            "running: not degraded"
        );
        // Just after the attempt fails: the backoff is at least
        // `INITIAL / 2` (jitter only ever shortens), so the sensor is
        // reliably still down here.
        tokio::time::sleep(Duration::from_millis(501)).await;
        let snap = health.snapshot();
        assert_eq!(snap.status, Status::Degraded as i32, "in backoff: degraded");
        assert_eq!(snap.error_counters.get("sensor_test_stopped"), Some(&1));
        task.abort();
    }

    #[test]
    fn a_sensor_that_never_ran_leaves_the_component_ok() {
        let (health, _sensors) = fixture();
        assert_eq!(health.snapshot().status, Status::Ok as i32);
        assert!(health.snapshot().error_counters.is_empty());
    }
}
