//! Thin client to `kernel.sock` (Architecture.md §5.2). C6 itself lands in
//! Phase 7; this phase talks to a mock server bound at the same path.
//! FR-KNO-08: ask C6 to evaluate the capability of every generation before
//! executing it.
use std::path::PathBuf;
use std::time::Duration;

use neuroos_ipc::{DEFAULT_MAX_FRAME, connect, read_envelope_deadline, write_envelope_deadline};
use neuroos_proto::v1::{ActionRequest, Envelope, Taint, envelope};
use neuroos_taint::TaintFlags;

/// rules.md §5.7: C6 checks get a 50 ms deadline (confirmation dialogs
/// excluded -- there is no dialog on this path, generation is always SAFE
/// base tier).
pub const KERNEL_CHECK_DEADLINE: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum KernelClientError {
    #[error("failed to connect to kernel.sock at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("kernel.sock request failed: {0}")]
    Io(#[from] neuroos_ipc::FramingError),
    #[error("kernel.sock closed the connection with no response")]
    NoResponse,
    #[error("kernel.sock returned an error: {0}")]
    Remote(String),
    #[error("kernel.sock sent an unexpected response type")]
    UnexpectedResponse,
}

pub struct KernelClient {
    socket_path: PathBuf,
}

impl KernelClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// FR-KNO-08 / Architecture.md §6.1: `EvaluateCapability(llm.generate,
    /// taint)`. Returns whether C6 approved it.
    pub async fn evaluate_capability(
        &self,
        capability: &str,
        taint: TaintFlags,
    ) -> Result<bool, KernelClientError> {
        let mut stream = connect(&self.socket_path, KERNEL_CHECK_DEADLINE)
            .await
            .map_err(|source| KernelClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
        let request = Envelope {
            schema_version: 1,
            trace_id: String::new(),
            request_id: 1,
            sent_at_ns: neuroos_common::now_ns(),
            body: Some(envelope::Body::ActionRequest(ActionRequest {
                capability: capability.to_string(),
                taint: Some(Taint {
                    flags: taint.bits(),
                }),
            })),
        };
        write_envelope_deadline(
            &mut stream,
            &request,
            DEFAULT_MAX_FRAME,
            KERNEL_CHECK_DEADLINE,
        )
        .await?;
        let response =
            read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, KERNEL_CHECK_DEADLINE)
                .await?
                .ok_or(KernelClientError::NoResponse)?;
        match response.body {
            Some(envelope::Body::ActionResponse(r)) => Ok(r.approved),
            Some(envelope::Body::Error(e)) => Err(KernelClientError::Remote(e.message)),
            _ => Err(KernelClientError::UnexpectedResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = KernelClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent-kernel.sock",
        ));
        let err = client
            .evaluate_capability("llm.generate", TaintFlags::empty())
            .await
            .unwrap_err();
        assert!(matches!(err, KernelClientError::Connect { .. }));
    }
}
