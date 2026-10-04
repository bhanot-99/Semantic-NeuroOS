//! `healthd.sock`: serves `AggregateStatusRequest` -> `AggregateStatusResponse`
//! to `neuroosctl status` (Architecture.md §5.2).
use std::sync::Arc;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{AggregateStatusResponse, ComponentStatus, Envelope, envelope};

use crate::aggregate::Aggregate;

/// Binds `healthd.sock`. Separate from `serve_on` so a bind failure is
/// reported to `main` *before* the serving task is spawned: dropping the
/// spawned task's result used to hide it completely, leaving healthd
/// running with no socket for `neuroosctl status` to talk to (M2).
pub fn bind(
    socket_path: impl Into<std::path::PathBuf>,
    allowed_uids: Vec<u32>,
) -> std::io::Result<UdsServer> {
    UdsServer::bind(UdsServerConfig::new(socket_path, allowed_uids))
}

pub async fn serve(
    aggregate: Arc<Aggregate>,
    socket_path: impl Into<std::path::PathBuf>,
    allowed_uids: Vec<u32>,
) -> std::io::Result<()> {
    serve_on(bind(socket_path, allowed_uids)?, aggregate).await
}

/// Serves an already-bound socket until its listener becomes unusable.
pub async fn serve_on(server: UdsServer, aggregate: Arc<Aggregate>) -> std::io::Result<()> {
    loop {
        let Some((mut stream, _cred)) = server.accept().await? else {
            continue;
        };
        let aggregate = aggregate.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(&mut stream, &aggregate).await {
                tracing::debug!(error = %e, "healthd.sock connection ended");
            }
        });
    }
}

async fn handle_connection<S>(
    stream: &mut S,
    aggregate: &Aggregate,
) -> Result<(), neuroos_ipc::FramingError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(req) = read_envelope(stream, DEFAULT_MAX_FRAME).await? {
        if !matches!(req.body, Some(envelope::Body::AggregateStatusRequest(_))) {
            continue;
        }
        let components = aggregate
            .snapshot()
            .into_iter()
            .map(|r| ComponentStatus {
                name: r.name,
                status: r.status as i32,
                rss_bytes: r.rss_bytes,
                budget_bytes: r.budget_bytes,
                uptime_s: r.uptime_s,
                latency_histograms: r.latency_histograms,
                error_counters: r.error_counters,
                last_error_at_ns: r.last_error_at_ns,
                build_info: r.build_info,
            })
            .collect();
        let resp = Envelope {
            schema_version: 1,
            trace_id: req.trace_id,
            request_id: req.request_id,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::AggregateStatusResponse(
                AggregateStatusResponse {
                    components,
                    generated_at_ns: neuroos_common::now_ns(),
                },
            )),
        };
        write_envelope(stream, &resp, DEFAULT_MAX_FRAME).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    /// M2: `main` used to `tokio::spawn(serve(..))` and drop the handle, so
    /// a bind failure was invisible and healthd kept running with no
    /// socket. `bind` is now a separate, reportable step.
    #[test]
    fn a_bind_failure_is_reported_rather_than_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        // The socket's parent path is an existing *file*, so
        // `create_dir_all` on it cannot succeed.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"").unwrap();
        let err = match bind(blocker.join("healthd.sock"), vec![0]) {
            Ok(_) => panic!("expected the bind to fail"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::NotADirectory | std::io::ErrorKind::AlreadyExists
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_bound_socket_serves_an_aggregate_status_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("healthd.sock");
        let aggregate = Arc::new(Aggregate::new(&[]));
        let server = bind(&path, vec![crate::current_uid()]).unwrap();
        tokio::spawn(serve_on(server, aggregate));

        let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let req = Envelope {
            schema_version: 1,
            trace_id: String::new(),
            request_id: 7,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::AggregateStatusRequest(
                neuroos_proto::v1::AggregateStatusRequest {},
            )),
        };
        write_envelope(&mut stream, &req, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let resp = read_envelope(&mut stream, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resp.request_id, 7);
        assert!(matches!(
            resp.body,
            Some(envelope::Body::AggregateStatusResponse(_))
        ));
    }
}
