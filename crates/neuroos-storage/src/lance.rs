//! LanceDB: vectors + chunks, one table per domain family (Architecture.md
//! §7.2: `attention`, `work`, `knowledge`, `system`, `external`), columns
//! `chunk_id, entity_id, text, vector[384], taint, t_ns, domain`.
use neuroos_common::sync::lock;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
    UInt32Array, UInt64Array,
};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::index::Index;
use lancedb::index::vector::IvfHnswFlatIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::{CompactionOptions, OptimizeAction};
use lancedb::{Connection, Table};
use neuroos_health::{Histogram, p99_ns};

/// FR-STO-07: a family promotes to an HNSW index once its measured query
/// p99 exceeds this — no item-count threshold, per spec.
const HNSW_PROMOTION_P99_THRESHOLD: Duration = Duration::from_millis(5);

/// Guards the p99 check against a single cold/slow query firing promotion
/// off a near-empty histogram; not itself an item-count promotion
/// criterion (FR-STO-07 has none) — just a minimum sample size for the
/// p99 estimate to mean anything.
const MIN_SAMPLES_BEFORE_PROMOTION_CHECK: u64 = 20;

/// BUG-006: one query slower than this promotes on its own, without
/// waiting for the sample floor above.
///
/// The floor exists so a *borderline* p99 (just over the 5 ms threshold)
/// isn't decided by one outlier. It was never meant to delay an obvious
/// case, but that is what it did: flat scan over a real ~6,100-event
/// corpus measures ~150 ms, 30x the threshold, so every query until the
/// floor was reached missed C5's own 100 ms query deadline (rules.md
/// §5.7) and came back as a degraded "couldn't retrieve evidence" answer.
/// Measured on this machine's own recording: 21 of 59 real questions
/// failed, contiguously from the first one, and the curve was identical
/// after letting the store settle for 45 s — so it was the floor, not a
/// warm-up or a compaction backlog.
///
/// 100 ms is C3's own copy of that deadline (C3 must not depend on C5a,
/// AB-1). A sample past it is not noise relative to a 5 ms threshold, and
/// the asymmetry is stark: promoting one family early costs a background
/// index build that only ever makes queries faster, while promoting late
/// costs the user ~20 unanswered questions. FR-STO-07 is still honoured —
/// promotion is decided by measured p99 exceeding 5 ms, never by item
/// count; this only changes how long C3 waits before believing its own
/// measurement.
const PROMOTE_IMMEDIATELY_ABOVE: Duration = Duration::from_millis(100);

/// BUG-007(c): a family is compacted in the background after this many
/// `insert` calls. Telemetry ingest adds one row (one fragment) per call;
/// at a measured ~6,100 events/3 h, waiting for the 6-hourly backup left
/// thousands of fragments and 140-250 ms flat scans. Compacting every 256
/// inserts keeps queries at ~20-40 ms for ~0.3 s of off-path work.
const COMPACT_EVERY_INSERTS: u64 = 256;

/// `bge-small-en-v1.5`'s output width (matches `embed::EMBEDDING_DIM`; kept
/// separate so this module has no compile-time dependency on `embed`).
pub const EMBEDDING_DIM: i32 = 384;

/// Architecture.md §7.2's 5 domain families.
pub const FAMILIES: &[&str] = &["attention", "work", "knowledge", "system", "external"];

const DISTANCE_COLUMN: &str = "_distance";

#[derive(Debug, thiserror::Error)]
pub enum LanceError {
    #[error("lancedb error: {0}")]
    Lance(#[from] lancedb::Error),
    #[error("arrow error: {0}")]
    Arrow(#[from] ArrowError),
    #[error("unknown domain family {0:?} (expected one of {FAMILIES:?})")]
    UnknownFamily(String),
    #[error("storage path is not valid UTF-8: {0:?}")]
    InvalidPath(std::path::PathBuf),
    #[error("malformed record batch: missing or mistyped column {0:?}")]
    MalformedBatch(&'static str),
    /// M15: the `vector` column is a `FixedSizeList<Float32, EMBEDDING_DIM>`;
    /// anything else (notably an empty vector from a failed embed) panics
    /// inside Arrow's builder rather than erroring, so it is rejected here.
    #[error("chunk {chunk_id:?} has a {got}-wide embedding, expected {expected}")]
    WrongEmbeddingWidth {
        chunk_id: String,
        got: usize,
        expected: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRecord {
    pub chunk_id: String,
    pub entity_id: i64,
    pub text: String,
    pub vector: Vec<f32>,
    pub taint: u32,
    pub t_ns: u64,
    pub domain: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkMatch {
    pub chunk_id: String,
    pub entity_id: i64,
    pub text: String,
    pub taint: u32,
    pub t_ns: u64,
    pub domain: String,
    pub distance: f32,
    /// See `ChunkMatch.keyword_score` in storage.proto; 0 for vector hits.
    pub keyword_score: f32,
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("entity_id", DataType::Int64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBEDDING_DIM,
            ),
            false,
        ),
        Field::new("taint", DataType::UInt32, false),
        Field::new("t_ns", DataType::UInt64, false),
        Field::new("domain", DataType::Utf8, false),
    ]))
}

fn empty_reader() -> RecordBatchIterator<std::vec::IntoIter<Result<RecordBatch, ArrowError>>> {
    RecordBatchIterator::new(Vec::new().into_iter(), schema())
}

fn records_to_batch(chunks: &[ChunkRecord]) -> Result<RecordBatch, LanceError> {
    // M15: Arrow's FixedSizeList builder *asserts* on a child array whose
    // length isn't `rows * EMBEDDING_DIM`, so a single mis-sized embedding
    // (an empty vector from a failed embed, a stale-dimension record from a
    // reindex) would panic the task instead of failing the insert.
    for c in chunks {
        if c.vector.len() != EMBEDDING_DIM as usize {
            return Err(LanceError::WrongEmbeddingWidth {
                chunk_id: c.chunk_id.clone(),
                got: c.vector.len(),
                expected: EMBEDDING_DIM as usize,
            });
        }
    }
    let vector = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        chunks
            .iter()
            .map(|c| Some(c.vector.iter().copied().map(Some).collect::<Vec<_>>())),
        EMBEDDING_DIM,
    );
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.chunk_id.as_str()),
            )),
            Arc::new(Int64Array::from_iter_values(
                chunks.iter().map(|c| c.entity_id),
            )),
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.text.as_str()),
            )),
            Arc::new(vector),
            Arc::new(UInt32Array::from_iter_values(
                chunks.iter().map(|c| c.taint),
            )),
            Arc::new(UInt64Array::from_iter_values(chunks.iter().map(|c| c.t_ns))),
            Arc::new(StringArray::from_iter_values(
                chunks.iter().map(|c| c.domain.as_str()),
            )),
        ],
    )?;
    Ok(batch)
}

fn column<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a T, LanceError> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<T>())
        .ok_or(LanceError::MalformedBatch(name))
}

fn decode_batch(batch: &RecordBatch, has_distance: bool) -> Result<Vec<ChunkMatch>, LanceError> {
    let chunk_id = column::<StringArray>(batch, "chunk_id")?;
    let entity_id = column::<Int64Array>(batch, "entity_id")?;
    let text = column::<StringArray>(batch, "text")?;
    let taint = column::<UInt32Array>(batch, "taint")?;
    let t_ns = column::<UInt64Array>(batch, "t_ns")?;
    let domain = column::<StringArray>(batch, "domain")?;
    let distance = has_distance
        .then(|| column::<arrow_array::Float32Array>(batch, DISTANCE_COLUMN))
        .transpose()?;

    Ok((0..batch.num_rows())
        .map(|i| ChunkMatch {
            chunk_id: chunk_id.value(i).to_string(),
            entity_id: entity_id.value(i),
            text: text.value(i).to_string(),
            taint: taint.value(i),
            t_ns: t_ns.value(i),
            domain: domain.value(i).to_string(),
            distance: distance.map(|d| d.value(i)).unwrap_or(0.0),
            keyword_score: 0.0,
        })
        .collect())
}

pub struct LanceStore {
    /// BUG-002: opened once in `open()` and reused for every call. LanceDB's
    /// own `Table` is meant to be long-lived (its `DatasetConsistencyWrapper`
    /// tracks the dataset's version internally, and its own test suite --
    /// `table/dataset.rs`'s `test_iops_open_strong_consistency`/
    /// `test_reload_resets_consistency_timer` -- measures real read IOPS to
    /// prove repeated calls on the *same* handle cost ~0 extra I/O). Calling
    /// `conn.table_names()` (a directory listing) + `conn.open_table()` (a
    /// manifest read) on every `query`/`insert`/etc. call instead, as this
    /// used to, re-pays that I/O on every single request -- measured as the
    /// dominant cost of the 4-8x-over-budget flat-scan latency (phases.md
    /// §7.3 PF). Safe to hold across writes: every write here goes through
    /// this same connection's own `Table` handles (no other process/writer
    /// touches this LanceDB directory), and `Table::add`/`delete`/
    /// `merge_insert` all call the wrapper's own `update()` on success, so a
    /// query issued right after a write on the *same* handle always sees
    /// that write -- confirmed directly in `table/dataset.rs`'s
    /// `test_update_stores_newer_version`.
    tables: HashMap<&'static str, Table>,
    /// FR-STO-07: per-family query-latency histogram driving HNSW
    /// promotion (not persisted — resets on restart, which just means the
    /// promotion decision re-measures fresh instead of carrying over a
    /// stale one; already-created indexes stay on disk regardless).
    query_latencies: Mutex<HashMap<String, Histogram>>,
    /// Families already promoted to HNSW this session, so a slow query
    /// doesn't retry `create_index` every time. `Arc`-wrapped (not just
    /// `Mutex`) so BUG-003's fix can clone a handle into the detached
    /// `tokio::spawn`ed task that actually builds the index, without
    /// needing `self: Arc<Self>` on every `LanceStore` method.
    promoted: Arc<Mutex<HashSet<String>>>,
    /// BUG-007(c): `insert` calls per family since its last compaction.
    inserts_since_compact: Mutex<HashMap<String, u64>>,
    /// Families with a background compaction in flight, so a burst of
    /// inserts doesn't stack up concurrent compactions of the same table.
    compacting: Arc<Mutex<HashSet<String>>>,
    /// H3: per-family lock held by every background rewrite of the table
    /// (compaction, HNSW build) and by [`Self::purge_deleted`], whose
    /// unverified-file cleanup would otherwise delete files an in-flight
    /// rewrite is still writing.
    maintenance: HashMap<&'static str, Arc<tokio::sync::Mutex<()>>>,
}

impl LanceStore {
    /// Opens (creating if needed) the LanceDB database at `path` and opens
    /// every domain-family table exactly once, caching the handles in
    /// `tables` (BUG-002) — every other method looks them up there instead
    /// of re-opening.
    pub async fn open(path: &Path) -> Result<Self, LanceError> {
        let uri = path
            .to_str()
            .ok_or_else(|| LanceError::InvalidPath(path.to_path_buf()))?;
        let conn = lancedb::connect(uri).execute().await?;
        let mut tables = HashMap::with_capacity(FAMILIES.len());
        for family in FAMILIES {
            tables.insert(*family, Self::open_or_create_table(&conn, family).await?);
        }
        Ok(Self {
            tables,
            query_latencies: Mutex::new(HashMap::new()),
            promoted: Arc::new(Mutex::new(HashSet::new())),
            inserts_since_compact: Mutex::new(HashMap::new()),
            compacting: Arc::new(Mutex::new(HashSet::new())),
            maintenance: FAMILIES
                .iter()
                .map(|f| (*f, Arc::new(tokio::sync::Mutex::new(()))))
                .collect(),
        })
    }

    fn maintenance_lock(&self, family: &str) -> Result<Arc<tokio::sync::Mutex<()>>, LanceError> {
        self.maintenance
            .get(family)
            .cloned()
            .ok_or_else(|| LanceError::UnknownFamily(family.to_string()))
    }

    fn check_family(family: &str) -> Result<(), LanceError> {
        if FAMILIES.contains(&family) {
            Ok(())
        } else {
            Err(LanceError::UnknownFamily(family.to_string()))
        }
    }

    /// One-time (per family, called only from `open()`) open-or-create; the
    /// real disk I/O this used to pay on every hot-path call (BUG-002).
    async fn open_or_create_table(conn: &Connection, family: &str) -> Result<Table, LanceError> {
        let names = conn.table_names().execute().await?;
        if names.iter().any(|n| n == family) {
            Ok(conn.open_table(family).execute().await?)
        } else {
            let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(empty_reader());
            Ok(conn.create_table(family, reader).execute().await?)
        }
    }

    /// O(1) lookup of `family`'s already-open table — no I/O (BUG-002).
    /// Every `FAMILIES` entry is opened eagerly in `open()`, so a missing
    /// entry here can only mean an invalid family name.
    fn table(&self, family: &str) -> Result<&Table, LanceError> {
        Self::check_family(family)?;
        self.tables
            .get(family)
            .ok_or_else(|| LanceError::UnknownFamily(family.to_string()))
    }

    /// FR-STO-04: batched insert (the ingest path's batching requirement —
    /// call with every chunk from one ingest cycle, not one row at a time).
    pub async fn insert(&self, family: &str, chunks: &[ChunkRecord]) -> Result<(), LanceError> {
        if chunks.is_empty() {
            return Ok(());
        }
        let table = self.table(family)?;
        let batch = records_to_batch(chunks)?;
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
            RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema()),
        );
        table.add(reader).execute().await?;
        self.maybe_spawn_compaction(family, table)?;
        Ok(())
    }

    /// BUG-007(c): every `COMPACT_EVERY_INSERTS` inserts, merges `family`'s
    /// small fragments in a detached task, off both the ingest and query
    /// paths (same reasoning as BUG-003's HNSW promotion). A failure (e.g.
    /// a commit conflict with a concurrent write) just leaves the counter
    /// reset; the next threshold crossing tries again.
    fn maybe_spawn_compaction(&self, family: &str, table: &Table) -> Result<(), LanceError> {
        {
            let mut counts = lock(&self.inserts_since_compact);
            let count = counts.entry(family.to_string()).or_insert(0);
            *count += 1;
            if *count < COMPACT_EVERY_INSERTS || !lock(&self.compacting).insert(family.to_string())
            {
                return Ok(());
            }
            *count = 0;
        }
        let maintenance = self.maintenance_lock(family)?;
        let family = family.to_string();
        let table = table.clone();
        let compacting = Arc::clone(&self.compacting);
        tokio::spawn(async move {
            let _maintenance = maintenance.lock().await;
            let result = table
                .optimize(OptimizeAction::Compact {
                    options: CompactionOptions::default(),
                    remap_options: None,
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(family, error = %err, "BUG-007: background compaction failed, will retry");
            }
            lock(&compacting).remove(&family);
        });
        Ok(())
    }

    /// FR-STO-05: exact (flat) nearest-neighbor search within one family,
    /// unless FR-STO-07's HNSW promotion has already fired for it (P4-S05)
    /// — LanceDB transparently uses whatever index exists on the `vector`
    /// column, so this method's own query stays the same either way.
    pub async fn query(
        &self,
        family: &str,
        vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ChunkMatch>, LanceError> {
        let table = self.table(family)?;
        let start = Instant::now();
        let batches = table
            .query()
            .nearest_to(vector)?
            .limit(top_k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        let elapsed = start.elapsed();
        let results = batches
            .iter()
            .map(|b| decode_batch(b, true))
            .collect::<Result<Vec<_>, _>>()
            .map(|nested| nested.into_iter().flatten().collect());
        if self.record_query_latency(family, elapsed) {
            // BUG-003: `create_index` was previously `.await`ed right here,
            // inside the query call that crossed the promotion threshold —
            // that one query paid the *entire* index-build cost as its own
            // latency (measured: 118 real seconds on a 20k-item corpus).
            // Spawned as its own detached task instead: LanceDB serves flat
            // scans for `family` until the index actually exists (no
            // "index in progress" state to check — `create_index` just
            // hasn't returned yet), so every other query keeps working at
            // flat-scan latency while this builds in the background.
            Self::spawn_hnsw_promotion(
                family.to_string(),
                table.clone(),
                self.promoted.clone(),
                self.maintenance_lock(family)?,
            );
        }
        results
    }

    /// Records one query's latency for `family` and reports whether it
    /// should now be promoted to HNSW (FR-STO-07: p99 > 5.0 ms, no
    /// item-count threshold). Split out from `query()` so the promotion
    /// policy itself is unit-testable with synthetic durations instead of
    /// needing a real query slow enough to cross 5ms.
    fn record_query_latency(&self, family: &str, elapsed: Duration) -> bool {
        if lock(&self.promoted).contains(family) {
            return false;
        }
        let hist = {
            let mut latencies = lock(&self.query_latencies);
            let h = latencies.entry(family.to_string()).or_default();
            h.record(elapsed);
            h.to_proto()
        };
        // BUG-006: the sample floor applies only to borderline cases. A
        // single query past `PROMOTE_IMMEDIATELY_ABOVE` is already decisive,
        // and waiting out the floor is what made the first ~20 queries
        // against a fresh store fail. The p99 check below still decides.
        if elapsed <= PROMOTE_IMMEDIATELY_ABOVE && hist.count < MIN_SAMPLES_BEFORE_PROMOTION_CHECK {
            return false;
        }
        let Some(p99) = p99_ns(&hist) else {
            return false;
        };
        if (p99 as u128) <= HNSW_PROMOTION_P99_THRESHOLD.as_nanos() {
            return false;
        }
        lock(&self.promoted).insert(family.to_string())
    }

    /// BUG-003: spawns `create_index` (`IVF_HNSW_FLAT` on `table`'s `vector`
    /// column) as its own detached background task instead of running it
    /// inline in the triggering query. Takes owned/`Arc`-cloned arguments
    /// (not `&self`) because a `tokio::spawn`ed future must be `'static`.
    /// Best-effort: a training failure (e.g. too little data yet) logs and
    /// un-marks `family` in `promoted` so the next qualifying query's
    /// `record_query_latency` retries, instead of leaving it permanently
    /// stuck un-promoted.
    fn spawn_hnsw_promotion(
        family: String,
        table: Table,
        promoted: Arc<Mutex<HashSet<String>>>,
        maintenance: Arc<tokio::sync::Mutex<()>>,
    ) {
        tokio::spawn(async move {
            let _maintenance = maintenance.lock().await;
            let result = table
                .create_index(
                    &["vector"],
                    Index::IvfHnswFlat(IvfHnswFlatIndexBuilder::default()),
                )
                .execute()
                .await;
            if let Err(err) = result {
                tracing::warn!(family, error = %err, "FR-STO-07 HNSW promotion failed, will retry on a future slow query");
                lock(&promoted).remove(&family);
            } else {
                tracing::info!(
                    family,
                    "FR-STO-07: promoted family to HNSW index (p99 > 5ms)"
                );
            }
        });
    }

    /// Test/introspection hook: number of data fragments in `family`'s table.
    pub async fn fragment_count(&self, family: &str) -> Result<usize, LanceError> {
        Ok(self
            .table(family)?
            .stats()
            .await?
            .fragment_stats
            .num_fragments)
    }

    /// Test/introspection hook: whether `family` has been promoted to HNSW
    /// this session.
    pub fn is_promoted(&self, family: &str) -> bool {
        lock(&self.promoted).contains(family)
    }

    /// FR-STO-05's `QueryHybridVectorText`: search every family (a question
    /// isn't scoped to one domain) and merge by distance.
    ///
    /// BUG-002: each family is an independent LanceDB table (its own files,
    /// its own `Table` handle in `self.tables`), so there's no correctness
    /// reason to query them one at a time -- structurally correct to run
    /// concurrently. Note: this alone did not measurably reduce real
    /// end-to-end latency in this session's own testing (see BUGS.md's
    /// BUG-002 write-up); `storage.sock`'s connection handling is pinned to
    /// one `LocalSet` thread (`server.rs`), so the real win here depends on
    /// whether LanceDB's query execution yields control back to the
    /// executor mid-poll, which wasn't confirmed. Kept as a correct,
    /// zero-risk improvement, not a proven fix on its own.
    pub async fn query_all_families(
        &self,
        vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ChunkMatch>, LanceError> {
        let per_family = futures::future::try_join_all(
            FAMILIES
                .iter()
                .map(|family| self.query(family, vector, top_k)),
        )
        .await?;
        let mut all: Vec<ChunkMatch> = per_family.into_iter().flatten().collect();
        all.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        all.truncate(top_k);
        Ok(all)
    }

    /// FR-STO-11: every row currently in `family` (full scan, no vector
    /// search) — the background re-index's source of each chunk's
    /// original `text` to re-embed. The returned `distance` field is
    /// meaningless here (no query vector); callers doing a re-index never
    /// look at it.
    pub async fn all_chunks(&self, family: &str) -> Result<Vec<ChunkMatch>, LanceError> {
        let table = self.table(family)?;
        let batches = table
            .query()
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        batches
            .iter()
            .map(|b| decode_batch(b, false))
            .collect::<Result<Vec<_>, _>>()
            .map(|nested| nested.into_iter().flatten().collect())
    }

    /// FR-STO-11: writes new vectors (and unchanged metadata) back for
    /// existing rows in `family`, matched by `chunk_id` — an upsert via
    /// `merge_insert`, not delete+reinsert, so a query racing this sees
    /// either the pre- or post-reindex vector for a row, never a gap.
    pub async fn upsert_vectors(
        &self,
        family: &str,
        chunks: &[ChunkRecord],
    ) -> Result<(), LanceError> {
        if chunks.is_empty() {
            return Ok(());
        }
        let table = self.table(family)?;
        let batch = records_to_batch(chunks)?;
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
            RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema()),
        );
        let mut merge = table.merge_insert(&["chunk_id"]);
        merge.when_matched_update_all(None);
        merge.execute(reader).await?;
        Ok(())
    }

    /// FR-STO-12 ("forget"): deletes every row matching `predicate` (a
    /// LanceDB SQL-like filter, e.g. `"entity_id IN (1,2,3)"`) from one
    /// family.
    pub async fn delete(&self, family: &str, predicate: &str) -> Result<(), LanceError> {
        let table = self.table(family)?;
        table.delete(predicate).await?;
        Ok(())
    }

    /// Same, across every family — "forget" isn't scoped to one domain.
    pub async fn delete_all_families(&self, predicate: &str) -> Result<(), LanceError> {
        for family in FAMILIES {
            self.delete(family, predicate).await?;
        }
        Ok(())
    }

    /// H3 (FR-STO-12/FR-PRV-03): makes earlier [`Self::delete`]s physical.
    /// A LanceDB delete only records a deletion vector -- the row stays in
    /// its data file and in every older dataset version (which backups
    /// copy). Compacting with `materialize_deletions_threshold = 0`
    /// rewrites every fragment that has deletions, then pruning every old
    /// version removes the files that still held the rows. Waits for any
    /// in-flight background compaction or HNSW build on the family first
    /// (`maintenance`), since `delete_unverified` would otherwise remove
    /// files those are still writing. Callers hold the engine lock, so no
    /// foreground write races this either.
    pub async fn purge_deleted(&self) -> Result<(), LanceError> {
        for family in FAMILIES {
            let maintenance = self.maintenance_lock(family)?;
            let _maintenance = maintenance.lock().await;
            let table = self.table(family)?;
            table
                .optimize(OptimizeAction::Compact {
                    options: CompactionOptions {
                        materialize_deletions: true,
                        materialize_deletions_threshold: 0.0,
                        ..CompactionOptions::default()
                    },
                    remap_options: None,
                })
                .await?;
            table
                .optimize(OptimizeAction::Prune {
                    older_than: Some(lancedb::table::optimize::Duration::zero()),
                    delete_unverified: Some(true),
                    error_if_tagged_old_versions: Some(false),
                })
                .await?;
        }
        Ok(())
    }

    /// BUG-002: real telemetry ingest calls `insert` one chunk at a time
    /// (`store_chunk`'s own doc comment: "fine at telemetry ingest rates" --
    /// batching is only the spool/replay path's concern), and Lance's
    /// on-disk format is append-only, so every single-row insert becomes
    /// its own fragment *file*. A flat/KNN scan has to open every fragment
    /// in a table, so file *count* (not row count) drives its cost. Real
    /// measurement against this machine's own ~8h recording found a family
    /// with real-world-typical ingest volume had accumulated 192 fragment
    /// files (`attention.lance/data/`), and that alone accounted for
    /// essentially the entire ~170-190ms `query_all_families` was costing
    /// -- the other four (still-empty) families cost next to nothing.
    /// `lance`'s own docs describe exactly this ("small files can hurt read
    /// and write performance... if writes are run frequently, compaction
    /// should run frequently too") and `optimize(Compact)` is its answer:
    /// merge small fragments into fewer, larger ones.
    ///
    /// C8: this used to say compaction happens only here, on the 6-hourly
    /// backup cadence. That stopped being true with BUG-007(c), which found
    /// the backup interval far too slow for real ingest rates (~6,100
    /// events/3 h, one fragment per row) and added per-family background
    /// compaction every `COMPACT_EVERY_INSERTS` inserts. That is the primary
    /// mechanism now. This whole-store pass still runs from
    /// `StorageEngine::backup` as the backstop that catches families whose
    /// insert counters never reach the threshold (Architecture.md §7.5).
    /// Neither path runs on the hot ingest path: the per-insert one spawns
    /// off-task and holds the family's maintenance lock, so ingest latency is
    /// never traded for query latency (rules.md AB-11).
    pub async fn compact_all_families(&self) -> Result<(), LanceError> {
        for family in FAMILIES {
            let maintenance = self.maintenance_lock(family)?;
            let _maintenance = maintenance.lock().await;
            self.table(family)?
                .optimize(OptimizeAction::Compact {
                    options: CompactionOptions::default(),
                    remap_options: None,
                })
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn chunk(id: &str, vector: Vec<f32>) -> ChunkRecord {
        ChunkRecord {
            chunk_id: id.to_string(),
            entity_id: 1,
            text: format!("text for {id}"),
            vector,
            taint: 0,
            t_ns: 1,
            domain: "window_focus".into(),
        }
    }

    /// M15: an embedding that is not exactly `EMBEDDING_DIM` wide used to
    /// reach Arrow's `FixedSizeList` builder, which panics (`Invalid
    /// argument error`/assert) instead of returning an error -- taking the
    /// whole ingest task down. The width is a precondition of the schema,
    /// so it is checked here, where the batch is built.
    #[tokio::test]
    async fn inserting_an_empty_vector_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let err = store
            .insert("attention", &[chunk("empty", Vec::new())])
            .await
            .unwrap_err();
        assert!(
            matches!(err, LanceError::WrongEmbeddingWidth { ref chunk_id, got, expected }
                if chunk_id == "empty" && got == 0 && expected == EMBEDDING_DIM as usize),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn inserting_a_too_wide_vector_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let err = store
            .insert(
                "attention",
                &[chunk("wide", vec![1.0; EMBEDDING_DIM as usize + 1])],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LanceError::WrongEmbeddingWidth { .. }));
    }

    #[tokio::test]
    async fn insert_then_query_finds_the_nearest_vector() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[
                    chunk("near", vec![1.0; EMBEDDING_DIM as usize]),
                    chunk("far", vec![-1.0; EMBEDDING_DIM as usize]),
                ],
            )
            .await
            .unwrap();

        let results = store
            .query("attention", &[1.0; EMBEDDING_DIM as usize], 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, "near");
    }

    #[tokio::test]
    async fn query_all_families_merges_across_families_by_distance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[chunk("a", vec![1.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();
        store
            .insert(
                "knowledge",
                &[chunk("b", vec![0.99; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();

        let results = store
            .query_all_families(&[1.0; EMBEDDING_DIM as usize], 2)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, "a"); // exact match, closest
        assert!(results[0].distance <= results[1].distance);
    }

    #[tokio::test]
    async fn empty_family_returns_no_matches_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let results = store
            .query("work", &[0.0; EMBEDDING_DIM as usize], 5)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delete_removes_matching_rows_by_predicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let mut a = chunk("a", vec![1.0; EMBEDDING_DIM as usize]);
        a.entity_id = 1;
        let mut b = chunk("b", vec![1.0; EMBEDDING_DIM as usize]);
        b.entity_id = 2;
        store.insert("attention", &[a, b]).await.unwrap();

        store.delete("attention", "entity_id = 1").await.unwrap();
        let remaining = store
            .query("attention", &[1.0; EMBEDDING_DIM as usize], 10)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].chunk_id, "b");
    }

    #[tokio::test]
    async fn unknown_family_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let err = store
            .insert(
                "not_a_real_family",
                &[chunk("x", vec![0.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LanceError::UnknownFamily(_)));
    }

    #[tokio::test]
    async fn all_chunks_returns_every_row_in_the_family() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[
                    chunk("a", vec![1.0; EMBEDDING_DIM as usize]),
                    chunk("b", vec![-1.0; EMBEDDING_DIM as usize]),
                ],
            )
            .await
            .unwrap();

        let mut ids = store
            .all_chunks("attention")
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.chunk_id)
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn upsert_vectors_replaces_the_vector_for_an_existing_chunk_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[chunk("a", vec![1.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();

        let mut updated = chunk("a", vec![-1.0; EMBEDDING_DIM as usize]);
        updated.text = "re-embedded text".to_string();
        store.upsert_vectors("attention", &[updated]).await.unwrap();

        let results = store
            .query("attention", &[-1.0; EMBEDDING_DIM as usize], 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, "a");
        assert_eq!(results[0].text, "re-embedded text");

        // the row was updated in place, not duplicated
        assert_eq!(store.all_chunks("attention").await.unwrap().len(), 1);
    }

    // P4-S05 / FR-STO-07: promotion policy tested directly with synthetic
    // durations (not via real queries slow enough to cross 5ms — that's
    // covered by the PF benchmark, not a unit test).
    #[tokio::test]
    async fn fast_queries_never_promote() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        for _ in 0..MIN_SAMPLES_BEFORE_PROMOTION_CHECK * 2 {
            assert!(!store.record_query_latency("attention", Duration::from_micros(100)));
        }
        assert!(!store.is_promoted("attention"));
    }

    #[tokio::test]
    async fn too_few_samples_never_promotes_even_if_all_slow() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        for _ in 0..MIN_SAMPLES_BEFORE_PROMOTION_CHECK - 1 {
            assert!(!store.record_query_latency("attention", Duration::from_millis(50)));
        }
        assert!(!store.is_promoted("attention"));
    }

    // BUG-006: the sample floor above is what made the first ~20 real
    // queries against a fresh store miss C5's 100 ms deadline -- flat scan
    // on a real ~6,100-event corpus measures ~150 ms, so every query before
    // the floor was reached failed, and the 21st finally promoted. A single
    // sample that far over a 5 ms threshold is not noise, so it promotes on
    // its own.
    #[tokio::test]
    async fn one_sample_over_the_query_deadline_promotes_without_waiting_for_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        assert!(
            store.record_query_latency(
                "attention",
                PROMOTE_IMMEDIATELY_ABOVE + Duration::from_millis(1)
            ),
            "one query past the deadline should promote on the first sample"
        );
        assert!(store.is_promoted("attention"));
    }

    // The flip side, and why the floor still exists: a borderline sample
    // (over 5 ms but inside the deadline) must still wait for enough
    // samples to make a p99 meaningful, so one slow outlier can't promote
    // a family that is actually fine.
    #[tokio::test]
    async fn one_borderline_sample_still_waits_for_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        assert!(!store.record_query_latency("attention", Duration::from_millis(50)));
        assert!(!store.is_promoted("attention"));
    }

    #[tokio::test]
    async fn p99_above_threshold_reports_promotion_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let mut promotions = 0;
        for _ in 0..MIN_SAMPLES_BEFORE_PROMOTION_CHECK {
            if store.record_query_latency("attention", Duration::from_millis(50)) {
                promotions += 1;
            }
        }
        assert_eq!(promotions, 1, "should report the crossing exactly once");
        assert!(store.is_promoted("attention"));
        // Already promoted: further slow queries must not report again.
        assert!(!store.record_query_latency("attention", Duration::from_millis(50)));
    }

    // BUG-003: promotion must not block the triggering query on
    // `create_index` — `query()` should return as soon as its own search
    // completes, with the index build happening in a detached background
    // task. Can't reproduce the real 118s stall on tiny test data (index
    // build is near-instant there), but this proves the *shape* of the
    // fix: `query()`'s own return isn't gated on `is_promoted` flipping,
    // and it does eventually flip once the spawned task runs.
    #[tokio::test]
    async fn promotion_does_not_block_the_triggering_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        store
            .insert(
                "attention",
                &[chunk("a", vec![1.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();
        for _ in 0..MIN_SAMPLES_BEFORE_PROMOTION_CHECK - 1 {
            store.record_query_latency("attention", Duration::from_millis(50));
        }

        // This call's own `record_query_latency` crosses the threshold and
        // fires promotion — if it were still `.await`ed inline (the old
        // bug), this query call itself would pay `create_index`'s cost.
        let results = store
            .query("attention", &[1.0; EMBEDDING_DIM as usize], 1)
            .await
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "flat scan still serves results immediately"
        );

        // The spawned background task should complete shortly after,
        // without the query call above having waited for it.
        let mut promoted = store.is_promoted("attention");
        for _ in 0..50 {
            if promoted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            promoted = store.is_promoted("attention");
        }
        assert!(
            promoted,
            "background task should promote family to HNSW eventually"
        );
    }

    #[tokio::test]
    async fn promotion_is_scoped_per_family() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        for _ in 0..MIN_SAMPLES_BEFORE_PROMOTION_CHECK {
            store.record_query_latency("attention", Duration::from_millis(50));
        }
        assert!(store.is_promoted("attention"));
        assert!(!store.is_promoted("work"));
    }

    /// BUG-007(c): telemetry ingest inserts one row per call, so without
    /// compaction between 6-hourly backups a family accumulates one
    /// fragment per event and flat-scan queries blow C5's 100 ms deadline.
    #[tokio::test]
    async fn single_row_inserts_trigger_background_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        for i in 0..COMPACT_EVERY_INSERTS {
            let mut v = vec![0.0; EMBEDDING_DIM as usize];
            v[i as usize % EMBEDDING_DIM as usize] = 1.0;
            store
                .insert("attention", &[chunk(&format!("c{i}"), v)])
                .await
                .unwrap();
        }
        let mut fragments = store.fragment_count("attention").await.unwrap();
        for _ in 0..100 {
            if fragments < COMPACT_EVERY_INSERTS as usize {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            fragments = store.fragment_count("attention").await.unwrap();
        }
        assert!(
            fragments < COMPACT_EVERY_INSERTS as usize,
            "expected background compaction, still {fragments} fragments"
        );
    }

    /// Whether any file under `dir` contains `needle` byte-for-byte.
    fn dir_contains_bytes(dir: &Path, needle: &[u8]) -> bool {
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

    /// H3: a LanceDB delete only writes a deletion vector; the row stays in
    /// the data file and in every older dataset version until compaction
    /// rewrites it and the old versions are pruned. Forget must leave
    /// nothing recoverable on disk (or in the next backup's copy of it).
    #[tokio::test]
    async fn purge_removes_deleted_rows_and_old_versions_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceStore::open(dir.path()).await.unwrap();
        let secret = "forget-me-7f3a9c2e-unique-window-title";
        let mut forgotten = chunk("forgotten", vec![1.0; EMBEDDING_DIM as usize]);
        forgotten.entity_id = 42;
        forgotten.text = secret.to_string();
        store.insert("attention", &[forgotten]).await.unwrap();
        store
            .insert(
                "attention",
                &[chunk("kept", vec![-1.0; EMBEDDING_DIM as usize])],
            )
            .await
            .unwrap();

        store
            .delete_all_families("entity_id IN (42)")
            .await
            .unwrap();
        assert!(
            dir_contains_bytes(dir.path(), secret.as_bytes()),
            "precondition: a bare delete leaves the text on disk"
        );

        store.purge_deleted().await.unwrap();

        assert!(
            !dir_contains_bytes(dir.path(), secret.as_bytes()),
            "forgotten text must be gone from every file after purge"
        );
        let table = store.table("attention").unwrap();
        assert_eq!(table.list_versions().await.unwrap().len(), 1);
        let remaining = store.all_chunks("attention").await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].chunk_id, "kept");
    }
}
