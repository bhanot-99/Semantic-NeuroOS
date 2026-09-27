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
    #[error(transparent)]
    Engine(#[from] EngineError),
}

pub fn parse_spool_file(path: &Path) -> Result<SpoolDocument, SpoolError> {
    let bytes = std::fs::read(path).map_err(|source| SpoolError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| SpoolError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Parses, ingests, then deletes the spool file. A parse failure leaves the
/// file in place (so a malformed/corrupt payload isn't silently lost, and
/// a re-run can retry it) rather than deleting it.
pub async fn ingest_spool_file(engine: &mut StorageEngine, path: &Path) -> Result<(), SpoolError> {
    let doc = parse_spool_file(path)?;
    engine
        .ingest_external_document(&doc.doc_id, &doc.text, neuroos_common::now_ns())
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
        if is_spool_json(&path) {
            ingest_spool_file(engine, &path).await?;
            count += 1;
        }
    }
    Ok(count)
}

/// Runs forever, ingesting each spool file as it's written (real inotify,
/// via `notify`). `engine_mutex` is shared so a caller can also serve
/// queries against the same `StorageEngine` concurrently.
pub async fn watch_forever(engine_mutex: &tokio::sync::Mutex<StorageEngine>, dir: &Path) {
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
        }
    }

    while let Some(path) = rx.recv().await {
        let mut engine = engine_mutex.lock().await;
        if let Err(e) = ingest_spool_file(&mut engine, &path).await {
            tracing::warn!(error = %e, path = %path.display(), "failed to ingest spool file");
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
