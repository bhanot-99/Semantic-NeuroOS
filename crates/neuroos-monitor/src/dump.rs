//! Record/replay dump format (FR-MON-10, P3-S06): "length-prefixed envelopes
//! with a header with anonymisation flag" (phases.md §6.1 item 4).
//!
//! Layout: a fixed `Header` followed by zero or more u32-LE length-prefixed
//! `RawTelemetryEvent` protobuf frames (reusing `neuroos_ipc::framing`, the
//! same framing every socket in this system uses).
use std::path::Path;

use neuroos_ipc::framing::{self, DEFAULT_MAX_FRAME, FramingError};
use neuroos_proto::v1::RawTelemetryEvent;
use prost::Message;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};

pub const MAGIC: [u8; 4] = *b"NRMD"; // NeuRoos Monitor Dump
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("not a neuroos-monitor dump file (bad magic)")]
    BadMagic,
    #[error("unsupported dump format version {0} (expected {FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// L14: the dump path already exists, and overwriting it would
    /// destroy captured telemetry.
    #[error("{0} already exists; refusing to overwrite a recorded dump")]
    AlreadyExists(std::path::PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub created_at_ns: u64,
    pub anonymised: bool,
}

const HEADER_LEN: usize = 4 + 4 + 8 + 1; // magic + version + created_at_ns + anonymised

impl Header {
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf[8..16].copy_from_slice(&self.created_at_ns.to_le_bytes());
        buf[16] = u8::from(self.anonymised);
        buf
    }

    fn decode(buf: &[u8; HEADER_LEN]) -> Result<Self, DumpError> {
        if buf[0..4] != MAGIC {
            return Err(DumpError::BadMagic);
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap_or_default());
        if version != FORMAT_VERSION {
            return Err(DumpError::UnsupportedVersion(version));
        }
        let created_at_ns = u64::from_le_bytes(buf[8..16].try_into().unwrap_or_default());
        Ok(Self {
            created_at_ns,
            anonymised: buf[16] != 0,
        })
    }
}

#[derive(Debug)]
pub struct DumpWriter {
    w: BufWriter<File>,
}

impl DumpWriter {
    /// L14: refuses to write over an existing file. This used to be
    /// `File::create`, which truncates: pointing `--record` (or
    /// `neuroosctl monitor anonymise`) at a path that already held a dump
    /// destroyed it silently, and a recorded dump is real captured
    /// telemetry that cannot be re-recorded. An operator who means to
    /// replace one deletes it first.
    pub async fn create(path: &Path, header: Header) -> Result<Self, DumpError> {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => DumpError::AlreadyExists(path.into()),
                _ => DumpError::Io(e),
            })?;
        let mut w = BufWriter::new(file);
        w.write_all(&header.encode()).await?;
        // The header is flushed on its own so a reader opening the file
        // mid-recording sees a valid header rather than an empty file.
        w.flush().await?;
        Ok(Self { w })
    }

    /// Appends one length-prefixed event to the buffer. L14: this used to
    /// go through [`framing::write_frame`], which flushes every frame --
    /// right for a socket (latency), pure syscall overhead for a file the
    /// caller is going to flush anyway. Callers that need the event on
    /// disk now call [`Self::flush`] themselves, which makes the choice
    /// visible instead of implicit.
    ///
    /// The `BufWriter` is what makes a flushed frame *one* write syscall
    /// rather than two (prefix, then payload), so it earns its place even
    /// for a caller that flushes after every event.
    pub async fn write_event(&mut self, event: &RawTelemetryEvent) -> Result<(), DumpError> {
        let buf = event.encode_to_vec();
        if buf.len() as u64 > DEFAULT_MAX_FRAME as u64 {
            return Err(DumpError::Framing(FramingError::FrameTooLarge(
                buf.len() as u32,
                DEFAULT_MAX_FRAME,
            )));
        }
        self.w.write_all(&(buf.len() as u32).to_le_bytes()).await?;
        self.w.write_all(&buf).await?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<(), DumpError> {
        self.w.flush().await?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct DumpReader {
    r: BufReader<File>,
    pub header: Header,
}

impl DumpReader {
    pub async fn open(path: &Path) -> Result<Self, DumpError> {
        let mut r = BufReader::new(File::open(path).await?);
        let mut buf = [0u8; HEADER_LEN];
        r.read_exact(&mut buf).await?;
        let header = Header::decode(&buf)?;
        Ok(Self { r, header })
    }

    /// `Ok(None)` at a clean end of file (no partial event pending).
    pub async fn read_event(&mut self) -> Result<Option<RawTelemetryEvent>, DumpError> {
        match framing::read_frame(&mut self.r, DEFAULT_MAX_FRAME).await? {
            Some(buf) => Ok(Some(
                RawTelemetryEvent::decode(buf.as_slice())
                    .map_err(|e| DumpError::Framing(FramingError::Decode(e)))?,
            )),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code

    /// L14: `File::create` truncates, so re-running `--record` (or
    /// `neuroosctl monitor anonymise`) onto an existing dump destroyed
    /// captured telemetry without a word.
    #[tokio::test]
    async fn create_refuses_to_overwrite_an_existing_dump() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.nrmd");
        let header = Header {
            created_at_ns: 42,
            anonymised: false,
        };

        let mut w = DumpWriter::create(&path, header).await.unwrap();
        w.write_event(&RawTelemetryEvent {
            observed_at_ns: 7,
            source: "test".into(),
            payload: None,
        })
        .await
        .unwrap();
        w.flush().await.unwrap();
        let size_before = tokio::fs::metadata(&path).await.unwrap().len();

        let err = DumpWriter::create(&path, header).await.unwrap_err();
        assert!(
            matches!(err, DumpError::AlreadyExists(_)),
            "expected AlreadyExists, got {err:?}"
        );

        // The original dump is untouched, which is the whole point.
        assert_eq!(tokio::fs::metadata(&path).await.unwrap().len(), size_before);
        let mut r = DumpReader::open(&path).await.unwrap();
        assert_eq!(r.header.created_at_ns, 42);
        assert!(r.read_event().await.unwrap().is_some());
    }

    /// L14: `write_event` buffers now instead of flushing every frame, so
    /// a dump is only complete once the writer is flushed -- and a flushed
    /// dump still reads back exactly as before.
    #[tokio::test]
    async fn events_are_buffered_until_flush_and_then_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffered.nrmd");
        let mut w = DumpWriter::create(
            &path,
            Header {
                created_at_ns: 1,
                anonymised: true,
            },
        )
        .await
        .unwrap();

        let event = |at: u64| RawTelemetryEvent {
            observed_at_ns: at,
            source: "test".into(),
            payload: None,
        };
        for at in 0..8u64 {
            w.write_event(&event(at)).await.unwrap();
        }
        // Still only the header on disk: the frames are in the BufWriter.
        let header_only = tokio::fs::metadata(&path).await.unwrap().len();
        assert_eq!(header_only, HEADER_LEN as u64, "write_event must not flush");

        w.flush().await.unwrap();
        assert!(tokio::fs::metadata(&path).await.unwrap().len() > header_only);

        let mut r = DumpReader::open(&path).await.unwrap();
        assert!(r.header.anonymised);
        let mut seen = Vec::new();
        while let Some(e) = r.read_event().await.unwrap() {
            seen.push(e.observed_at_ns);
        }
        assert_eq!(seen, (0..8).collect::<Vec<u64>>());
    }
    use super::*;
    use neuroos_proto::v1::IdleEvent;
    use neuroos_proto::v1::raw_telemetry_event::Payload;

    fn idle_event(idle: bool, at: u64) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at,
            source: "idle".into(),
            payload: Some(Payload::Idle(IdleEvent { idle })),
        }
    }

    #[tokio::test]
    async fn record_then_replay_round_trips_events_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.bin");
        let header = Header {
            created_at_ns: 42,
            anonymised: false,
        };
        let mut w = DumpWriter::create(&path, header).await.unwrap();
        w.write_event(&idle_event(true, 1)).await.unwrap();
        w.write_event(&idle_event(false, 2)).await.unwrap();
        w.flush().await.unwrap();

        let mut r = DumpReader::open(&path).await.unwrap();
        assert_eq!(r.header, header);
        assert_eq!(r.read_event().await.unwrap(), Some(idle_event(true, 1)));
        assert_eq!(r.read_event().await.unwrap(), Some(idle_event(false, 2)));
        assert_eq!(r.read_event().await.unwrap(), None);
    }

    #[tokio::test]
    async fn empty_dump_has_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        DumpWriter::create(
            &path,
            Header {
                created_at_ns: 1,
                anonymised: true,
            },
        )
        .await
        .unwrap();

        let mut r = DumpReader::open(&path).await.unwrap();
        assert!(r.header.anonymised);
        assert_eq!(r.read_event().await.unwrap(), None);
    }

    #[tokio::test]
    async fn bad_magic_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.bin");
        tokio::fs::write(&path, b"not a dump file at all, way more than header len")
            .await
            .unwrap();
        let err = DumpReader::open(&path).await.unwrap_err();
        assert!(matches!(err, DumpError::BadMagic));
    }
}
