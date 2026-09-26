//! Deadline-wrapped envelope read/write, layered on `framing`.
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::framing::FramingError;

pub async fn write_envelope_deadline<W: AsyncWrite + Unpin>(
    w: &mut W,
    env: &neuroos_proto::v1::Envelope,
    max_frame: u32,
    deadline: Duration,
) -> Result<(), FramingError> {
    match tokio::time::timeout(deadline, crate::framing::write_envelope(w, env, max_frame)).await {
        Ok(res) => res,
        Err(_) => Err(FramingError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "write deadline exceeded",
        ))),
    }
}

pub async fn read_envelope_deadline<R: AsyncRead + Unpin>(
    r: &mut R,
    max_frame: u32,
    deadline: Duration,
) -> Result<Option<neuroos_proto::v1::Envelope>, FramingError> {
    match tokio::time::timeout(deadline, crate::framing::read_envelope(r, max_frame)).await {
        Ok(res) => res,
        Err(_) => Err(FramingError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "read deadline exceeded",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_times_out_when_peer_is_silent() {
        let (_a, mut b) = tokio::io::duplex(64); // `_a` kept open but never writes
        let err = read_envelope_deadline(
            &mut b,
            crate::framing::DEFAULT_MAX_FRAME,
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FramingError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut));
    }
}
