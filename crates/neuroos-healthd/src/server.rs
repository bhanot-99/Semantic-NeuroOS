//! `healthd.sock`: serves `AggregateStatusRequest` -> `AggregateStatusResponse`
//! to `neuroosctl status` (Architecture.md §5.2).
use std::sync::Arc;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{AggregateStatusResponse, ComponentStatus, Envelope, envelope};

use crate::aggregate::Aggregate;

pub async fn serve(
    aggregate: Arc<Aggregate>,
    socket_path: impl Into<std::path::PathBuf>,
    allowed_uids: Vec<u32>,
) -> std::io::Result<()> {
    let server = UdsServer::bind(UdsServerConfig::new(socket_path, allowed_uids))?;
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
