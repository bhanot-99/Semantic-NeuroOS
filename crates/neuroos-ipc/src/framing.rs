//! u32-LE length-prefixed protobuf framing (Architecture.md §5.1).
use std::io;

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default max frame size: 4 MiB (Architecture.md §5.1). Control sockets should
/// pass a smaller value explicitly.
pub const DEFAULT_MAX_FRAME: u32 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    #[error("frame of {0} bytes exceeds max frame size {1} bytes")]
    FrameTooLarge(u32, u32),
    #[error("connection closed mid-frame")]
    Truncated,
    #[error("failed to decode protobuf payload: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Writes `payload` as a u32-LE length prefix followed by the raw bytes.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
    max_frame: u32,
) -> Result<(), FramingError> {
    if payload.len() as u64 > max_frame as u64 {
        return Err(FramingError::FrameTooLarge(payload.len() as u32, max_frame));
    }
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one length-prefixed frame. Rejects frames declaring a length above
/// `max_frame` without reading the body, and reports a clean end-of-stream
/// (before any bytes of a new frame arrive) as `Ok(None)`.
pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    max_frame: u32,
) -> Result<Option<Vec<u8>>, FramingError> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = r.read_exact(&mut len_buf).await {
        return match e.kind() {
            io::ErrorKind::UnexpectedEof => Ok(None),
            _ => Err(e.into()),
        };
    }
    let len = u32::from_le_bytes(len_buf);
    if len > max_frame {
        return Err(FramingError::FrameTooLarge(len, max_frame));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await.map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FramingError::Truncated,
        _ => e.into(),
    })?;
    Ok(Some(buf))
}

/// Encodes and writes an `Envelope` as one frame.
pub async fn write_envelope<W: AsyncWrite + Unpin>(
    w: &mut W,
    env: &neuroos_proto::v1::Envelope,
    max_frame: u32,
) -> Result<(), FramingError> {
    let buf = env.encode_to_vec();
    write_frame(w, &buf, max_frame).await
}

/// Reads one frame and decodes it as an `Envelope`. `Ok(None)` means the peer
/// closed the connection cleanly between frames.
pub async fn read_envelope<R: AsyncRead + Unpin>(
    r: &mut R,
    max_frame: u32,
) -> Result<Option<neuroos_proto::v1::Envelope>, FramingError> {
    match read_frame(r, max_frame).await? {
        Some(buf) => Ok(Some(neuroos_proto::v1::Envelope::decode(buf.as_slice())?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[tokio::test]
    async fn roundtrip_small_payload() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, b"hello", DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let got = read_frame(&mut b, DEFAULT_MAX_FRAME).await.unwrap();
        assert_eq!(got.as_deref(), Some(&b"hello"[..]));
    }

    #[tokio::test]
    async fn roundtrip_empty_payload() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, b"", DEFAULT_MAX_FRAME).await.unwrap();
        let got = read_frame(&mut b, DEFAULT_MAX_FRAME).await.unwrap();
        assert_eq!(got.as_deref(), Some(&b""[..]));
    }

    #[tokio::test]
    async fn exactly_max_frame_is_accepted() {
        let payload = vec![7u8; 16];
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &payload, 16).await.unwrap();
        let got = read_frame(&mut b, 16).await.unwrap();
        assert_eq!(got, Some(payload));
    }

    #[tokio::test]
    async fn oversized_write_is_rejected_before_sending() {
        let (mut a, _b) = tokio::io::duplex(1024);
        let err = write_frame(&mut a, &[0u8; 17], 16).await.unwrap_err();
        assert!(matches!(err, FramingError::FrameTooLarge(17, 16)));
    }

    #[tokio::test]
    async fn oversized_declared_length_is_rejected_on_read() {
        // hand-craft a frame whose length prefix lies about the payload size
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&17u32.to_le_bytes()).await.unwrap();
        let err = read_frame(&mut b, 16).await.unwrap_err();
        assert!(matches!(err, FramingError::FrameTooLarge(17, 16)));
    }

    #[tokio::test]
    async fn truncated_frame_is_reported() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&10u32.to_le_bytes()).await.unwrap();
        a.write_all(b"abc").await.unwrap(); // promised 10 bytes, sent 3
        drop(a);
        let err = read_frame(&mut b, DEFAULT_MAX_FRAME).await.unwrap_err();
        assert!(matches!(err, FramingError::Truncated));
    }

    #[tokio::test]
    async fn clean_eof_between_frames_is_not_an_error() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        let got = read_frame(&mut b, DEFAULT_MAX_FRAME).await.unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn envelope_roundtrip() {
        use neuroos_proto::v1::{Envelope, Error, ErrorCode, envelope};
        let env = Envelope {
            schema_version: 1,
            trace_id: "abc".into(),
            request_id: 7,
            sent_at_ns: 123,
            body: Some(envelope::Body::Error(Error {
                code: ErrorCode::Internal as i32,
                message: "x".into(),
                retryable: false,
            })),
        };
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_envelope(&mut a, &env, DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let got = read_envelope(&mut b, DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(env, got);
    }
}
