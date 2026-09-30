//! Envelope framing that also carries a file descriptor via `SCM_RIGHTS`
//! (Architecture.md §5.1: "The memfd is passed with SCM_RIGHTS over
//! inference.sock"). Mirrors `cpp/libneuroos/src/framing.cpp`'s
//! `write_envelope_with_fd`/`read_envelope_with_fd` bit-for-bit: the fd
//! rides on the same `sendmsg()` call that carries the 4-byte length
//! prefix, because Linux only delivers ancillary data to the `recvmsg()`
//! call that reads the accompanying bytes -- a plain `read()` of those same
//! bytes would silently drop the fd. `tokio::net::UnixStream::async_io`
//! (the officially sanctioned way to mix raw syscalls with tokio's reactor)
//! drives the readiness wait; `rustix::net::{sendmsg,recvmsg}` does the
//! actual syscall.
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};

use prost::Message;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recv, recvmsg, sendmsg,
};
use tokio::io::Interest;
use tokio::net::UnixStream;

use crate::framing::FramingError;

/// Sends `env` with `fd_to_send` attached via `SCM_RIGHTS`, in the single
/// `sendmsg()` call that carries the whole frame (length prefix + body).
/// This codebase's fd-carrying frames are small control messages (never
/// the bulk token stream itself), so a short write is treated as a real
/// transport error rather than retried piecemeal -- splitting the frame
/// across multiple `sendmsg()` calls would separate the ancillary data
/// from part of the frame, matching the C++ implementation's own
/// documented choice.
pub async fn write_envelope_with_fd(
    stream: &UnixStream,
    env: &neuroos_proto::v1::Envelope,
    max_frame: u32,
    fd_to_send: BorrowedFd<'_>,
) -> Result<(), FramingError> {
    let body = env.encode_to_vec();
    if body.len() as u32 > max_frame {
        return Err(FramingError::FrameTooLarge(body.len() as u32, max_frame));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);

    stream
        .async_io(Interest::WRITABLE, || {
            let iov = [rustix::io::IoSlice::new(&frame)];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            let fds = [fd_to_send];
            control.push(SendAncillaryMessage::ScmRights(&fds));
            let n = sendmsg(stream, &iov, &mut control, SendFlags::NOSIGNAL)
                .map_err(io::Error::from)?;
            if n != frame.len() {
                return Err(io::Error::other(
                    "short sendmsg while attaching an fd: frame split across writes",
                ));
            }
            Ok(())
        })
        .await
        .map_err(FramingError::Io)
}

/// Reads one envelope, receiving an fd via `SCM_RIGHTS` if the sender
/// attached one to it. `Ok(None)` means the peer closed the connection
/// cleanly between frames (same convention as [`crate::read_envelope`]).
pub async fn read_envelope_with_fd(
    stream: &UnixStream,
    max_frame: u32,
) -> Result<Option<(neuroos_proto::v1::Envelope, Option<OwnedFd>)>, FramingError> {
    let mut len_buf = [0u8; 4];
    let mut got = 0usize;
    let mut received_fd: Option<OwnedFd> = None;
    while got < 4 {
        let (n, fd) = stream
            .async_io(Interest::READABLE, || {
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
                let mut control = RecvAncillaryBuffer::new(&mut space);
                let mut iov = [rustix::io::IoSliceMut::new(&mut len_buf[got..])];
                let msg = recvmsg(stream, &mut iov, &mut control, RecvFlags::empty())
                    .map_err(io::Error::from)?;
                let mut fd = None;
                for m in control.drain() {
                    if let RecvAncillaryMessage::ScmRights(mut fds) = m {
                        fd = fds.next();
                    }
                }
                Ok::<_, io::Error>((msg.bytes, fd))
            })
            .await
            .map_err(FramingError::Io)?;
        if n == 0 {
            if got == 0 {
                return Ok(None); // clean EOF between frames
            }
            return Err(FramingError::Truncated);
        }
        if fd.is_some() {
            received_fd = fd;
        }
        got += n;
    }
    let len = u32::from_le_bytes(len_buf);
    if len > max_frame {
        return Err(FramingError::FrameTooLarge(len, max_frame));
    }
    // Read the body with the same raw-syscall-via-async_io approach as the
    // length prefix (no ancillary data expected here, but tokio's
    // AsyncRead is implemented for `UnixStream`/`&mut UnixStream`, not
    // `&UnixStream`, which is all this function has to work with).
    let mut body = vec![0u8; len as usize];
    let mut got = 0usize;
    while got < body.len() {
        let n = stream
            .async_io(Interest::READABLE, || {
                let (_, n) =
                    recv(stream, &mut body[got..], RecvFlags::empty()).map_err(io::Error::from)?;
                Ok::<_, io::Error>(n)
            })
            .await
            .map_err(FramingError::Io)?;
        if n == 0 {
            return Err(FramingError::Truncated);
        }
        got += n;
    }
    let env = neuroos_proto::v1::Envelope::decode(body.as_slice())?;
    Ok(Some((env, received_fd)))
}

/// Deadline-wrapped [`read_envelope_with_fd`], matching
/// [`crate::read_envelope_deadline`]'s convention.
pub async fn read_envelope_with_fd_deadline(
    stream: &UnixStream,
    max_frame: u32,
    deadline: std::time::Duration,
) -> Result<Option<(neuroos_proto::v1::Envelope, Option<OwnedFd>)>, FramingError> {
    match tokio::time::timeout(deadline, read_envelope_with_fd(stream, max_frame)).await {
        Ok(res) => res,
        Err(_) => Err(FramingError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "read deadline exceeded",
        ))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use std::os::fd::AsFd;

    use neuroos_proto::v1::{Envelope, Error, ErrorCode, envelope};
    use neuroos_shm::{SharedMap, create_memfd};

    use super::*;

    fn test_envelope() -> Envelope {
        Envelope {
            schema_version: 1,
            trace_id: "test".into(),
            request_id: 1,
            sent_at_ns: 0,
            body: Some(envelope::Body::Error(Error {
                code: ErrorCode::Internal as i32,
                message: "x".into(),
                retryable: false,
            })),
        }
    }

    /// Real proof, not just "an fd of some kind arrived": creates a real
    /// memfd, writes a byte pattern into it, sends it via SCM_RIGHTS over a
    /// real `UnixStream::pair()`, and confirms the *same* underlying
    /// memory (not a coincidentally-same-sized unrelated fd) is visible on
    /// the receiving end.
    #[tokio::test]
    async fn fd_and_envelope_both_arrive_together() {
        let (a, b) = UnixStream::pair().unwrap();

        let memfd = create_memfd("fd-framing-test", 4096).unwrap();
        {
            let map = SharedMap::new(memfd.as_fd(), 4096).unwrap();
            // SAFETY: `map` owns a valid 4096-byte MAP_SHARED mapping of
            // `memfd`; writing within bounds before anyone else maps it.
            unsafe {
                std::ptr::write_bytes(map.as_ptr(), 0xAB, 8);
            }
        }

        let env = test_envelope();
        write_envelope_with_fd(&a, &env, crate::framing::DEFAULT_MAX_FRAME, memfd.as_fd())
            .await
            .unwrap();

        let (got_env, got_fd) = read_envelope_with_fd(&b, crate::framing::DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .expect("connection should not have closed");
        assert_eq!(got_env, env);
        let got_fd = got_fd.expect("an fd should have been received");
        assert_eq!(rustix::fs::fstat(got_fd.as_fd()).unwrap().st_size, 4096);

        let map = SharedMap::new(got_fd.as_fd(), 4096).unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(map.as_ptr(), 8) };
        assert_eq!(
            bytes, &[0xAB; 8],
            "must be the same underlying memfd, not just any fd"
        );
    }

    #[tokio::test]
    async fn envelope_without_an_fd_reads_back_with_none() {
        let (mut a, b) = UnixStream::pair().unwrap();
        crate::write_envelope(&mut a, &test_envelope(), crate::framing::DEFAULT_MAX_FRAME)
            .await
            .unwrap();
        let (_, fd) = read_envelope_with_fd(&b, crate::framing::DEFAULT_MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        assert!(fd.is_none());
    }
}
