//! Ties the ingest filter, domain adapters, embedder and both stores
//! together into the one real ingest/query path (Architecture.md §6.2):
//! `RawTelemetryEvent` -> filter -> adapter -> SQLite (always) + LanceDB
//! (only for domains with text worth embedding).
use std::path::{Path, PathBuf};

use neuroos_proto::v1::RawTelemetryEvent;

use crate::adapters::PendingChunk;
use crate::embed::{EmbedError, Embedder};
use crate::ingest::filter::IngestFilter;
use crate::lance::{ChunkRecord, LanceError, LanceStore};
use crate::sqlite::StorageError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Sqlite(#[from] StorageError),
    #[error(transparent)]
    Lance(#[from] LanceError),
    #[error(transparent)]
    Embed(#[from] EmbedError),
}

pub struct StorageEngine {
    conn: rusqlite::Connection,
    filter: IngestFilter,
    lance: LanceStore,
    lance_dir: PathBuf,
    embedder: Embedder,
}

impl StorageEngine {
    pub async fn open(
        sqlite_path: &Path,
        lance_path: &Path,
        models_dir: &Path,
        onnxruntime_dylib: &Path,
    ) -> Result<Self, EngineError> {
        let conn = crate::sqlite::open(sqlite_path)?;
        let lance = LanceStore::open(lance_path).await?;
        let embedder = Embedder::load(models_dir, onnxruntime_dylib)?;
        Ok(Self {
            conn,
            filter: IngestFilter::new(),
            lance,
            lance_dir: lance_path.to_path_buf(),
            embedder,
        })
    }

    /// FR-STO-01/02: filters and persists one event, embedding+storing any
    /// resulting chunks. Chunks are embedded one ingest call at a time here
    /// (fine at telemetry ingest rates); a spool/replay bulk path can batch
    /// multiple chunks into one `embed()` call instead.
    pub async fn ingest(&mut self, event: &RawTelemetryEvent) -> Result<(), EngineError> {
        let pending = crate::adapters::ingest(&self.conn, &mut self.filter, event)?;
        for chunk in pending {
            self.store_chunk(chunk).await?;
        }
        Ok(())
    }

    async fn store_chunk(&mut self, chunk: PendingChunk) -> Result<(), EngineError> {
        let vector = self
            .embedder
            .embed(&[chunk.text.as_str()])?
            .into_iter()
            .next()
            .unwrap_or_default();
        let record = ChunkRecord {
            chunk_id: format!("{}-{}-{}", chunk.domain, chunk.entity_id, chunk.t_ns),
            entity_id: chunk.entity_id,
            text: chunk.text,
            vector,
            taint: chunk.taint.bits(),
            t_ns: chunk.t_ns,
            domain: chunk.domain.to_string(),
        };
        self.lance
            .insert(chunk.family, std::slice::from_ref(&record))
            .await?;
        Ok(())
    }

    /// FR-STO-05: `QueryHybridVectorText` — embed the question, search
    /// every family, merge by distance.
    pub async fn query_hybrid(
        &mut self,
        text: &str,
        top_k: usize,
    ) -> Result<Vec<crate::lance::ChunkMatch>, EngineError> {
        let vector = self
            .embedder
            .embed(&[text])?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(self.lance.query_all_families(&vector, top_k).await?)
    }

    /// FR-STO-06: `QueryFocusHistory(t, ±window)` — the deictic-snap query.
    pub fn query_focus_history(
        &self,
        t_ns: u64,
        window_ns: u64,
    ) -> Result<Option<crate::sqlite::FocusHistoryRow>, EngineError> {
        Ok(crate::sqlite::query_focus_history(
            &self.conn, t_ns, window_ns,
        )?)
    }

    /// FR-STO-08 (P4-S06): ingests one already-parsed spool document as
    /// `external_documents`, always tagged `EXTERNAL_UNTRUSTED` — taint is
    /// never lowered from here on (R0-3). Deleting the spool file on
    /// success is the caller's job (`spool::ingest_spool_file`), not this
    /// method's — this only touches SQLite/LanceDB.
    pub async fn ingest_external_document(
        &mut self,
        doc_id: &str,
        text: &str,
        fetched_at_ns: u64,
    ) -> Result<(), EngineError> {
        let entity_id = crate::sqlite::upsert_entity(
            &self.conn,
            crate::adapters::DOMAIN_EXTERNAL_DOCUMENTS,
            "document",
            doc_id,
            neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED,
            fetched_at_ns,
            true,
        )?;
        self.store_chunk(PendingChunk {
            family: "external",
            entity_id,
            domain: crate::adapters::DOMAIN_EXTERNAL_DOCUMENTS,
            taint: neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED,
            t_ns: fetched_at_ns,
            text: text.to_string(),
        })
        .await
    }

    /// FR-STO-12/FR-PRV-03: forgets everything tied to `app_id` — SQLite
    /// rows and their matching LanceDB chunks (`entity_id IN (...)`, safe
    /// to build directly since only our own `i64` entity ids are
    /// interpolated, never user text). Returns how many entities were
    /// forgotten.
    pub async fn forget_by_app(&mut self, app_id: &str) -> Result<usize, EngineError> {
        let entity_ids = crate::sqlite::forget_by_app(&self.conn, app_id)?;
        self.delete_lance_rows(&entity_ids).await?;
        Ok(entity_ids.len())
    }

    /// Same, but for everything at or after `since_ns`.
    pub async fn forget_since(&mut self, since_ns: u64) -> Result<usize, EngineError> {
        let entity_ids = crate::sqlite::forget_since(&self.conn, since_ns)?;
        self.delete_lance_rows(&entity_ids).await?;
        Ok(entity_ids.len())
    }

    /// Architecture.md §7.5's daily GC job: expires entities/focus_history
    /// past their domain's retention and purges the matching LanceDB rows.
    pub async fn gc(&mut self, now_ns: u64) -> Result<crate::lifecycle::GcSummary, EngineError> {
        let (summary, entity_ids) = crate::lifecycle::gc_expired_entities(&self.conn, now_ns)?;
        self.delete_lance_rows(&entity_ids).await?;
        Ok(summary)
    }

    /// Architecture.md §7.5's 6-hourly backup job.
    pub async fn backup(
        &self,
        backups_root: &Path,
        label: &str,
        keep: usize,
    ) -> Result<PathBuf, crate::lifecycle::BackupError> {
        crate::lifecycle::backup(&self.conn, &self.lance_dir, backups_root, label, keep).await
    }

    async fn delete_lance_rows(&self, entity_ids: &[i64]) -> Result<(), EngineError> {
        if entity_ids.is_empty() {
            return Ok(());
        }
        let ids = entity_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.lance
            .delete_all_families(&format!("entity_id IN ({ids})"))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_proto::v1::raw_telemetry_event::Payload;
    use neuroos_proto::v1::window_event::Kind;
    use neuroos_proto::v1::{ToplevelState, WindowEvent, WindowOpened, WindowStateChanged};

    fn dev_models_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
    }

    fn dev_onnxruntime_dylib() -> std::path::PathBuf {
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
    }

    fn window_event(toplevel_id: u64, at_ns: u64, kind: Kind) -> RawTelemetryEvent {
        RawTelemetryEvent {
            observed_at_ns: at_ns,
            source: "wayland_cosmic".into(),
            payload: Some(Payload::Window(WindowEvent {
                toplevel_id,
                kind: Some(kind),
            })),
        }
    }

    /// Live proof (P4-S02/S03/S04 integration): a real focus session, real
    /// embedding, real LanceDB insert, then a real hybrid query finds it —
    /// needs the real model + onnxruntime fetched, so `#[ignore]`d like this
    /// crate's other real-download-dependent tests (`cargo test -p
    /// neuroos-storage --lib -- --ignored engine`).
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn full_pipeline_ingest_then_hybrid_query_finds_the_window() {
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

        engine
            .ingest(&window_event(
                1,
                0,
                Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "quarterly revenue dashboard".into(),
                    pid: 0,
                    pid_known: false,
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                0,
                Kind::StateChanged(WindowStateChanged {
                    states: vec![ToplevelState::Activated as i32],
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                6_000_000_000,
                Kind::StateChanged(WindowStateChanged { states: vec![] }),
            ))
            .await
            .unwrap();

        let results = engine.query_hybrid("revenue numbers", 3).await.unwrap();
        assert!(
            results.iter().any(|r| r.text.contains("revenue")),
            "expected the ingested revenue-dashboard chunk among results: {results:?}"
        );
    }

    /// Live proof (P4-S09): a real ingested+embedded document is actually
    /// gone from both SQLite and LanceDB after forget_by_app — needs the
    /// real model + onnxruntime fetched, so `#[ignore]`d like this crate's
    /// other real-download-dependent tests.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn forget_by_app_removes_the_entity_and_its_lance_chunk() {
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

        engine
            .ingest(&window_event(
                1,
                0,
                Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "quarterly revenue dashboard".into(),
                    pid: 0,
                    pid_known: false,
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                0,
                Kind::StateChanged(WindowStateChanged {
                    states: vec![ToplevelState::Activated as i32],
                }),
            ))
            .await
            .unwrap();
        engine
            .ingest(&window_event(
                1,
                6_000_000_000,
                Kind::StateChanged(WindowStateChanged { states: vec![] }),
            ))
            .await
            .unwrap();
        assert!(!engine.query_hybrid("revenue", 1).await.unwrap().is_empty());

        let forgotten = engine.forget_by_app("org.mozilla.firefox").await.unwrap();
        assert_eq!(forgotten, 1);
        assert!(
            engine.query_hybrid("revenue", 1).await.unwrap().is_empty(),
            "the forgotten chunk must no longer be found by vector search"
        );
        let entities: i64 = engine
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE label = 'org.mozilla.firefox'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entities, 0);
    }
}
