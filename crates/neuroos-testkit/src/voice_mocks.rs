//! Mock `voice.sock` server for Phase 5 IT work (C2 itself lands in Phase
//! 6; phases.md §8.3 explicitly plans "mock C2/C6" for this phase).
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{Envelope, PreambleResponse, envelope};

/// Accepts connections, records the `Instant` its first `PreambleRequest`
/// arrives, and acks every request with `PreambleResponse{played: true}`.
/// Callers read the handle after their exchange completes.
pub fn spawn_preamble_recorder(
    sock_path: impl AsRef<Path>,
    allowed_uid: u32,
) -> Arc<Mutex<Option<Instant>>> {
    let received = Arc::new(Mutex::new(None));
    let handle = Arc::clone(&received);
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        let Ok(server) = UdsServer::bind(UdsServerConfig::new(sock_path, vec![allowed_uid])) else {
            return;
        };
        loop {
            let Ok(Some((mut stream, _cred))) = server.accept().await else {
                continue;
            };
            while let Ok(Some(env)) = read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
                if matches!(env.body, Some(envelope::Body::Preamble(_))) {
                    let mut guard = handle.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.is_none() {
                        *guard = Some(Instant::now());
                    }
                }
                let resp = Envelope {
                    schema_version: 1,
                    trace_id: env.trace_id,
                    request_id: env.request_id,
                    sent_at_ns: 0,
                    body: Some(envelope::Body::PreambleResponse(PreambleResponse {
                        played: true,
                    })),
                };
                if write_envelope(&mut stream, &resp, DEFAULT_MAX_FRAME)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    received
}
