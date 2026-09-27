//! Mock `storage.sock` server for Phase 5 IT work that needs to observe
//! *ordering* against another mock (e.g. P5-S02's "preamble precedes
//! retrieval" assertion) without paying for a real `StorageEngine`
//! (embedder + LanceDB + SQLite) or an AB-1-violating dependency on
//! `neuroos-storage` from production code. C3's own real server lives in
//! `neuroos-storage::server`.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{Envelope, QueryFocusHistoryResponse, envelope};

/// Accepts connections, records the `Instant` its first
/// `QueryFocusHistoryRequest` arrives, and responds with an empty result
/// (`row: None`) -- callers using this care about ordering, not the
/// answer content (which P5-S01's own test already proves for real).
pub fn spawn_focus_history_recorder(
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
                if matches!(env.body, Some(envelope::Body::QueryFocusHistoryRequest(_))) {
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
                    body: Some(envelope::Body::QueryFocusHistoryResponse(
                        QueryFocusHistoryResponse { row: None },
                    )),
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
