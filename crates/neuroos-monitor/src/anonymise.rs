//! Anonymiser tool for fixtures (phases.md §6.1 item 5): strips personal
//! content (window titles, media metadata, file paths) from a recorded
//! dump before it's committed to `tests/fixtures/telemetry/`, replacing it
//! with a stable placeholder derived from a keyed hash of the original —
//! distinct real values stay distinct after anonymisation (useful for e.g.
//! "does this title change" assertions) without leaking the real content.
//!
//! M8: the placeholder used to be an unsalted FNV-1a hash, which is
//! reversible by guessing. Fixtures are committed to the repository, so
//! anyone holding one could confirm "was this window titled
//! `Barclays — Statement`?" by hashing the candidate and comparing — the
//! exact disclosure R0-6 exists to prevent, one dictionary away. Every run
//! now draws a random key, derives placeholders with HMAC-SHA256 under it,
//! and discards the key when the run ends: within one dump the mapping is
//! still consistent, but nothing outside that run can reproduce or test it.
use std::path::Path;

use hmac::{Hmac, Mac};
use neuroos_proto::v1::raw_telemetry_event::Payload;
use neuroos_proto::v1::window_event::Kind;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::dump::{DumpError, DumpReader, DumpWriter, Header};

/// Hex characters of HMAC tag kept in a placeholder (64 bits). Enough to
/// keep distinct titles distinct across a dump without making the
/// placeholders unreadably long.
const PLACEHOLDER_HEX_LEN: usize = 16;

/// Holds one run's anonymisation key, as an already-keyed HMAC instance
/// cloned per value. Never serialised, never logged — the point of M8 is
/// that the mapping cannot be reproduced after the run that made it.
///
/// The key lives inside the `Hmac` rather than in a field of its own so
/// there is exactly one copy of it, and so the one fallible step
/// (`new_from_slice`) happens in the constructor, which already returns a
/// `Result` — rules.md §5.1 leaves no room for an `unwrap` on a hot path.
pub struct Anonymiser {
    mac: Hmac<Sha256>,
}

impl Anonymiser {
    /// Draws a fresh key from the OS CSPRNG. A failure here is fatal to
    /// anonymisation: falling back to an unkeyed hash would silently
    /// reintroduce M8.
    pub fn with_random_key() -> Result<Self, DumpError> {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(key.as_mut()).map_err(|e| {
            DumpError::Io(std::io::Error::other(format!(
                "could not draw an anonymisation key: {e}"
            )))
        })?;
        Self::with_key(key.as_ref())
    }

    fn with_key(key: &[u8]) -> Result<Self, DumpError> {
        let mac = <Hmac<Sha256>>::new_from_slice(key).map_err(|_| {
            DumpError::Io(std::io::Error::other(
                "HMAC-SHA256 rejected the anonymisation key length",
            ))
        })?;
        Ok(Self { mac })
    }

    /// A placeholder for `original`, tagged with `kind` so the same text
    /// appearing as a title and as an album does not collide.
    fn placeholder(&self, kind: &str, original: &str) -> String {
        if original.is_empty() {
            return String::new();
        }
        format!("{kind}-{}", self.keyed_hash(kind, original))
    }

    fn anonymise_path(&self, path: &str) -> String {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        format!("/anon/{}{ext}", self.keyed_hash("path", path))
    }

    /// HMAC-SHA256 over `kind` and `original` under the run key, truncated
    /// to [`PLACEHOLDER_HEX_LEN`] hex characters. `hmac` + `sha2` rather
    /// than a hand-rolled keyed hash (rules.md §4.2); the length prefix
    /// stops `("ab", "c")` and `("a", "bc")` from colliding.
    fn keyed_hash(&self, kind: &str, original: &str) -> String {
        let mut mac = self.mac.clone();
        mac.update(&(kind.len() as u64).to_le_bytes());
        mac.update(kind.as_bytes());
        mac.update(original.as_bytes());
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(PLACEHOLDER_HEX_LEN);
        for b in tag.iter().take(PLACEHOLDER_HEX_LEN / 2) {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    fn anonymise_event(&self, event: &mut neuroos_proto::v1::RawTelemetryEvent) {
        match &mut event.payload {
            Some(Payload::Window(w)) => self.anonymise_window_kind(&mut w.kind),
            Some(Payload::Mpris(m)) => {
                m.title = self.placeholder("title", &m.title);
                m.artist = self.placeholder("artist", &m.artist);
                m.album = self.placeholder("album", &m.album);
                m.track_id = self.placeholder("track", &m.track_id);
            }
            Some(Payload::FileActivity(f)) => {
                f.path = self.anonymise_path(&f.path);
            }
            _ => {}
        }
    }

    fn anonymise_window_kind(&self, kind: &mut Option<Kind>) {
        match kind {
            Some(Kind::Opened(o)) => o.title = self.placeholder("title", &o.title),
            Some(Kind::TitleChanged(t)) => t.title = self.placeholder("title", &t.title),
            _ => {}
        }
    }
}

pub async fn anonymise_file(input: &Path, output: &Path) -> Result<u64, DumpError> {
    // M8: one fresh key per run, zeroized when this value drops.
    let anonymiser = Anonymiser::with_random_key()?;
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
        anonymiser.anonymise_event(&mut event);
        writer.write_event(&event).await?;
        count += 1;
    }
    writer.flush().await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::{RawTelemetryEvent, WindowEvent, WindowOpened};

    /// A fixed key, so these tests can assert on stability and distinctness
    /// without depending on the random key a real run draws.
    fn fixed() -> Anonymiser {
        Anonymiser::with_key(&[7u8; 32]).unwrap()
    }

    #[test]
    fn placeholder_is_stable_and_distinct_under_one_key() {
        let a = fixed();
        assert_eq!(
            a.placeholder("title", "My Secret Document.txt"),
            a.placeholder("title", "My Secret Document.txt"),
            "the same title must map consistently within a run"
        );
        assert_ne!(
            a.placeholder("title", "My Secret Document.txt"),
            a.placeholder("title", "Something else"),
            "distinct titles must stay distinct"
        );
        assert!(a.placeholder("title", "x").starts_with("title-"));
    }

    /// M8: the whole point. A committed fixture used to be checkable
    /// against a guess, because the hash had no secret in it. Two runs now
    /// produce different placeholders for the same title, so a guess can
    /// never be confirmed from the file alone.
    #[test]
    fn two_runs_never_produce_the_same_placeholder() {
        let title = "Barclays — Statement January";
        let first = Anonymiser::with_random_key().unwrap();
        let second = Anonymiser::with_random_key().unwrap();
        assert_ne!(
            first.placeholder("title", title),
            second.placeholder("title", title),
            "a per-run key must make the mapping unreproducible"
        );
    }

    /// M8: an attacker holding the fixture and a dictionary must not be
    /// able to reproduce a placeholder, which is exactly what an unkeyed
    /// hash let them do.
    #[test]
    fn a_placeholder_cannot_be_reproduced_without_the_key() {
        let real = fixed();
        // Same construction, different key.
        let guesser = Anonymiser::with_key(&[8u8; 32]).unwrap();
        let candidate = "My Bank Account - Statement";
        assert_ne!(
            real.placeholder("title", candidate),
            guesser.placeholder("title", candidate)
        );
    }

    #[test]
    fn the_same_text_in_different_fields_does_not_collide() {
        let a = fixed();
        let text = "Nocturne";
        assert_ne!(
            a.placeholder("title", text),
            a.placeholder("album", text),
            "the kind tag must separate the two namespaces"
        );
    }

    #[test]
    fn placeholder_of_empty_string_stays_empty() {
        assert_eq!(fixed().placeholder("title", ""), "");
    }

    #[test]
    fn keyed_hash_is_the_expected_width() {
        assert_eq!(fixed().keyed_hash("title", "x").len(), PLACEHOLDER_HEX_LEN);
    }

    #[test]
    fn anonymise_path_keeps_extension_only() {
        let p = fixed().anonymise_path("/home/u/Notes/2026-plan.md");
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
        fixed().anonymise_event(&mut event);
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

    /// Two anonymisations of the same dump must not be byte-identical, or
    /// the per-run key is not actually being used end to end.
    #[tokio::test]
    async fn two_anonymisations_of_one_dump_differ() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.bin");
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
                    title: "Barclays Statement".into(),
                    pid: 1,
                    pid_known: true,
                })),
            })),
        })
        .await
        .unwrap();
        w.flush().await.unwrap();

        let title_of = |path: &std::path::Path| {
            let path = path.to_path_buf();
            async move {
                let mut r = DumpReader::open(&path).await.unwrap();
                match r.read_event().await.unwrap().unwrap().payload {
                    Some(Payload::Window(WindowEvent {
                        kind: Some(Kind::Opened(o)),
                        ..
                    })) => o.title,
                    other => panic!("unexpected payload: {other:?}"),
                }
            }
        };

        let first = dir.path().join("a.bin");
        let second = dir.path().join("b.bin");
        anonymise_file(&input, &first).await.unwrap();
        anonymise_file(&input, &second).await.unwrap();
        assert_ne!(title_of(&first).await, title_of(&second).await);
    }
}
