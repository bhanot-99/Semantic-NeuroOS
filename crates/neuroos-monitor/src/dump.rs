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
    pub async fn create(path: &Path, header: Header) -> Result<Self, DumpError> {
        let mut w = BufWriter::new(File::create(path).await?);
        w.write_all(&header.encode()).await?;
        w.flush().await?;
        Ok(Self { w })
    }

    pub async fn write_event(&mut self, event: &RawTelemetryEvent) -> Result<(), DumpError> {
        let buf = event.encode_to_vec();
        framing::write_frame(&mut self.w, &buf, DEFAULT_MAX_FRAME).await?;
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
