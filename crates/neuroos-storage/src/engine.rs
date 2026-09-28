//! Ties the ingest filter, domain adapters, embedder and both stores
//! together into the one real ingest/query path (Architecture.md §6.2):
//! `RawTelemetryEvent` -> filter -> adapter -> SQLite (always) + LanceDB
//! (only for domains with text worth embedding).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

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
    lance: Arc<LanceStore>,
    lance_dir: PathBuf,
    sqlite_path: PathBuf,
    models_dir: PathBuf,
    onnxruntime_dylib: PathBuf,
    /// BUG-001: `Arc<Mutex<_>>`, not a plain field, so `embed_blocking` can
    /// move a handle to it into `spawn_blocking` — see that fn's doc
    /// comment for why. The `std::sync::Mutex` here is uncontended in
    /// practice (the *async* `Mutex<StorageEngine>` in `server.rs` already
    /// serializes every request before this is ever touched); it exists
    /// only so the embedder can be shared with the blocking-pool thread.
    embedder: Arc<StdMutex<Embedder>>,
}

impl StorageEngine {
    pub async fn open(
        sqlite_path: &Path,
        lance_path: &Path,
        models_dir: &Path,
        onnxruntime_dylib: &Path,
    ) -> Result<Self, EngineError> {
        let conn = crate::sqlite::open(sqlite_path)?;
        let lance = Arc::new(LanceStore::open(lance_path).await?);
        let embedder = Arc::new(StdMutex::new(Embedder::load(
            models_dir,
            onnxruntime_dylib,
        )?));
        let engine = Self {
            conn,
            filter: IngestFilter::new(),
            lance,
            lance_dir: lance_path.to_path_buf(),
            sqlite_path: sqlite_path.to_path_buf(),
            models_dir: models_dir.to_path_buf(),
            onnxruntime_dylib: onnxruntime_dylib.to_path_buf(),
            embedder,
        };
        engine.spawn_reindex_for_changed_families();
        Ok(engine)
    }

    /// FR-STO-11: on open, compares each family's stored
    /// `index_meta.embedding_model_id` against the embedder just loaded;
    /// any mismatch gets a background task that re-embeds every existing
    /// chunk with its own, independently-loaded `Embedder` and writes the
    /// new vectors back via `LanceStore::upsert_vectors` — `self.embedder`
    /// (used by `ingest`/`query_hybrid`) is never touched by it, so
    /// interactive ingest/query traffic is never blocked by a re-index.
    fn spawn_reindex_for_changed_families(&self) {
        let current_model_id = Embedder::model_id();
        for family in crate::lance::FAMILIES {
            let stale = match crate::sqlite::get_index_meta(&self.conn, family) {
                Ok(Some(meta)) => meta.embedding_model_id != current_model_id,
                // Never indexed yet -- nothing to reindex; ingest records
                // index_meta going forward so the *next* model change has
                // something to compare against.
                Ok(None) => false,
                Err(err) => {
                    tracing::warn!(family, error = %err, "FR-STO-11: failed to read index_meta, skipping reindex check");
                    continue;
                }
            };
            if !stale {
                continue;
            }
            let lance = Arc::clone(&self.lance);
            let sqlite_path = self.sqlite_path.clone();
            let models_dir = self.models_dir.clone();
            let onnxruntime_dylib = self.onnxruntime_dylib.clone();
            let family = family.to_string();
            tokio::spawn(async move {
                if let Err(err) = reindex_family(
                    &lance,
                    &sqlite_path,
                    &models_dir,
                    &onnxruntime_dylib,
                    &family,
                )
                .await
                {
                    tracing::error!(family, error = %err, "FR-STO-11: background reindex failed");
                }
            });
        }
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
        let vector = embed_blocking(&self.embedder, vec![chunk.text.clone()])
            .await?
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
        // FR-STO-11: records which model produced this family's vectors,
        // so a *future* `Embedder::model_id()` change has something to
        // compare against on the next `open()`.
        crate::sqlite::upsert_index_meta(
            &self.conn,
            chunk.family,
            Embedder::model_id(),
            crate::lance::EMBEDDING_DIM as i64,
            if self.lance.is_promoted(chunk.family) {
                "hnsw"
            } else {
                "flat"
            },
            0.0,
            neuroos_common::now_ns() as i64,
        )?;
        Ok(())
    }

    /// FR-STO-05: `QueryHybridVectorText` — embed the question, search
    /// every family, merge by distance.
    pub async fn query_hybrid(
        &mut self,
        text: &str,
        top_k: usize,
    ) -> Result<Vec<crate::lance::ChunkMatch>, EngineError> {
        let vector = embed_blocking(&self.embedder, vec![text.to_string()])
            .await?
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

    /// FR-KNO-10: entities the Python cold worker builds co-occurrence
    /// candidates from. `since_ns = 0` means every entity.
    pub fn list_entities(
        &self,
        since_ns: u64,
    ) -> Result<Vec<crate::sqlite::EntityRow>, EngineError> {
        Ok(crate::sqlite::list_entities(&self.conn, since_ns)?)
    }

    /// FR-KNO-10: every edge, for the cold worker's APPNP propagation input.
    pub fn list_edges(&self) -> Result<Vec<crate::sqlite::EdgeRow>, EngineError> {
        Ok(crate::sqlite::list_edges(&self.conn)?)
    }

    /// FR-KNO-10: create/reinforce one edge the cold worker computed.
    pub fn upsert_edge(&self, edge: &crate::sqlite::EdgeRow) -> Result<(), EngineError> {
        Ok(crate::sqlite::upsert_edge(&self.conn, edge)?)
    }

    /// FR-KNO-10: "prune unreinforced edges after 72h" -- returns how many
    /// hypothesis edges were deleted.
    pub fn prune_hypothesis_edges(&self, older_than_ns: u64) -> Result<u64, EngineError> {
        Ok(crate::sqlite::prune_hypothesis_edges(
            &self.conn,
            older_than_ns,
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
        // BUG-002: ride the existing 6-hourly backup cadence to keep
        // fragment count (and therefore query latency) down -- see
        // `LanceStore::compact_all_families`'s doc comment. Best-effort,
        // same policy as HNSW promotion: a compaction failure (e.g. a
        // concurrent writer) shouldn't block the backup itself, which is
        // more safety-critical than this maintenance step.
        if let Err(err) = self.lance.compact_all_families().await {
            tracing::warn!(error = %err, "BUG-002: LanceDB compaction failed before backup, continuing");
        }
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

/// BUG-001: `Embedder::embed` runs real, synchronous, CPU-bound ONNX
/// inference (~10ms/call per `embed.rs`'s own doc comment). `StorageEngine`
/// is reached only through one `Arc<Mutex<StorageEngine>>` (`server.rs`),
/// so every request is already serialized by design -- the bug was never
/// that serialization, it's that running that ~10ms of real CPU work
/// *inline* on the async runtime's own thread freezes the whole reactor for
/// that duration: no other task (another connection's cheap SQL-only
/// query, the accept loop, a timer) can make any progress at all until it
/// returns, not just callers waiting on the same mutex. `ort::Session`
/// (`fastembed::TextEmbedding`'s own field, wrapping it) is `unsafe impl
/// Send + Sync` as of `ort` 2.0.0-rc.13 (confirmed directly in its vendored
/// source, `session/mod.rs`), so `Embedder` is `Send` and Tokio's own
/// documented fix for CPU-bound work applies directly: hand it to
/// `spawn_blocking`'s dedicated thread pool and `.await` the result, which
/// is a real yield point -- the runtime keeps servicing everything else
/// while the real inference call runs elsewhere.
async fn embed_blocking(
    embedder: &Arc<StdMutex<Embedder>>,
    texts: Vec<String>,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let embedder = Arc::clone(embedder);
    tokio::task::spawn_blocking(move || {
        let mut embedder = embedder.lock().unwrap_or_else(|e| e.into_inner());
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        embedder.embed(&refs)
    })
    .await
    // rules.md §5: no naked `.unwrap()/.expect()`. A `JoinError` here means
    // the blocking closure itself panicked (it never calls `.await`, so it
    // can't be cancelled) -- propagate that original panic rather than
    // manufacturing a new message, the standard `spawn_blocking` idiom.
    .unwrap_or_else(|join_err| std::panic::resume_unwind(join_err.into_panic()))
}

/// FR-STO-11's actual re-index work: loads its own `Embedder` (a second,
/// independent model load — never the caller's interactive one), re-embeds
/// every chunk currently in `family` with it, writes the new vectors back
/// via `LanceStore::upsert_vectors` (an upsert on `chunk_id`, so a query
/// racing this sees either the pre- or post-reindex vector, never a gap),
/// then records the new `embedding_model_id` so the mismatch that
/// triggered this clears. A free function (not a `StorageEngine` method)
/// so `spawn_reindex_for_changed_families` can run it in a `tokio::spawn`
/// task that outlives the borrow of `&self`.
async fn reindex_family(
    lance: &LanceStore,
    sqlite_path: &Path,
    models_dir: &Path,
    onnxruntime_dylib: &Path,
    family: &str,
) -> Result<(), EngineError> {
    let mut embedder = Embedder::load(models_dir, onnxruntime_dylib)?;
    let rows = lance.all_chunks(family).await?;
    if rows.is_empty() {
        return Ok(());
    }
    // BUG-001: same fix as `embed_blocking` -- this background task's own
    // embedder is never shared, so the whole per-chunk loop can move into
    // one `spawn_blocking` call instead of freezing the async runtime for
    // however long re-embedding every chunk in `family` takes.
    let records = tokio::task::spawn_blocking(move || -> Result<Vec<ChunkRecord>, EmbedError> {
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let vector = embedder
                .embed(&[row.text.as_str()])?
                .into_iter()
                .next()
                .unwrap_or_default();
            records.push(ChunkRecord {
                chunk_id: row.chunk_id,
                entity_id: row.entity_id,
                text: row.text,
                vector,
                taint: row.taint,
                t_ns: row.t_ns,
                domain: row.domain,
            });
        }
        Ok(records)
    })
    .await
    .unwrap_or_else(|join_err| std::panic::resume_unwind(join_err.into_panic()))?;
    let reindexed = records.len();
    lance.upsert_vectors(family, &records).await?;

    let conn = crate::sqlite::open(sqlite_path)?;
    crate::sqlite::upsert_index_meta(
        &conn,
        family,
        Embedder::model_id(),
        crate::lance::EMBEDDING_DIM as i64,
        if lance.is_promoted(family) {
            "hnsw"
        } else {
            "flat"
        },
        0.0,
        neuroos_common::now_ns() as i64,
    )?;
    tracing::info!(
        family,
        rows = reindexed,
        "FR-STO-11: background re-index complete"
    );
    Ok(())
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

    /// Live proof (P4-S09 / phases.md §7.4 "Forget verified (rows,
    /// vectors, next backup)"): the third leg -- a backup taken *after* a
    /// forget must not resurrect the forgotten row. Needs the real model +
    /// onnxruntime fetched, so `#[ignore]`d like this crate's other
    /// real-download-dependent tests.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn forgotten_rows_do_not_reappear_in_the_next_backup() {
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

        let forgotten = engine.forget_by_app("org.mozilla.firefox").await.unwrap();
        assert_eq!(forgotten, 1);

        let backups_root = tempfile::tempdir().unwrap();
        let backup_dest = crate::lifecycle::backup(
            &engine.conn,
            &engine.lance_dir,
            backups_root.path(),
            "after-forget",
            8,
        )
        .await
        .unwrap();

        let restored = crate::sqlite::open(&backup_dest.join("meta.sqlite3")).unwrap();
        let entities: i64 = restored
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE label = 'org.mozilla.firefox'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            entities, 0,
            "a backup taken after forget must not contain the forgotten entity"
        );
    }

    /// Live proof (P4-S08/FR-STO-11): ingesting records `index_meta`, and
    /// running the background re-index function directly (rather than
    /// waiting for a spawned task) re-embeds the existing chunk in place
    /// — still findable afterward, not duplicated, `index_meta` still
    /// current — needs the real model + onnxruntime fetched, so
    /// `#[ignore]`d like this crate's other real-download-dependent tests.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn reindex_family_re_embeds_in_place_without_duplicating() {
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

        let meta = crate::sqlite::get_index_meta(&engine.conn, "attention")
            .unwrap()
            .expect("ingest should have recorded index_meta");
        assert_eq!(meta.embedding_model_id, Embedder::model_id());

        reindex_family(
            &engine.lance,
            &engine.sqlite_path,
            &engine.models_dir,
            &engine.onnxruntime_dylib,
            "attention",
        )
        .await
        .unwrap();

        assert_eq!(
            engine.lance.all_chunks("attention").await.unwrap().len(),
            1,
            "re-index must upsert in place, not duplicate rows"
        );
        assert!(
            !engine.query_hybrid("revenue", 1).await.unwrap().is_empty(),
            "the re-embedded chunk must still be findable"
        );
    }
}
