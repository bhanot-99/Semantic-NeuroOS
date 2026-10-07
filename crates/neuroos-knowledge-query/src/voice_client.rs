//! Thin client to `voice.sock` (Architecture.md §5.2). C2 itself lands in
//! Phase 6; this phase talks to a mock server bound at the same path (see
//! `neuroos_testkit::voice_mocks`). FR-KNO-02: request an acoustic preamble
//! before any retrieval work begins.
use std::path::PathBuf;
use std::time::Duration;

use neuroos_ipc::{OnRestart, request_once};
use neuroos_proto::v1::{PreambleRequest, envelope};

/// rules.md §5.7: no voice.sock-specific deadline is named, so this uses
/// the default "control call" budget (250 ms).
pub const VOICE_REQUEST_DEADLINE: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum VoiceClientError {
    #[error("failed to connect to voice.sock at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("voice.sock request failed: {0}")]
    Io(#[from] neuroos_ipc::FramingError),
    #[error("voice.sock closed the connection with no response")]
    NoResponse,
    #[error("voice.sock returned an error: {0}")]
    Remote(String),
    #[error("voice.sock sent an unexpected response type")]
    UnexpectedResponse,
}

/// C3: `request_once` reports the transport failures this enum already had
/// variants for, so the mapping is one-to-one and every existing
/// `VoiceClientError::Connect { .. }` match keeps working.
impl From<neuroos_ipc::RequestError> for VoiceClientError {
    fn from(e: neuroos_ipc::RequestError) -> Self {
        match e {
            neuroos_ipc::RequestError::Connect { path, source } => Self::Connect { path, source },
            neuroos_ipc::RequestError::Framing(e) => Self::Io(e),
            neuroos_ipc::RequestError::NoResponse => Self::NoResponse,
        }
    }
}

#[derive(Clone)]
pub struct VoiceClient {
    socket_path: PathBuf,
}

impl VoiceClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// FR-KNO-02: `clip_id` is the preamble phrase (design.md §9.3's
    /// catalogue). Returns once C2 has accepted it -- rules.md §5.6 (fail
    /// soft): a failure here must not stop the answer itself from
    /// proceeding; callers decide whether to treat this as fatal.
    pub async fn request_preamble(&self, clip_id: &str) -> Result<(), VoiceClientError> {
        // C3: connect/envelope/write/read lives in neuroos_ipc::request_once.
        let response = request_once(
            &self.socket_path,
            envelope::Body::Preamble(PreambleRequest {
                clip_id: clip_id.to_string(),
            }),
            1,
            VOICE_REQUEST_DEADLINE,
            OnRestart::Retry,
        )
        .await?;
        match response.body {
            Some(envelope::Body::PreambleResponse(_)) => Ok(()),
            Some(envelope::Body::Error(e)) => Err(VoiceClientError::Remote(e.message)),
            _ => Err(VoiceClientError::UnexpectedResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn connect_failure_is_a_clear_error_not_a_hang() {
        let client = VoiceClient::new(PathBuf::from(
            "/tmp/neuroos-knowledge-query-test-nonexistent-voice.sock",
        ));
        let err = client.request_preamble("test").await.unwrap_err();
        assert!(matches!(err, VoiceClientError::Connect { .. }));
    }
}
