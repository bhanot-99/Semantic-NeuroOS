//! Anonymiser tool for fixtures (phases.md §6.1 item 5): strips personal
//! content (window titles, media metadata, file paths) from a recorded
//! dump before it's committed to `tests/fixtures/telemetry/`, replacing it
//! with a stable placeholder derived from a hash of the original — distinct
//! real values stay distinct after anonymisation (useful for e.g. "does
//! this title change" assertions) without leaking the real content.
use std::path::Path;

use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;

use crate::dump::{DumpError, DumpReader, DumpWriter, Header};

pub async fn anonymise_file(input: &Path, output: &Path) -> Result<u64, DumpError> {
    let mut reader = DumpReader::open(input).await?;
    let mut writer = DumpWriter::create(
        output,
        Header {
            created_at_ns: reader.header.created_at_ns,
            anonymised: true,
        },
    )
    .await?;

    let mut count = 0u64;
    while let Some(mut event) = reader.read_event().await? {
        anonymise_event(&mut event);
        writer.write_event(&event).await?;
        count += 1;
    }
    writer.flush().await?;
    Ok(count)
}

fn anonymise_event(event: &mut neuroos_proto::v1::RawTelemetryEvent) {
    match &mut event.payload {
        Some(Payload::Window(w)) => anonymise_window_kind(&mut w.kind),
        Some(Payload::Mpris(m)) => {
            m.title = placeholder("title", &m.title);
            m.artist = placeholder("artist", &m.artist);
            m.album = placeholder("album", &m.album);
            m.track_id = placeholder("track", &m.track_id);
        }
        Some(Payload::FileActivity(f)) => {
            f.path = anonymise_path(&f.path);
        }
        _ => {}
    }
}

fn anonymise_window_kind(kind: &mut Option<Kind>) {
    match kind {
        Some(Kind::Opened(o)) => o.title = placeholder("title", &o.title),
        Some(Kind::TitleChanged(t)) => t.title = placeholder("title", &t.title),
        _ => {}
    }
}

fn anonymise_path(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    format!("/anon/{}{ext}", short_hash(path))
}

fn placeholder(kind: &str, original: &str) -> String {
    if original.is_empty() {
        return String::new();
    }
    format!("{kind}-{}", short_hash(original))
}

/// Not cryptographic — this only needs to be stable and cheap, not
/// collision-resistant against an adversary (fixtures aren't a security
/// boundary; R0-6 is why this exists at all).
fn short_hash(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::{RawTelemetryEvent, WindowEvent, WindowOpened};

    #[test]
    fn placeholder_is_stable_and_distinct() {
        let a = placeholder("title", "My Secret Document.txt");
        let b = placeholder("title", "My Secret Document.txt");
        let c = placeholder("title", "Something else");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("title-"));
    }

    #[test]
    fn placeholder_of_empty_string_stays_empty() {
        assert_eq!(placeholder("title", ""), "");
    }

    #[test]
    fn anonymise_path_keeps_extension_only() {
        let p = anonymise_path("/home/u/Notes/2026-plan.md");
        assert!(p.starts_with("/anon/"));
        assert!(p.ends_with(".md"));
        assert!(!p.contains("2026-plan"));
    }

    #[test]
    fn anonymise_event_replaces_window_title_but_keeps_app_id() {
        let mut event = RawTelemetryEvent {
            observed_at_ns: 1,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id: 1,
                kind: Some(Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "My Bank Account - Statement".into(),
                    pid: 123,
                    pid_known: true,
                })),
            })),
        };
        anonymise_event(&mut event);
        match event.payload {
            Some(Payload::Window(WindowEvent {
                kind: Some(Kind::Opened(o)),
                ..
            })) => {
                assert_eq!(o.app_id, "org.mozilla.firefox"); // not personal, kept
                assert!(!o.title.contains("Bank"));
                assert_eq!(o.pid, 123); // kept
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn anonymise_file_round_trips_and_sets_the_header_flag() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.bin");
        let output = dir.path().join("out.bin");

        let mut w = crate::dump::DumpWriter::create(
            &input,
            Header {
                created_at_ns: 5,
                anonymised: false,
            },
        )
        .await
        .unwrap();
        w.write_event(&RawTelemetryEvent {
            observed_at_ns: 1,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id: 1,
                kind: Some(Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "Secret".into(),
                    pid: 1,
                    pid_known: true,
                })),
            })),
        })
        .await
        .unwrap();
        w.flush().await.unwrap();

        let count = anonymise_file(&input, &output).await.unwrap();
        assert_eq!(count, 1);

        let mut r = DumpReader::open(&output).await.unwrap();
        assert!(r.header.anonymised);
        let event = r.read_event().await.unwrap().unwrap();
        match event.payload {
            Some(Payload::Window(WindowEvent {
                kind: Some(Kind::Opened(o)),
                ..
            })) => assert!(!o.title.contains("Secret")),
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
