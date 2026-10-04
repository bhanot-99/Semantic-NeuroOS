//! health endpoint server + latency histograms
//!
//! Any component gets a health endpoint by adding one line in `main`:
//! ```ignore
//! let health = neuroos_health::HealthServer::new("neuroos-monitor v0.1.0");
//! tokio::spawn(health.clone().serve(socket_path, vec![healthd_uid]));
//! ```
mod histogram;
mod percentile;
mod rss;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// See `histogram::lock`'s doc comment: recovers rather than panics on a
/// poisoned mutex (rules.md §5).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub use histogram::Histogram;
pub use percentile::{p50_ns, p99_ns, percentile_ns};
pub use rss::rss_bytes;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{Envelope, HealthResponse, Status, envelope};

pub struct HealthServer {
    start: Instant,
    build_info: String,
    status: AtomicI32,
    histograms: Mutex<HashMap<String, Arc<Histogram>>>,
    error_counters: Mutex<HashMap<String, u64>>,
}

impl HealthServer {
    pub fn new(build_info: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            build_info: build_info.into(),
            status: AtomicI32::new(Status::Ok as i32),
            histograms: Mutex::new(HashMap::new()),
            error_counters: Mutex::new(HashMap::new()),
        })
    }

    /// The status this component reports to healthd. M3 fixed the fact
    /// that nothing ever called this, so every endpoint always said OK:
    /// the convention across components is now
    ///
    /// * a failure that costs one request -> [`Self::incr_error`] only; one
    ///   bad request does not make the component sick, and healthd's own
    ///   scrape is what decides when the error *rate* matters;
    /// * a subsystem that has stopped working -- a sensor that lost its
    ///   bus, a feed that cannot reconnect -- -> `incr_error` plus
    ///   `set_status(Degraded)`, cleared back to `Ok` once it recovers;
    /// * a component that cannot serve at all -> `Status::Down` is never
    ///   self-reported: it is what healthd records when the scrape itself
    ///   fails, since a process that cannot answer cannot say so.
    pub fn set_status(&self, status: Status) {
        self.status.store(status as i32, Ordering::Relaxed);
    }

    pub fn status(&self) -> i32 {
        self.status.load(Ordering::Relaxed)
    }

    /// Counts one occurrence of a named failure class (`name` is a stable
    /// identifier, never user content -- rules.md R0-6).
    pub fn incr_error(&self, name: &str) {
        *lock(&self.error_counters)
            .entry(name.to_string())
            .or_insert(0) += 1;
    }

    /// Counts the failure *and* reports the component as degraded: for a
    /// subsystem that has stopped working, not a single failed request.
    pub fn report_degraded(&self, name: &str) {
        self.incr_error(name);
        self.set_status(Status::Degraded);
    }

    /// Records `duration` under a named latency histogram, creating it on first use.
    pub fn record_latency(&self, name: &str, duration: Duration) {
        let hist = lock(&self.histograms)
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Histogram::new()))
            .clone();
        hist.record(duration);
    }

    pub fn snapshot(&self) -> HealthResponse {
        let latency_histograms = lock(&self.histograms)
            .iter()
            .map(|(k, v)| (k.clone(), v.to_proto()))
            .collect();
        let error_counters = lock(&self.error_counters).clone();
        HealthResponse {
            status: self.status.load(Ordering::Relaxed),
            rss_bytes: rss_bytes().unwrap_or(0),
            uptime_s: self.start.elapsed().as_secs(),
            latency_histograms,
            error_counters,
            build_info: self.build_info.clone(),
        }
    }

    /// Binds `socket_path` and serves `HealthRequest` -> `HealthResponse` to
    /// any peer in `allowed_uids` (in practice, just healthd's UID) until an
    /// unrecoverable accept error occurs.
    pub async fn serve(
        self: Arc<Self>,
        socket_path: impl Into<PathBuf>,
        allowed_uids: Vec<u32>,
    ) -> io::Result<()> {
        let server = UdsServer::bind(UdsServerConfig::new(socket_path, allowed_uids))?;
        loop {
            let Some((mut stream, _cred)) = server.accept().await? else {
                continue; // rejected peer; keep serving
            };
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(e) = this.handle_connection(&mut stream).await {
                    tracing::debug!(error = %e, "health connection ended");
                }
            });
        }
    }

    async fn handle_connection<S>(&self, stream: &mut S) -> Result<(), neuroos_ipc::FramingError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        while let Some(req) = read_envelope(stream, DEFAULT_MAX_FRAME).await? {
            if !matches!(req.body, Some(envelope::Body::HealthRequest(_))) {
                continue;
            }
            let resp = Envelope {
                schema_version: 1,
                trace_id: req.trace_id,
                request_id: req.request_id,
                sent_at_ns: now_ns(),
                body: Some(envelope::Body::HealthResponse(self.snapshot())),
            };
            write_envelope(stream, &resp, DEFAULT_MAX_FRAME).await?;
        }
        Ok(())
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_ipc::connect;
    use neuroos_proto::v1::HealthRequest;

    /// M3: nothing in production called `set_status`/`incr_error`, so
    /// every component's endpoint reported OK no matter what. These are
    /// the two reporting shapes the components now use.
    #[test]
    fn one_failed_request_counts_without_making_the_component_sick() {
        let h = HealthServer::new("test v0");
        h.incr_error("query");
        let snap = h.snapshot();
        assert_eq!(snap.error_counters.get("query"), Some(&1));
        assert_eq!(snap.status, Status::Ok as i32);
    }

    #[test]
    fn a_stopped_subsystem_counts_and_reports_degraded() {
        let h = HealthServer::new("test v0");
        h.report_degraded("sensor_mpris");
        let snap = h.snapshot();
        assert_eq!(snap.error_counters.get("sensor_mpris"), Some(&1));
        assert_eq!(snap.status, Status::Degraded as i32);
        h.set_status(Status::Ok);
        assert_eq!(h.snapshot().status, Status::Ok as i32);
    }

    #[test]
    fn snapshot_reflects_recorded_data() {
        let h = HealthServer::new("test v0");
        h.record_latency("rpc", Duration::from_millis(5));
        h.incr_error("timeout");
        h.set_status(Status::Degraded);
        let snap = h.snapshot();
        assert_eq!(snap.status, Status::Degraded as i32);
        assert_eq!(snap.error_counters.get("timeout"), Some(&1));
        assert_eq!(snap.latency_histograms.get("rpc").unwrap().count, 1);
        assert_eq!(snap.build_info, "test v0");
    }

    #[tokio::test]
    async fn serves_health_request_over_real_uds_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("comp.health.sock");
        let my_uid = unsafe { libc_getuid() };

        let health = HealthServer::new("neuroos-test v0.1.0");
        health.record_latency("op", Duration::from_micros(500));
        let health_for_serve = health.clone();
        let sock_path_for_serve = sock_path.clone();
        tokio::spawn(async move {
            let _ = health_for_serve
                .serve(sock_path_for_serve, vec![my_uid])
                .await;
        });

        // give the server a moment to bind
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = connect(&sock_path, Duration::from_secs(1)).await.unwrap();
        let req = Envelope {
            schema_version: 1,
            trace_id: "t".into(),
            request_id: 1,
            sent_at_ns: now_ns(),
            body: Some(envelope::Body::HealthRequest(HealthRequest {})),
        };
        write_envelope(&mut client, &req, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let resp = read_envelope(&mut client, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        match resp.body {
            Some(envelope::Body::HealthResponse(hr)) => {
                assert_eq!(hr.status, Status::Ok as i32);
                assert_eq!(hr.build_info, "neuroos-test v0.1.0");
                assert!(hr.rss_bytes > 0);
                assert_eq!(hr.latency_histograms.get("op").unwrap().count, 1);
            }
            other => panic!("expected HealthResponse, got {other:?}"),
        }
    }

    /// # Safety
    /// `getuid()` takes no arguments and cannot fail.
    unsafe fn libc_getuid() -> u32 {
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }
}
