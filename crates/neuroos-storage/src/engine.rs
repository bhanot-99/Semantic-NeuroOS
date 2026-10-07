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
    /// M15: the embedder returned no vector for a chunk's text. Treated as
    /// a failed ingest -- an empty vector is not a valid embedding.
    #[error("the embedder produced no vector for this chunk")]
    NoEmbedding,
}

pub struct StorageEngine {
    conn: rusqlite::Connection,
    filter: IngestFilter,
    lance: Arc<LanceStore>,
    lance_dir: PathBuf,
    sqlite_path: PathBuf,
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
        let mut engine = Self {
            conn,
            filter: IngestFilter::new(),
            lance,
            lance_dir: lance_path.to_path_buf(),
            sqlite_path: sqlite_path.to_path_buf(),
            embedder,
        };
        engine.warm_query_path().await;
        engine.spawn_reindex_for_changed_families();
        Ok(engine)
    }

    /// M22: runs one throwaway hybrid query so the *first real* one is not
    /// also the cold one. ONNX Runtime's first inference allocates its
    /// arenas and finishes optimizing the graph, LanceDB opens each
    /// family's dataset, and SQLite prepares the FTS statement -- measured
    /// at 71 ms cold against 43 ms warm (debug build), i.e. 71 % of C5a's
    /// 100 ms storage deadline before any disk cache misses, which is what
    /// made `ask_end_to_end` flake right after a clean rebuild. `open()`
    /// already spends ~700 ms loading the model, so this belongs here,
    /// where healthd has not yet been told C3 is up.
    ///
    /// Best-effort by design (rules.md §5.6): a store that cannot answer
    /// this -- a brand new one with no datasets yet -- must still open.
    async fn warm_query_path(&mut self) {
        if let Err(err) = self.query_hybrid("warm up the query path", 1).await {
            tracing::debug!(error = %err, "query path warm-up did not complete");
        }
    }

    /// FR-STO-11: on open, compares each family's stored
    /// `index_meta.embedding_model_id` against the embedder just loaded;
    /// any mismatch gets a background task that re-embeds every existing
    /// chunk with *this* engine's `Embedder` (M16: it used to load a
    /// second model, doubling resident model memory) and writes the
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
            // M16: the engine's own embedder, not a second model load.
            let embedder = Arc::clone(&self.embedder);
            let family = family.to_string();
            tokio::spawn(async move {
                if let Err(err) = reindex_family(&lance, &sqlite_path, &embedder, &family).await {
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
        // M15: `unwrap_or_default()` here turned "the embedder returned
        // nothing" into an empty vector, which then panicked inside
        // Arrow's FixedSizeList builder (see `lance::records_to_batch`).
        // A missing embedding is a failed ingest, not a zero-width vector.
        let vector = embed_blocking(&self.embedder, vec![chunk.text.clone()])
            .await?
            .into_iter()
            .next()
            .ok_or(EngineError::NoEmbedding)?;
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
        // ADR-0015: ingest is what makes a family slow to scan, so it is
        // also what should trigger C3 measuring its own search latency --
        // off-task, and only until the family is indexed. Without this the
        // first real question after a burst is the one that discovers the
        // store got slow, and pays for it (BUG-006).
        self.lance.maybe_spawn_latency_probe(chunk.family);
        // M15: the vector is in LanceDB now; the keyword row and
        // `index_meta` (FR-STO-11: which model produced this family's
        // vectors, so a *future* `Embedder::model_id()` change has
        // something to compare against on the next `open()`) go in as one
        // transaction. Two stores cannot share a transaction, so if the
        // SQLite half fails the vector is deleted again rather than left
        // behind as a hit no keyword row or index_meta entry knows about.
        let meta = crate::sqlite::insert_chunk_with_index_meta(
            &self.conn,
            &crate::sqlite::FtsChunk {
                chunk_id: &record.chunk_id,
                entity_id: record.entity_id,
                domain: &record.domain,
                taint: record.taint,
                t_ns: record.t_ns,
                text: &record.text,
            },
            &crate::sqlite::IndexMetaWrite {
                collection: chunk.family,
                embedding_model_id: Embedder::model_id(),
                dim: crate::lance::EMBEDDING_DIM as i64,
                index_kind: if self.lance.is_promoted(chunk.family) {
                    "hnsw"
                } else {
                    "flat"
                },
                p99_ms: 0.0,
                updated_ns: neuroos_common::now_ns() as i64,
            },
        );
        if let Err(err) = meta {
            // `chunk_id` is our own generated `{domain}-{entity_id}-{t_ns}`
            // (never user text), so interpolating it is safe.
            if let Err(cleanup) = self
                .lance
                .delete(chunk.family, &format!("chunk_id = '{}'", record.chunk_id))
                .await
            {
                tracing::error!(error = %cleanup, "failed to roll back an orphaned vector");
            }
            return Err(err.into());
        }
        Ok(())
    }

    /// FR-STO-05: `QueryHybridVectorText` — embed the question, search
    /// every family, merge by distance.
    pub async fn query_hybrid(
        &mut self,
        text: &str,
        top_k: usize,
    ) -> Result<Vec<crate::lance::ChunkMatch>, EngineError> {
        // bge-*-v1.5's own retrieval instruction for short queries against
        // passages (BAAI model card); chunks are embedded without it.
        let query = format!("{BGE_QUERY_INSTRUCTION}{text}");
        let vector = embed_blocking(&self.embedder, vec![query])
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let dense = self
            .lance
            .query_all_families(&vector, top_k * CANDIDATE_OVERFETCH)
            .await?;
        let keyword =
            crate::sqlite::search_chunks_fts(&self.conn, text, top_k * CANDIDATE_OVERFETCH)?;
        Ok(fuse_rrf(dense, keyword, text, top_k))
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

    /// BUG-007: `QueryActivity`: focused titles by total dwell, and media
    /// started, within `[since_ns, until_ns]` (0 = unbounded).
    pub fn query_activity(
        &self,
        since_ns: u64,
        until_ns: u64,
        limit: usize,
    ) -> Result<
        (
            Vec<crate::sqlite::ActivityRow>,
            Vec<crate::sqlite::ActivityRow>,
        ),
        EngineError,
    > {
        Ok((
            crate::sqlite::activity_windows(&self.conn, since_ns, until_ns, limit)?,
            crate::sqlite::activity_media(&self.conn, since_ns, until_ns, limit)?,
        ))
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
        doc: &crate::spool::SpoolDocument,
        fetched_at_ns: u64,
    ) -> Result<(), EngineError> {
        // M14: C7 re-fetches (a polled feed, a retry), and a chunk id
        // carries its fetch time, so every re-fetch used to add another
        // full copy of the document -- search then returned the same page
        // several times over. The newest fetch replaces the older ones.
        // Checked before the upsert, so a first fetch doesn't pay for a
        // no-op delete (which would still cost a LanceDB dataset version).
        let already_fetched = crate::sqlite::entity_exists(
            &self.conn,
            crate::adapters::DOMAIN_EXTERNAL_DOCUMENTS,
            &doc.doc_id,
        )?;
        let entity_id = crate::sqlite::upsert_entity(
            &self.conn,
            crate::adapters::DOMAIN_EXTERNAL_DOCUMENTS,
            "document",
            &doc.doc_id,
            neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED,
            fetched_at_ns,
            true,
        )?;
        if already_fetched {
            self.lance
                .delete("external", &format!("entity_id = {entity_id}"))
                .await?;
            crate::sqlite::delete_chunks_fts(&self.conn, &[entity_id])?;
        }
        self.store_chunk(PendingChunk {
            family: "external",
            entity_id,
            domain: crate::adapters::DOMAIN_EXTERNAL_DOCUMENTS,
            taint: neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED,
            t_ns: fetched_at_ns,
            text: doc.chunk_text(),
        })
        .await
    }

    /// FR-STO-12/FR-PRV-03: forgets everything tied to `app_id` — SQLite
    /// rows and their matching LanceDB chunks — and returns how many
    /// entities were forgotten. LanceDB goes first, while the entity ids
    /// are still in SQLite, so a forget that fails part-way is retryable;
    /// the purge then removes the rows physically (H3).
    pub async fn forget_by_app(&mut self, app_id: &str) -> Result<usize, EngineError> {
        let entity_ids = crate::sqlite::entity_ids_for_app(&self.conn, app_id)?;
        self.delete_lance(entity_predicate(&entity_ids)).await?;
        let forgotten = crate::sqlite::forget_by_app(&self.conn, app_id)?;
        self.lance.purge_deleted().await?;
        Ok(forgotten.len())
    }

    /// Same, but for everything recorded at or after `since_ns`: entities
    /// created in the window, and every chunk from the window whichever
    /// entity it belongs to (H2).
    pub async fn forget_since(&mut self, since_ns: u64) -> Result<usize, EngineError> {
        let entity_ids = crate::sqlite::entity_ids_created_since(&self.conn, since_ns)?;
        let recent = format!("t_ns >= {since_ns}");
        let predicate = match entity_predicate(&entity_ids) {
            Some(by_entity) => format!("{by_entity} OR {recent}"),
            None => recent,
        };
        self.delete_lance(Some(predicate)).await?;
        let forgotten = crate::sqlite::forget_since(&self.conn, since_ns)?;
        self.lance.purge_deleted().await?;
        Ok(forgotten.len())
    }

    /// Architecture.md §7.5's daily GC job: expires rows past their
    /// domain's retention in SQLite, then the matching LanceDB chunks
    /// (every chunk of the domain older than its cutoff, plus those of
    /// expired entities), then purges them physically. Re-running after a
    /// partial failure finishes the job: the cutoffs are recomputed.
    pub async fn gc(&mut self, now_ns: u64) -> Result<crate::lifecycle::GcSummary, EngineError> {
        let outcome = crate::lifecycle::gc_expired_entities(&self.conn, now_ns)?;
        let mut clauses: Vec<String> = outcome
            .chunk_cutoffs
            .iter()
            .map(|(domain, cutoff)| format!("(domain = '{domain}' AND t_ns < {cutoff})"))
            .collect();
        clauses.extend(entity_predicate(&outcome.entity_ids));
        if !clauses.is_empty() {
            self.delete_lance(Some(clauses.join(" OR "))).await?;
        }
        self.lance.purge_deleted().await?;
        Ok(outcome.summary)
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

    async fn delete_lance(&self, predicate: Option<String>) -> Result<(), EngineError> {
        if let Some(predicate) = predicate {
            self.lance.delete_all_families(&predicate).await?;
        }
        Ok(())
    }
}

/// `entity_id IN (...)` over our own `i64` ids (never user text, so safe
/// to interpolate), or `None` when there are none.
fn entity_predicate(entity_ids: &[i64]) -> Option<String> {
    if entity_ids.is_empty() {
        return None;
    }
    let ids = entity_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("entity_id IN ({ids})"))
}

/// BAAI's query-side instruction for bge-*-v1.5 retrieval.
const BGE_QUERY_INSTRUCTION: &str = "Represent this sentence for searching relevant passages: ";

/// Candidates pulled from each retriever per requested result: telemetry
/// repeats the same window title many times, so duplicates are dropped
/// before fusion and each list still needs enough distinct texts.
const CANDIDATE_OVERFETCH: usize = 4;

/// Reciprocal-rank-fusion constant (Cormack et al., 2009; k = 60 is the
/// standard choice and LanceDB's own default).
const RRF_K: f32 = 60.0;

/// Crude suffix stripping so "tests"/"installed"/"watching" match "test"/
/// "install"/"watch" inside a chunk; FTS5's own porter stemmer already
/// handles the BM25 side, this only feeds [`terms_matched`].
fn stem(term: &str) -> &str {
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(root) = term.strip_suffix(suffix)
            && root.len() >= 3
        {
            return root;
        }
    }
    term
}

/// How many distinct question terms occur in `text` (BUG-007(b)'s
/// keyword evidence strength): one shared generic word ("test", "watch")
/// is weak; two or more ("lush" + "pop") identify the activity.
fn terms_matched(terms: &[String], text: &str) -> f32 {
    let lower = text.to_lowercase();
    terms.iter().filter(|t| lower.contains(stem(t))).count() as f32
}

/// BUG-007(b): fuses dense (LanceDB) and keyword (FTS5/BM25) results by
/// reciprocal rank fusion, after dropping repeated texts within each list.
/// Every keyword hit's `keyword_score` is the number of distinct question
/// terms it contains; a keyword-only chunk has distance 2.0 (unknown).
/// Returns at most `top_k`, best first.
fn fuse_rrf(
    dense: Vec<crate::lance::ChunkMatch>,
    keyword: Vec<crate::sqlite::FtsHit>,
    question: &str,
    top_k: usize,
) -> Vec<crate::lance::ChunkMatch> {
    let terms = crate::sqlite::fts_terms(question);
    use std::collections::HashMap;
    let mut fused: Vec<(f32, crate::lance::ChunkMatch)> = Vec::new();
    let mut by_text: HashMap<String, usize> = HashMap::new();
    for (rank, m) in dense.into_iter().enumerate() {
        // M9 / Architecture.md §7.4: dropping a duplicate must not drop its
        // taint with it -- `taint(output) = union(taint(inputs))`, and R0-3
        // forbids any path that lowers it.
        if let Some(&i) = by_text.get(&m.text) {
            fused[i].1.taint |= m.taint;
            continue;
        }
        by_text.insert(m.text.clone(), fused.len());
        fused.push((1.0 / (RRF_K + rank as f32 + 1.0), m));
    }
    let mut seen_keyword = std::collections::HashSet::new();
    let mut keyword_rank = 0usize;
    for hit in keyword {
        if !seen_keyword.insert(hit.text.clone()) {
            // M9: same rule as the dense loop above -- a repeated text is
            // not a reason to forget where one of its copies came from.
            if let Some(&i) = by_text.get(&hit.text) {
                fused[i].1.taint |= hit.taint;
            }
            continue;
        }
        let rrf = 1.0 / (RRF_K + keyword_rank as f32 + 1.0);
        keyword_rank += 1;
        let searchable = format!("{} {}", hit.text, crate::sqlite::search_aliases(&hit.text));
        let score = terms_matched(&terms, &searchable);
        match by_text.get(&hit.text) {
            Some(&i) => {
                fused[i].0 += rrf;
                fused[i].1.keyword_score = score;
                // M9: the two hits are the same text from two indexes, so
                // the surviving chunk carries both provenances.
                fused[i].1.taint |= hit.taint;
            }
            None => {
                by_text.insert(hit.text.clone(), fused.len());
                fused.push((
                    rrf,
                    crate::lance::ChunkMatch {
                        chunk_id: hit.chunk_id,
                        entity_id: hit.entity_id,
                        text: hit.text,
                        taint: hit.taint,
                        t_ns: hit.t_ns,
                        domain: hit.domain,
                        distance: 2.0,
                        keyword_score: score,
                    },
                ));
            }
        }
    }
    fused.sort_by(|a, b| b.0.total_cmp(&a.0));
    fused.into_iter().take(top_k).map(|(_, m)| m).collect()
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
/// Best-effort rendering of a panic payload (C5). `Box<dyn Any>` only
/// carries a readable message for the `&str`/`String` payloads that
/// `panic!`/`assert!` produce; anything else reports its absence rather
/// than pretending to know.
fn panic_message(join_err: tokio::task::JoinError) -> String {
    let payload = join_err.into_panic();
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic payload was not a string".to_string()
    }
}

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
    // C5 / rules.md §5: no `panic!`, `unreachable!` or `resume_unwind` in
    // non-test code. A `JoinError` here means the blocking closure itself
    // panicked (it never awaits, so it cannot have been cancelled). This
    // used to `resume_unwind` the original payload, which unwound the
    // caller's task and took down whatever request was in flight; the panic
    // message is preserved in the error instead, so one bad embed degrades
    // one request.
    .unwrap_or_else(|join_err| Err(EmbedError::WorkerPanicked(panic_message(join_err))))
}

/// How many chunks one re-index batch re-embeds before releasing the
/// embedder again (M16). The lock is uncontended in the steady state (the
/// async `Mutex<StorageEngine>` in `server.rs` serializes requests before
/// it), so this exists purely so a re-index of a large family cannot hold
/// the embedder for minutes while queries queue behind it.
pub(crate) const REINDEX_BATCH_SIZE: usize = 32;

/// FR-STO-11's actual re-index work: re-embeds
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
    embedder: &Arc<StdMutex<Embedder>>,
    family: &str,
) -> Result<(), EngineError> {
    let rows = lance.all_chunks(family).await?;
    if rows.is_empty() {
        return Ok(());
    }
    // M16: the engine's embedder, in `REINDEX_BATCH_SIZE` batches. BUG-001
    // still applies to each batch (`embed_blocking` keeps the real
    // inference call off the async runtime), but the lock is released
    // between batches, so re-embedding a large family cannot hold the
    // embedder -- and with it every query -- for minutes at a time.
    let mut records = Vec::with_capacity(rows.len());
    for batch in rows.chunks(REINDEX_BATCH_SIZE) {
        let texts: Vec<String> = batch.iter().map(|row| row.text.clone()).collect();
        let vectors = embed_blocking(embedder, texts).await?;
        if vectors.len() != batch.len() {
            // One vector per text is `embed`'s contract; anything else
            // would silently mis-pair vectors with chunk ids.
            return Err(EngineError::NoEmbedding);
        }
        for (row, vector) in batch.iter().zip(vectors) {
            if vector.len() != crate::lance::EMBEDDING_DIM as usize {
                // M15: `upsert_vectors` would otherwise panic in Arrow.
                return Err(EngineError::NoEmbedding);
            }
            records.push(ChunkRecord {
                chunk_id: row.chunk_id.clone(),
                entity_id: row.entity_id,
                text: row.text.clone(),
                vector,
                taint: row.taint,
                t_ns: row.t_ns,
                domain: row.domain.clone(),
            });
        }
    }
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
    use crate::test_support::{dev_models_dir, dev_onnxruntime_dylib};
    use neuroos_proto::v1::raw_telemetry_event::Payload;
    use neuroos_proto::v1::window_event::Kind;
    use neuroos_proto::v1::{ToplevelState, WindowEvent, WindowOpened, WindowStateChanged};

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

    /// C4: the open → activate → deactivate trio that four tests used
    /// verbatim to get exactly one promoted, searchable focus session into
    /// the store. A 6 s dwell clears the ingest filter's minimum, and the
    /// title is what the `query_hybrid("revenue numbers", ..)` assertions
    /// match on.
    async fn ingest_one_focus_session(engine: &mut StorageEngine) {
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

        ingest_one_focus_session(&mut engine).await;

        let results = engine.query_hybrid("revenue numbers", 3).await.unwrap();
        assert!(
            results.iter().any(|r| r.text.contains("revenue")),
            "expected the ingested revenue-dashboard chunk among results: {results:?}"
        );
    }

    /// M22: the first query after `open()` is what `ask()` pays on the
    /// first question after a C3 start, and it used to be the *cold* one:
    /// 71 ms against C5a's 100 ms storage deadline in a debug build,
    /// before any disk-cache misses -- which is what made
    /// `ask_end_to_end` flake right after a clean rebuild.
    /// `warm_query_path` moves that cost into `open`.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn the_first_query_after_open_is_within_c5as_deadline() {
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
            .ingest_external_document(
                &crate::spool::SpoolDocument {
                    doc_id: "warm".into(),
                    url: String::new(),
                    text: "the quarterly revenue report".into(),
                    content_type: String::new(),
                },
                1,
            )
            .await
            .unwrap();

        let t = std::time::Instant::now();
        engine.query_hybrid("revenue numbers", 5).await.unwrap();
        let first = t.elapsed();
        let t = std::time::Instant::now();
        engine.query_hybrid("revenue numbers", 5).await.unwrap();
        let second = t.elapsed();
        println!("first query {first:?}, second query {second:?}");
        // What the warm-up actually buys, and the only thing a debug
        // build on a shared machine can state honestly: the first query
        // is no longer an outlier against the ones after it. Before
        // `warm_query_path` it was 71 ms against 43 ms. The absolute
        // budget (FR-STO-05) is `tests/pf_benchmark.rs`'s subject, which
        // follows the same "print the number, bound only the absurd"
        // convention.
        assert!(
            first < second * 2 + std::time::Duration::from_millis(20),
            "the first query after open ({first:?}) is still a cold outlier \
             against the second ({second:?})"
        );
    }

    /// M16: `reindex_family` loaded a *second* `Embedder` -- a whole
    /// second resident copy of bge-small-en-v1.5 -- doubling C3's model
    /// memory against a 300 MiB hard cap (PRD §6.2) for as long as the
    /// re-index ran. It now re-embeds through the engine's own embedder.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn a_reindex_does_not_load_a_second_model() {
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
        // More chunks than one re-index batch, so the batching loop's
        // boundary is exercised too.
        for i in 0..(REINDEX_BATCH_SIZE * 2 + 1) {
            engine
                .ingest_external_document(
                    &crate::spool::SpoolDocument {
                        doc_id: format!("doc-{i}"),
                        url: String::new(),
                        text: format!("document number {i} about quarterly revenue"),
                        content_type: String::new(),
                    },
                    1_000 + i as u64,
                )
                .await
                .unwrap();
        }

        let loads_before = Embedder::load_count();
        reindex_family(
            &engine.lance,
            &engine.sqlite_path,
            &engine.embedder,
            "external",
        )
        .await
        .unwrap();
        assert_eq!(
            Embedder::load_count(),
            loads_before,
            "a re-index must reuse the engine's embedder, not load another model"
        );
        assert_eq!(
            engine.lance.all_chunks("external").await.unwrap().len(),
            REINDEX_BATCH_SIZE * 2 + 1,
            "every chunk must still be there, re-embedded in place"
        );
    }

    /// M15: the vector insert and the SQLite metadata write were three
    /// independent steps, so a metadata failure left a vector in LanceDB
    /// that no keyword row and no `index_meta` entry knew about -- it
    /// would still come back from a dense query. Live, because a real
    /// vector needs the real embedder.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn a_failed_metadata_write_leaves_no_orphan_vector() {
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

        // The one metadata failure that can be provoked without a disk
        // fault: the table the write needs is gone.
        engine.conn.execute_batch("DROP TABLE index_meta").unwrap();
        let err = engine
            .ingest_external_document(
                &crate::spool::SpoolDocument {
                    doc_id: "doc-1".into(),
                    url: String::new(),
                    text: "a fetched document".into(),
                    content_type: String::new(),
                },
                1,
            )
            .await
            .expect_err("the metadata write must fail");
        assert!(matches!(err, EngineError::Sqlite(_)), "unexpected: {err}");

        let orphans = engine.lance.all_chunks("external").await.unwrap();
        assert!(
            orphans.is_empty(),
            "a failed metadata write must not leave a searchable vector: {orphans:?}"
        );
    }

    fn hit(id: &str, text: &str) -> crate::sqlite::FtsHit {
        crate::sqlite::FtsHit {
            chunk_id: id.into(),
            entity_id: 1,
            domain: "window_focus".into(),
            taint: 0,
            t_ns: 0,
            text: text.into(),
            score: 1.0,
        }
    }

    #[test]
    fn keyword_score_counts_distinct_question_terms_matched() {
        let fused = fuse_rrf(
            Vec::new(),
            vec![
                hit("a", "VaughnValle/lush-pop: A clean and green Linux setup"),
                hit("b", "-c: Test"),
            ],
            "Who made the lush-pop repository? python unit tests",
            5,
        );
        let score = |id: &str| {
            fused
                .iter()
                .find(|m| m.chunk_id == id)
                .unwrap()
                .keyword_score
        };
        assert!((score("a") - 2.0).abs() < f32::EPSILON, "lush + pop");
        assert!((score("b") - 1.0).abs() < f32::EPSILON, "tests ~ test");
    }

    fn dense(id: &str, text: &str, taint: u32) -> crate::lance::ChunkMatch {
        crate::lance::ChunkMatch {
            chunk_id: id.into(),
            entity_id: 1,
            text: text.into(),
            taint,
            t_ns: 0,
            domain: "window_focus".into(),
            distance: 0.1,
            keyword_score: 0.0,
        }
    }

    fn tainted_hit(id: &str, text: &str, taint: u32) -> crate::sqlite::FtsHit {
        let mut h = hit(id, text);
        h.taint = taint;
        h
    }

    const EXTERNAL: u32 = 1; // TaintFlags::EXTERNAL_UNTRUSTED.bits()
    const MODEL: u32 = 4; // TaintFlags::MODEL_GENERATED.bits()

    /// M9: when a dense hit and a keyword hit carried the same text, the
    /// merge kept the *dense* chunk's taint and discarded the keyword
    /// hit's. If the keyword hit was the EXTERNAL_UNTRUSTED one, that flag
    /// vanished -- taint lowered, which R0-3 forbids outright.
    #[test]
    fn merging_a_dense_and_keyword_hit_unions_their_taint() {
        let fused = fuse_rrf(
            vec![dense("a", "same text", 0)],
            vec![tainted_hit("a-kw", "same text", EXTERNAL)],
            "same text",
            5,
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].taint, EXTERNAL,
            "the keyword hit's taint must survive the merge"
        );
    }

    /// The same leak in the other direction: a duplicate *dense* chunk was
    /// skipped outright, taking its taint with it.
    #[test]
    fn dropping_a_duplicate_dense_chunk_keeps_its_taint() {
        let fused = fuse_rrf(
            vec![
                dense("a", "same text", 0),
                dense("b", "same text", EXTERNAL),
            ],
            Vec::new(),
            "same text",
            5,
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].taint, EXTERNAL,
            "the dropped duplicate's taint must be unioned in"
        );
    }

    #[test]
    fn a_duplicate_keyword_hit_also_contributes_its_taint() {
        let fused = fuse_rrf(
            Vec::new(),
            vec![
                tainted_hit("a", "same text", 0),
                tainted_hit("b", "same text", EXTERNAL),
            ],
            "same text",
            5,
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].taint, EXTERNAL);
    }

    #[test]
    fn taint_flags_from_several_sources_all_accumulate() {
        let fused = fuse_rrf(
            vec![dense("a", "same text", MODEL)],
            vec![tainted_hit("a-kw", "same text", EXTERNAL)],
            "same text",
            5,
        );
        assert_eq!(fused[0].taint, EXTERNAL | MODEL);
    }

    #[test]
    fn fusion_drops_repeated_texts_and_ranks_agreement_first() {
        let fused = fuse_rrf(
            Vec::new(),
            vec![hit("a", "same"), hit("b", "same"), hit("c", "other")],
            "same other",
            5,
        );
        assert_eq!(fused.len(), 2);
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

        ingest_one_focus_session(&mut engine).await;
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

    /// H2 live proof: the window entity is promoted (created) at t=6s by a
    /// first session, then a later "quarterly revenue dashboard" segment
    /// ends at t=9s. Forgetting since t=8.5s must remove that chunk from
    /// LanceDB and FTS while the entity and its earlier chunk survive.
    #[tokio::test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    async fn forget_since_removes_a_recent_chunk_of_an_older_entity() {
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
        let active = || {
            Kind::StateChanged(WindowStateChanged {
                states: vec![ToplevelState::Activated as i32],
            })
        };
        let inactive = || Kind::StateChanged(WindowStateChanged { states: vec![] });
        let s = 1_000_000_000;
        for (at, kind) in [
            (
                0,
                Kind::Opened(WindowOpened {
                    app_id: "org.mozilla.firefox".into(),
                    title: "kernel mailing list archive".into(),
                    pid: 0,
                    pid_known: false,
                }),
            ),
            (0, active()),
            (6 * s, inactive()),
            (7 * s, active()),
            (
                8 * s,
                Kind::TitleChanged(neuroos_proto::v1::WindowTitleChanged {
                    title: "quarterly revenue dashboard".into(),
                }),
            ),
            (9 * s, inactive()),
        ] {
            engine.ingest(&window_event(1, at, kind)).await.unwrap();
        }
        let mentions_revenue = |m: &Vec<crate::lance::ChunkMatch>| {
            m.iter().any(|c| c.text.contains("quarterly revenue"))
        };
        assert!(mentions_revenue(
            &engine.query_hybrid("quarterly revenue", 5).await.unwrap()
        ));

        let forgotten = engine.forget_since(8 * s + s / 2).await.unwrap();

        assert_eq!(forgotten, 0, "the entity itself predates the cutoff");
        let after = engine.query_hybrid("quarterly revenue", 5).await.unwrap();
        assert!(
            !mentions_revenue(&after),
            "the chunk recorded after the cutoff must be gone from LanceDB and FTS: {after:?}"
        );
        assert!(
            after.iter().any(|c| c.text.contains("kernel mailing list")),
            "chunks from before the cutoff are kept: {after:?}"
        );
        assert!(!dir_contains_bytes(
            lance_dir.path(),
            b"quarterly revenue dashboard"
        ));
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

        ingest_one_focus_session(&mut engine).await;

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
        // H3: nor its text in the copied LanceDB files (old dataset
        // versions included).
        assert!(
            !dir_contains_bytes(&backup_dest.join("lancedb"), b"quarterly revenue dashboard"),
            "the forgotten chunk's text must not survive in the backup's LanceDB copy"
        );
    }

    fn dir_contains_bytes(dir: &std::path::Path, needle: &[u8]) -> bool {
        std::fs::read_dir(dir).unwrap().any(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dir_contains_bytes(&path, needle)
            } else {
                std::fs::read(&path)
                    .unwrap()
                    .windows(needle.len())
                    .any(|w| w == needle)
            }
        })
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

        ingest_one_focus_session(&mut engine).await;

        let meta = crate::sqlite::get_index_meta(&engine.conn, "attention")
            .unwrap()
            .expect("ingest should have recorded index_meta");
        assert_eq!(meta.embedding_model_id, Embedder::model_id());

        reindex_family(
            &engine.lance,
            &engine.sqlite_path,
            &engine.embedder,
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
