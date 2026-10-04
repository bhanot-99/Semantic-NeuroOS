//! Fetcher spool ingest (P4-S06, FR-STO-08): `neuroos-fetcher` (C7) writes
//! one JSON file per fetched document to `/var/spool/neuroos-fetcher/`
//! (Architecture.md §6.3/§7.1); C3 parses, ingests as `external_documents`
//! (always `EXTERNAL_UNTRUSTED` — R0-3, taint is never lowered), then
//! deletes the spool file (§7.5's lifecycle table).
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::{EngineError, StorageEngine};

/// C7's spool payload schema. Not yet fixed by an ADR or a real
/// `neuroos-fetcher` implementation (Phase 8) — this is C3's half of the
/// contract, designed as the first real consumer; `neuroos-fetcher` must
/// match this shape when it's built.
#[derive(Debug, Deserialize, PartialEq)]
pub struct SpoolDocument {
    pub doc_id: String,
    pub url: String,
    pub text: String,
    #[serde(default)]
    pub content_type: String,
}

/// M14: a spool file is written by C7, a network-facing component
/// (Architecture.md §7.1), so its size is not something C3 can take on
/// trust: the file used to be read whole into memory with no bound, under
/// a 300 MiB RSS cap (PRD §6.2). 8 MiB is far more than any fetched
/// article's text and small enough to be harmless.
pub const MAX_SPOOL_FILE_BYTES: usize = 8 * 1024 * 1024;
/// The document body itself becomes one chunk, one embedding and one FTS
/// row, so it is bounded more tightly than the file around it.
pub const MAX_DOCUMENT_TEXT_BYTES: usize = 1024 * 1024;

/// `content_type`s whose payload is text C3 can meaningfully embed. An
/// empty value means C7 did not say, which stays allowed (the field is
/// `#[serde(default)]` and the schema predates this check).
const TEXT_CONTENT_TYPES: &[&str] = &[
    "text/",
    "application/json",
    "application/xml",
    "application/xhtml+xml",
    "application/atom+xml",
    "application/rss+xml",
    "application/markdown",
];

impl SpoolDocument {
    /// The searchable text for this document: its body, prefixed with the
    /// URL it came from when there is one. M14: `url` used to be parsed
    /// and then dropped, so nothing recorded *where* an
    /// `EXTERNAL_UNTRUSTED` document came from and no question could match
    /// on it.
    pub fn chunk_text(&self) -> String {
        if self.url.is_empty() {
            return self.text.clone();
        }
        format!("{}\n{}", self.url, self.text)
    }

    fn content_type_is_text(&self) -> bool {
        if self.content_type.is_empty() {
            return true;
        }
        // "text/plain; charset=utf-8" -> "text/plain"
        let essence = self
            .content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        TEXT_CONTENT_TYPES
            .iter()
            .any(|allowed| essence.starts_with(allowed))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("failed to read spool file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse spool file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to remove spool file {path} after ingest: {source}")]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// M14: the spool entry is a symlink, a directory or a device, not a
    /// document C7 wrote. A symlink is how a compromised C7 would get C3
    /// -- which runs with the user's own file access -- to read a file
    /// that is not its to read.
    #[error("spool entry {path} is not a regular file")]
    NotARegularFile { path: PathBuf },
    #[error("spool file {path} is {got} bytes, over the {limit}-byte limit")]
    TooLarge {
        path: PathBuf,
        got: usize,
        limit: usize,
    },
    #[error("spool file {path} carries no text to ingest")]
    NoText { path: PathBuf },
    #[error("spool file {path} has content type {content_type:?}, which is not text")]
    UnsupportedContentType { path: PathBuf, content_type: String },
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Reads and validates one spool file. M14: everything a network-facing
/// C7 controls is checked before C3 acts on it -- that the entry really is
/// a regular file (not a symlink out of the spool), that it is not large
/// enough to exhaust C3's memory budget, that the body is text C3 can
/// embed, and that there is a body at all.
pub fn parse_spool_file(path: &Path) -> Result<SpoolDocument, SpoolError> {
    // `symlink_metadata` does not follow the link, so this is a statement
    // about the spool entry itself, not about its target. Opening the path
    // afterwards still follows a link raced in between, but such a link
    // would have to point at a regular file of an allowed size inside
    // C3's Landlock ruleset (H15) to get anywhere.
    let meta = std::fs::symlink_metadata(path).map_err(|source| SpoolError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !meta.is_file() {
        return Err(SpoolError::NotARegularFile {
            path: path.to_path_buf(),
        });
    }
    let size = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    if size > MAX_SPOOL_FILE_BYTES {
        return Err(SpoolError::TooLarge {
            path: path.to_path_buf(),
            got: size,
            limit: MAX_SPOOL_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|source| SpoolError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let doc: SpoolDocument =
        serde_json::from_slice(&bytes).map_err(|source| SpoolError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if doc.text.len() > MAX_DOCUMENT_TEXT_BYTES {
        return Err(SpoolError::TooLarge {
            path: path.to_path_buf(),
            got: doc.text.len(),
            limit: MAX_DOCUMENT_TEXT_BYTES,
        });
    }
    if doc.text.trim().is_empty() {
        return Err(SpoolError::NoText {
            path: path.to_path_buf(),
        });
    }
    if !doc.content_type_is_text() {
        return Err(SpoolError::UnsupportedContentType {
            path: path.to_path_buf(),
            content_type: doc.content_type.clone(),
        });
    }
    Ok(doc)
}

/// Parses, ingests, then deletes the spool file. A parse failure leaves the
/// file in place (so a malformed/corrupt payload isn't silently lost, and
/// a re-run can retry it) rather than deleting it.
pub async fn ingest_spool_file(engine: &mut StorageEngine, path: &Path) -> Result<(), SpoolError> {
    let doc = parse_spool_file(path)?;
    engine
        .ingest_external_document(&doc, neuroos_common::now_ns())
        .await?;
    std::fs::remove_file(path).map_err(|source| SpoolError::Remove {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn is_spool_json(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

/// Ingests every `*.json` file already present in `dir` (covers files
/// written while nothing was watching; this is also what tests exercise,
/// as phases.md's "local fake spool for tests" — a tempdir with hand-written
/// JSON files, no real inotify round trip needed).
pub async fn ingest_existing(engine: &mut StorageEngine, dir: &Path) -> Result<usize, SpoolError> {
    let mut count = 0;
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|source| SpoolError::Read {
            path: dir.to_path_buf(),
            source,
        })?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| SpoolError::Read {
            path: dir.to_path_buf(),
            source,
        })?
    {
        let path = entry.path();
        if !is_spool_json(&path) {
            continue;
        }
        // M14: one unusable file used to abort the whole sweep (`?`), so
        // every file after it in directory order was never ingested --
        // and, since the sweep re-runs from the start, a single bad file
        // could block the spool indefinitely. A failure is reported and
        // the file left in place for the operator (`ingest_spool_file`'s
        // own contract); the rest of the spool still drains.
        match ingest_spool_file(engine, &path).await {
            Ok(()) => count += 1,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "skipping a spool file");
            }
        }
    }
    Ok(count)
}

/// Runs forever, ingesting each spool file as it's written (real inotify,
/// via `notify`). `engine_mutex` is shared so a caller can also serve
/// queries against the same `StorageEngine` concurrently.
pub async fn watch_forever(
    engine_mutex: &tokio::sync::Mutex<StorageEngine>,
    dir: &Path,
    health: &neuroos_health::HealthServer,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let watcher_result = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(
            event.kind,
            notify::EventKind::Create(_) | notify::EventKind::Modify(_)
        ) {
            return;
        }
        for path in event.paths {
            if is_spool_json(&path) {
                let _ = tx.send(path);
            }
        }
    });
    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "failed to set up spool watcher");
            return;
        }
    };
    if !dir.is_dir() {
        // Normal until C7 (Phase 8) is installed and has created it.
        tracing::info!(path = %dir.display(), "no fetcher spool dir; external documents disabled");
        return;
    }
    if let Err(e) = notify::Watcher::watch(&mut watcher, dir, notify::RecursiveMode::NonRecursive) {
        tracing::error!(error = %e, path = %dir.display(), "failed to watch spool dir");
        return;
    }

    // Catch anything already present before the watcher started (same
    // reasoning as ingest_existing's own doc comment).
    {
        let mut engine = engine_mutex.lock().await;
        if let Err(e) = ingest_existing(&mut engine, dir).await {
            tracing::warn!(error = %e, "failed to ingest pre-existing spool files");
            health.incr_error(crate::server::SPOOL_FAILED);
        }
    }

    while let Some(path) = rx.recv().await {
        let mut engine = engine_mutex.lock().await;
        if let Err(e) = ingest_spool_file(&mut engine, &path).await {
            tracing::warn!(error = %e, path = %path.display(), "failed to ingest spool file");
            // M3: one bad document is counted but does not make C3 sick.
            health.incr_error(crate::server::SPOOL_FAILED);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn write_doc(dir: &Path, doc_id: &str, text: &str) -> PathBuf {
        let path = dir.join(format!("{doc_id}.json"));
        std::fs::write(
            &path,
            serde_json::json!({
                "doc_id": doc_id,
                "url": format!("https://example.com/{doc_id}"),
                "text": text,
                "content_type": "text/html",
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    /// M14: C7 is a separate, network-facing component (Architecture.md
    /// §7.1); a compromised one could drop a symlink into the spool and
    /// have C3 -- which runs with the user's own file access -- read any
    /// file it points at. H15's Landlock ruleset already stops a link out
    /// of C3's allowed paths; a link to something *inside* them (the
    /// SQLite database, another spool file) was still followed.
    #[test]
    fn a_symlink_in_the_spool_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("private.json");
        std::fs::write(&secret, br#"{"doc_id":"x","url":"","text":"secret"}"#).unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let err = parse_spool_file(&link).unwrap_err();
        assert!(matches!(err, SpoolError::NotARegularFile { .. }), "{err}");
    }

    /// M14: the file was read whole into memory with no bound, so one
    /// multi-gigabyte spool file was enough to take C3 (300 MiB cap, PRD
    /// §6.2) out with it.
    #[test]
    fn an_oversize_spool_file_is_rejected_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        std::fs::write(&path, vec![b'x'; MAX_SPOOL_FILE_BYTES + 1]).unwrap();
        let err = parse_spool_file(&path).unwrap_err();
        assert!(matches!(err, SpoolError::TooLarge { .. }), "{err}");
        assert!(path.exists(), "an oversize file is left for the operator");
    }

    /// A document whose *text* is enormous is also a problem: it becomes
    /// one chunk, one embedding and one FTS row.
    #[test]
    fn an_oversize_document_text_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wordy.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "doc_id": "1",
                "url": "https://example.com/1",
                "text": "x".repeat(MAX_DOCUMENT_TEXT_BYTES + 1),
            })
            .to_string(),
        )
        .unwrap();
        let err = parse_spool_file(&path).unwrap_err();
        assert!(matches!(err, SpoolError::TooLarge { .. }), "{err}");
    }

    #[test]
    fn a_document_with_no_text_is_rejected_rather_than_embedded_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, br#"{"doc_id":"1","url":"","text":"   "}"#).unwrap();
        let err = parse_spool_file(&path).unwrap_err();
        assert!(matches!(err, SpoolError::NoText { .. }), "{err}");
    }

    /// M14: `content_type` was parsed and then ignored, so a PDF or an
    /// image C7 handed over as raw bytes would be embedded as if it were
    /// prose.
    #[test]
    fn a_non_text_content_type_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pdf.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "doc_id": "1",
                "url": "https://example.com/a.pdf",
                "text": "%PDF-1.7 ...",
                "content_type": "application/pdf",
            })
            .to_string(),
        )
        .unwrap();
        let err = parse_spool_file(&path).unwrap_err();
        assert!(
            matches!(err, SpoolError::UnsupportedContentType { .. }),
            "{err}"
        );
    }

    #[test]
    fn the_usual_text_content_types_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        for ct in [
            "",
            "text/html",
            "text/plain; charset=utf-8",
            "application/json",
            "application/xhtml+xml",
        ] {
            let path = dir.path().join("doc.json");
            std::fs::write(
                &path,
                serde_json::json!({
                    "doc_id": "1", "url": "", "text": "hello", "content_type": ct,
                })
                .to_string(),
            )
            .unwrap();
            assert!(parse_spool_file(&path).is_ok(), "{ct:?} must be accepted");
        }
    }

    /// M14: the chunk text was the document body alone, so the `url` --
    /// the one thing that says *where* an untrusted document came from,
    /// and what a "which page said that" question matches on -- was
    /// dropped.
    #[test]
    fn the_url_is_part_of_the_searchable_text() {
        let doc = SpoolDocument {
            doc_id: "1".into(),
            url: "https://example.com/quarterly".into(),
            text: "revenue was up".into(),
            content_type: "text/html".into(),
        };
        let text = doc.chunk_text();
        assert!(text.contains("https://example.com/quarterly"));
        assert!(text.contains("revenue was up"));
    }

    #[test]
    fn a_document_with_no_url_is_just_its_text() {
        let doc = SpoolDocument {
            doc_id: "1".into(),
            url: String::new(),
            text: "revenue was up".into(),
            content_type: String::new(),
        };
        assert_eq!(doc.chunk_text(), "revenue was up");
    }

    #[test]
    fn parse_spool_file_reads_the_real_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_doc(dir.path(), "42", "hello world");
        let doc = parse_spool_file(&path).unwrap();
        assert_eq!(doc.doc_id, "42");
        assert_eq!(doc.text, "hello world");
        assert_eq!(doc.content_type, "text/html");
    }

    #[test]
    fn parse_spool_file_rejects_malformed_json_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"not json at all").unwrap();
        let err = parse_spool_file(&path).unwrap_err();
        assert!(matches!(err, SpoolError::Parse { .. }));
        assert!(path.exists(), "a malformed spool file must not be deleted");
    }

    fn dev_models_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
    }

    fn dev_onnxruntime_dylib() -> std::path::PathBuf {
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
    }

    /// M14: `ingest_existing` used `?`, so the first unusable file (a
    /// truncated write, a symlink, an oversize payload) aborted the whole
    /// sweep -- every file after it in directory order was never ingested,
    /// and since the loop runs again on the next start, a single bad file
    /// could block the spool indefinitely.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn one_bad_spool_file_does_not_stop_the_others() {
        let spool_dir = tempfile::tempdir().unwrap();
        // 26 names so the bad one is early in *any* directory order.
        for c in 'a'..='z' {
            write_doc(
                spool_dir.path(),
                &format!("{c}"),
                "the quarterly revenue report",
            );
        }
        std::fs::write(spool_dir.path().join("bad.json"), b"not json at all").unwrap();

        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let mut engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        let count = ingest_existing(&mut engine, spool_dir.path())
            .await
            .unwrap();
        assert_eq!(count, 26, "every good document must be ingested");
        let left: Vec<_> = std::fs::read_dir(spool_dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("bad.json")],
            "only the bad file is left behind"
        );
    }

    /// M14: C7 re-fetching a page (a feed it polls, a retry) writes the
    /// same `doc_id` again. `upsert_entity` kept one entity, but the chunk
    /// id carries the fetch time, so every re-fetch added another copy of
    /// the whole document to LanceDB and `chunks_fts` -- and search then
    /// returned the same page several times.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn re_ingesting_a_document_replaces_it_rather_than_duplicating_it() {
        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let mut engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        for (i, text) in ["the first revenue report", "the revised revenue report"]
            .iter()
            .enumerate()
        {
            let doc = SpoolDocument {
                doc_id: "same-doc".into(),
                url: "https://example.com/report".into(),
                text: (*text).into(),
                content_type: "text/html".into(),
            };
            engine
                .ingest_external_document(&doc, 1_000 + i as u64)
                .await
                .unwrap();
        }

        let results = engine.query_hybrid("revenue report", 10).await.unwrap();
        assert_eq!(results.len(), 1, "one document, one chunk: {results:?}");
        assert!(
            results[0].text.contains("revised"),
            "the newest fetch wins: {:?}",
            results[0].text
        );
    }

    /// Live proof (P4-S06): real spool files, real embedding, real taint
    /// attachment, real deletion on success — needs the real model +
    /// onnxruntime fetched, so `#[ignore]`d like this crate's other
    /// real-download-dependent tests (`cargo test -p neuroos-storage --lib
    /// -- --ignored spool`).
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn ingest_existing_ingests_and_deletes_real_spool_files() {
        let spool_dir = tempfile::tempdir().unwrap();
        write_doc(spool_dir.path(), "1", "the quarterly revenue report");
        write_doc(spool_dir.path(), "2", "a recipe for banana bread");

        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let mut engine = StorageEngine::open(
            &sqlite_dir.path().join("meta.sqlite3"),
            lance_dir.path(),
            &dev_models_dir(),
            &dev_onnxruntime_dylib(),
        )
        .await
        .expect("real model + onnxruntime should load");

        let count = ingest_existing(&mut engine, spool_dir.path())
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            std::fs::read_dir(spool_dir.path()).unwrap().count(),
            0,
            "spool files must be deleted after successful ingest"
        );

        let results = engine.query_hybrid("revenue numbers", 1).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].text.contains("revenue"));
        assert_eq!(
            neuroos_taint::TaintFlags::from_bits_truncate(results[0].taint),
            neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED
        );
    }
}
