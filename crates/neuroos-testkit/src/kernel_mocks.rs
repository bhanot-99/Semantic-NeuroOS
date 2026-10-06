//! Mock `kernel.sock` server for Phase 5 IT work (C6 itself lands in Phase
//! 7; phases.md §8.3 plans "mock C2/C6" for this phase).
use std::path::Path;

use neuroos_ipc::{DEFAULT_MAX_FRAME, UdsServer, UdsServerConfig, read_envelope, write_envelope};
use neuroos_proto::v1::{ActionResponse, Envelope, envelope};

/// Accepts connections and approves every `ActionRequest` -- generation is
/// always SAFE base tier (Architecture.md §8.3's tier table), so an
/// always-approve mock is a faithful stand-in for C6's real evaluation on
/// this path, not just a permissive shortcut.
pub fn spawn_always_approve(sock_path: impl AsRef<Path>, allowed_uid: u32) {
    let sock_path = sock_path.as_ref().to_path_buf();
    tokio::spawn(async move {
        let Ok(server) = UdsServer::bind(UdsServerConfig::new(sock_path, vec![allowed_uid])) else {
            return;
        };
        loop {
            let Ok(Some((mut stream, _cred, _permit))) = server.accept().await else {
                continue;
            };
            while let Ok(Some(env)) = read_envelope(&mut stream, DEFAULT_MAX_FRAME).await {
                if !matches!(env.body, Some(envelope::Body::ActionRequest(_))) {
                    continue;
                }
                let resp = Envelope {
                    schema_version: 1,
                    trace_id: env.trace_id,
                    request_id: env.request_id,
                    sent_at_ns: 0,
                    body: Some(envelope::Body::ActionResponse(ActionResponse {
                        approved: true,
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
}
