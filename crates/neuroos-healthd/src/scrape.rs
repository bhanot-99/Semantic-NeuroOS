//! Scrapes one target's `.health.sock` (Architecture.md §6.5): connect,
//! send `HealthRequest`, read `HealthResponse`, all within one deadline.
//! Any failure (connect refused, timeout, malformed frame) becomes a DOWN
//! record — PRD FR-HLT-01/02: "marks unreachable ones DOWN".
use std::time::Duration;

use neuroos_ipc::{DEFAULT_MAX_FRAME, connect, read_envelope_deadline, write_envelope_deadline};
use neuroos_proto::v1::{Envelope, HealthRequest, envelope};

use crate::aggregate::ComponentRecord;
use crate::targets::Target;

pub async fn scrape_one(target: &Target, timeout: Duration) -> ComponentRecord {
    match scrape_one_inner(target, timeout).await {
        Ok(record) => apply_cgroup_usage(record, target).apply_budget_alert(),
        Err(_) => down_record(target),
    }
}

/// Overrides the self-reported `rss_bytes` with the cgroup's own
/// `memory.current` when a real cgroup path is known (PRD FR-HLT-03: this
/// is the authoritative figure, not what the component reports about
/// itself). Falls back to the self-reported value if the cgroup can't be
/// read (e.g. the unit isn't in the cgroup hierarchy this path assumes).
fn apply_cgroup_usage(mut record: ComponentRecord, target: &Target) -> ComponentRecord {
    let Some(cgroup_path) = &target.cgroup_path else {
        return record;
    };
    match crate::cgroup::read_usage(cgroup_path) {
        Ok(usage) => record.rss_bytes = usage.memory_current_bytes,
        Err(e) => {
            tracing::debug!(target = %target.name, error = %e, "cgroup read failed, using self-reported rss")
        }
    }
    record
}

fn down_record(target: &Target) -> ComponentRecord {
    ComponentRecord {
        name: target.name.clone(),
        status: neuroos_proto::v1::Status::Down,
        rss_bytes: 0,
        budget_bytes: target.budget_bytes,
        uptime_s: 0,
        latency_histograms: Default::default(),
        error_counters: Default::default(),
        last_error_at_ns: neuroos_common::now_ns(),
        build_info: String::new(),
    }
}

async fn scrape_one_inner(
    target: &Target,
    timeout: Duration,
) -> Result<ComponentRecord, ScrapeError> {
    let mut stream = connect(&target.socket, timeout)
        .await
        .map_err(ScrapeError::Connect)?;

    let request = Envelope {
        schema_version: 1,
        trace_id: String::new(),
        request_id: 0,
        sent_at_ns: neuroos_common::now_ns(),
        body: Some(envelope::Body::HealthRequest(HealthRequest {})),
    };
    write_envelope_deadline(&mut stream, &request, DEFAULT_MAX_FRAME, timeout)
        .await
        .map_err(ScrapeError::Write)?;

    let response = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, timeout)
        .await
        .map_err(ScrapeError::Read)?
        .ok_or(ScrapeError::ConnectionClosed)?;

    match response.body {
        Some(envelope::Body::HealthResponse(hr)) => Ok(ComponentRecord {
            name: target.name.clone(),
            status: neuroos_proto::v1::Status::try_from(hr.status)
                .unwrap_or(neuroos_proto::v1::Status::Unknown),
            rss_bytes: hr.rss_bytes,
            budget_bytes: target.budget_bytes,
            uptime_s: hr.uptime_s,
            latency_histograms: hr.latency_histograms,
            error_counters: hr.error_counters,
            last_error_at_ns: 0,
            build_info: hr.build_info,
        }),
        _ => Err(ScrapeError::UnexpectedResponse),
    }
}

#[derive(Debug, thiserror::Error)]
enum ScrapeError {
    #[error("connect failed: {0}")]
    Connect(std::io::Error),
    #[error("write failed: {0}")]
    Write(neuroos_ipc::FramingError),
    #[error("read failed: {0}")]
    Read(neuroos_ipc::FramingError),
    #[error("connection closed before a response arrived")]
    ConnectionClosed,
    #[error("target sent something other than a HealthResponse")]
    UnexpectedResponse,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::path::PathBuf;

    use neuroos_health::HealthServer;

    use super::*;

    fn target(name: &str, socket: PathBuf) -> Target {
        Target {
            name: name.into(),
            socket,
            budget_bytes: 100 * 1024 * 1024,
            cgroup_path: None,
        }
    }

    #[tokio::test]
    async fn cgroup_usage_overrides_self_reported_rss_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("cg.health.sock");
        let health = HealthServer::new("test v0");
        let my_uid = current_uid();
        tokio::spawn(health.serve(sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let cgroup_dir = dir.path().join("fake-cgroup");
        std::fs::create_dir_all(&cgroup_dir).unwrap();
        std::fs::write(cgroup_dir.join("memory.current"), "999999\n").unwrap();

        let mut t = target("cg", sock);
        t.cgroup_path = Some(cgroup_dir);
        let record = scrape_one(&t, Duration::from_secs(1)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Ok);
        assert_eq!(record.rss_bytes, 999999);
    }

    #[tokio::test]
    async fn missing_cgroup_falls_back_to_self_reported_rss() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("nocg.health.sock");
        let health = HealthServer::new("test v0");
        let my_uid = current_uid();
        tokio::spawn(health.serve(sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut t = target("nocg", sock);
        t.cgroup_path = Some(dir.path().join("does-not-exist"));
        let record = scrape_one(&t, Duration::from_secs(1)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Ok);
        // self-reported rss_bytes from HealthServer's own process, whatever it is (not the sentinel above)
        assert_ne!(record.rss_bytes, 999999);
    }

    #[tokio::test]
    async fn unreachable_target_is_down() {
        let t = target(
            "nope",
            PathBuf::from("/tmp/neuroos-healthd-test-nonexistent.sock"),
        );
        let record = scrape_one(&t, Duration::from_millis(200)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Down);
    }

    #[tokio::test]
    async fn healthy_target_reports_ok() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("healthy.health.sock");
        let health = HealthServer::new("test v0");
        let my_uid = current_uid();
        tokio::spawn(health.serve(sock.clone(), vec![my_uid]));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let t = target("healthy", sock);
        let record = scrape_one(&t, Duration::from_secs(1)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Ok);
        assert_eq!(record.build_info, "test v0");
    }

    #[tokio::test]
    async fn slow_target_times_out_as_down() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("slow.health.sock");

        // A listener that accepts but never responds, simulating a hung peer.
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let t = target("slow", sock);
        let record = scrape_one(&t, Duration::from_millis(100)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Down);
    }

    #[tokio::test]
    async fn malformed_frame_is_down_not_a_crash() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("malformed.health.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // a length prefix promising far more than we send: triggers
                // FramingError::Truncated on the healthd side, not a panic.
                let _ = stream.write_all(&1_000_000u32.to_le_bytes()).await;
                let _ = stream.write_all(b"short").await;
            }
        });

        let t = target("malformed", sock);
        let record = scrape_one(&t, Duration::from_millis(300)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Down);
    }

    #[tokio::test]
    async fn budget_breach_end_to_end_downgrades_ok_to_degraded() {
        use neuroos_ipc::{read_envelope, write_envelope};
        use neuroos_proto::v1::HealthResponse;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("budget.health.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await
                && let Ok(Some(req)) = read_envelope(&mut stream, DEFAULT_MAX_FRAME).await
            {
                let resp = Envelope {
                    schema_version: 1,
                    trace_id: req.trace_id,
                    request_id: req.request_id,
                    sent_at_ns: neuroos_common::now_ns(),
                    body: Some(envelope::Body::HealthResponse(HealthResponse {
                        status: neuroos_proto::v1::Status::Ok as i32,
                        // 95% of the 100 MiB budget below: past the 90%
                        // apply_budget_alert threshold.
                        rss_bytes: 95 * 1024 * 1024,
                        uptime_s: 1,
                        latency_histograms: Default::default(),
                        error_counters: Default::default(),
                        build_info: "budget-test".into(),
                    })),
                };
                let _ = write_envelope(&mut stream, &resp, DEFAULT_MAX_FRAME).await;
            }
        });

        let t = target("budget", sock); // budget_bytes: 100 MiB, see `target()` helper above
        let record = scrape_one(&t, Duration::from_secs(1)).await;
        assert_eq!(record.status, neuroos_proto::v1::Status::Degraded);
        assert_eq!(record.rss_bytes, 95 * 1024 * 1024);
    }

    fn current_uid() -> u32 {
        // SAFETY: getuid() takes no arguments and cannot fail.
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        unsafe { getuid() }
    }
}
