//! One-shot request/response over a Unix socket.
//!
//! C3: every client in the system repeated the same five steps — connect,
//! build an `Envelope` around a body, write it under a deadline, read one
//! response under the same deadline, turn "connection closed with nothing"
//! into an error. That was duplicated nine times across `neuroosctl` and
//! C5a's four clients, each copy free to get the envelope header or the
//! deadline subtly wrong. The per-RPC part — matching the response body and
//! mapping failures into the caller's own error enum — stays with the
//! caller, because that genuinely differs per RPC.
//!
//! Streaming callers (C5a's `inference_client`, which writes a request and
//! then reads many slots from a shared memfd ring) do not use this: they own
//! their stream for the whole exchange.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use neuroos_proto::v1::{Envelope, envelope};

use crate::client::{connect, connect_retrying};
use crate::deadline::{read_envelope_deadline, write_envelope_deadline};
use crate::framing::{DEFAULT_MAX_FRAME, FramingError};

/// What can go wrong before the caller gets a response body to inspect.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("failed to connect to {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("request failed: {0}")]
    Framing(#[from] FramingError),
    #[error("the peer closed the connection with no response")]
    NoResponse,
}

/// Whether to wait out a server that is restarting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnRestart {
    /// Retry a transient connect failure on the jittered backoff schedule
    /// until the deadline is spent ([`connect_retrying`]). For a component
    /// that is expected to be running.
    Retry,
    /// Fail on the first connect error ([`connect`]).
    FailFast,
}

/// Connects to `socket_path`, sends one envelope carrying `body`, and
/// returns the single response envelope.
///
/// `deadline` is applied to each of the three stages (connect, write, read)
/// rather than to the whole exchange, matching what every call site this
/// replaced already did.
pub async fn request_once(
    socket_path: &Path,
    body: envelope::Body,
    request_id: u64,
    deadline: Duration,
    on_restart: OnRestart,
) -> Result<Envelope, RequestError> {
    let mut stream = match on_restart {
        OnRestart::Retry => connect_retrying(socket_path, deadline).await,
        OnRestart::FailFast => connect(socket_path, deadline).await,
    }
    .map_err(|source| RequestError::Connect {
        path: socket_path.to_path_buf(),
        source,
    })?;

    let request = Envelope {
        schema_version: 1,
        trace_id: String::new(),
        request_id,
        sent_at_ns: neuroos_common::now_ns(),
        body: Some(body),
    };
    write_envelope_deadline(&mut stream, &request, DEFAULT_MAX_FRAME, deadline).await?;
    read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, deadline)
        .await?
        .ok_or(RequestError::NoResponse)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::{HealthRequest, HealthResponse};

    #[tokio::test]
    async fn a_missing_socket_is_a_connect_error_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let err = request_once(
            &dir.path().join("nonexistent.sock"),
            envelope::Body::HealthRequest(HealthRequest {}),
            1,
            Duration::from_millis(200),
            OnRestart::FailFast,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RequestError::Connect { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_peer_that_closes_without_answering_is_no_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silent.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            // Read the request, then close without answering. (Dropping
            // before the read would reset the connection mid-write, which
            // is a Framing error, not NoResponse.)
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                .await;
        });

        let err = request_once(
            &path,
            envelope::Body::HealthRequest(HealthRequest {}),
            1,
            Duration::from_millis(500),
            OnRestart::FailFast,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RequestError::NoResponse), "{err:?}");
    }

    #[tokio::test]
    async fn a_real_round_trip_returns_the_response_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("echo.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let req =
                read_envelope_deadline(&mut stream, DEFAULT_MAX_FRAME, Duration::from_secs(1))
                    .await
                    .unwrap()
                    .unwrap();
            let resp = Envelope {
                schema_version: 1,
                trace_id: String::new(),
                request_id: req.request_id,
                sent_at_ns: 0,
                body: Some(envelope::Body::HealthResponse(HealthResponse::default())),
            };
            write_envelope_deadline(
                &mut stream,
                &resp,
                DEFAULT_MAX_FRAME,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        });

        let resp = request_once(
            &path,
            envelope::Body::HealthRequest(HealthRequest {}),
            7,
            Duration::from_secs(1),
            OnRestart::FailFast,
        )
        .await
        .unwrap();
        assert_eq!(resp.request_id, 7, "the request_id must round-trip");
        assert!(matches!(resp.body, Some(envelope::Body::HealthResponse(_))));
    }
}
