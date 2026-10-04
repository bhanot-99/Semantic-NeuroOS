//! `meta.sqlite3` (Architecture.md §7.2): WAL mode, `synchronous=NORMAL`,
//! single writer (this process). Owns `focus_history`, `event_counters`,
//! `aggregates_daily`, `entities`, `edges`, `chunks_meta`, `index_meta`.
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

use neuroos_taint::TaintFlags;

/// Every `.sql` file under `src/migrations/`, embedded at compile time
/// (`include_str!`, not a runtime file read — this binary must work from
/// wherever it's installed, not just a checkout).
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_initial", include_str!("migrations/0001_initial.sql")),
    (
        "0002_chunks_fts",
        include_str!("migrations/0002_chunks_fts.sql"),
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Opens (creating if needed) `meta.sqlite3` at `path`, sets the required
/// pragmas, and applies any migration not yet recorded in
/// `schema_migrations`.
pub fn open(path: &Path) -> Result<Connection, StorageError> {
    let conn = Connection::open(path)?;
    enable_incremental_auto_vacuum(&conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // H15: temp tables/indices (and VACUUM's scratch space) in memory, not
    // in /tmp, which C3's Landlock sandbox doesn't grant.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Same as [`open`] but in-memory, for tests that don't need a real file.
pub fn open_in_memory() -> Result<Connection, StorageError> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Architecture.md §7.5's GC step runs `PRAGMA incremental_vacuum`, which
/// is a no-op unless the file is in `auto_vacuum = INCREMENTAL` mode. The
/// mode only takes effect on an existing file after a full `VACUUM`, so a
/// database created before this was set is converted once, here.
fn enable_incremental_auto_vacuum(conn: &Connection) -> Result<(), StorageError> {
    const INCREMENTAL: i64 = 2;
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
    if mode != INCREMENTAL {
        conn.pragma_update(None, "auto_vacuum", INCREMENTAL)?;
        conn.execute_batch("VACUUM;")?;
    }
    Ok(())
}

fn migrate(conn: &Connection) -> Result<(), StorageError> {
    apply_migrations(conn, MIGRATIONS)
}

fn apply_migrations(conn: &Connection, migrations: &[(&str, &str)]) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT PRIMARY KEY,
            applied_ns INTEGER NOT NULL
        );",
    )?;
    for (version, sql) in migrations {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )?;
        if already_applied {
            continue;
        }
        // M17: the migration's statements and the row that records it as
        // applied are one transaction. SQLite's DDL is transactional, so a
        // failure part-way (a crash, a disk error, a statement this build
        // rejects) rolls the whole migration back instead of leaving
        // half-created tables with nothing recording them -- which made
        // every later `open()` re-run the migration and die on "table ...
        // already exists", permanently.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let applied = (|| {
            conn.execute_batch(sql)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_ns) VALUES (?1, ?2)",
                (version, neuroos_common::now_ns()),
            )
        })();
        match applied {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                // A failing rollback would mask the real cause, so it is
                // logged rather than returned (rules.md R0-6: no payloads).
                if let Err(rollback) = conn.execute_batch("ROLLBACK") {
                    tracing::error!(error = %rollback, "failed to roll back a partial migration");
                }
                return Err(err.into());
            }
        }
    }
    Ok(())
}

/// Upserts `event_counters(domain, key)`: creates the row on first sight,
/// otherwise bumps `count`/`last_ns`/`total_dwell_ms`.
pub fn touch_event_counter(
    conn: &Connection,
    domain: &str,
    key: &str,
    at_ns: u64,
    dwell_ms: u64,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO event_counters (domain, key, count, first_ns, last_ns, total_dwell_ms)
         VALUES (?1, ?2, 1, ?3, ?3, ?4)
         ON CONFLICT(domain, key) DO UPDATE SET
             count = count + 1,
             last_ns = excluded.last_ns,
             total_dwell_ms = total_dwell_ms + excluded.total_dwell_ms",
        (domain, key, at_ns as i64, dwell_ms as i64),
    )?;
    Ok(())
}

pub fn entity_exists(conn: &Connection, domain: &str, label: &str) -> Result<bool, StorageError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM entities WHERE domain = ?1 AND label = ?2)",
        (domain, label),
        |r| r.get(0),
    )?)
}

/// Inserts a new entity or, if `(domain, label)` already exists, bumps
/// `last_seen_ns` and unions in `taint` (taint is never lowered — R0-3).
/// Returns the entity's `id`.
pub fn upsert_entity(
    conn: &Connection,
    domain: &str,
    kind: &str,
    label: &str,
    taint: TaintFlags,
    at_ns: u64,
    permanent: bool,
) -> Result<i64, StorageError> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM entities WHERE domain = ?1 AND label = ?2",
            (domain, label),
            |row| row.get(0),
        )
        .ok();
    if let Some(id) = existing {
        conn.execute(
            "UPDATE entities SET last_seen_ns = ?1, taint = taint | ?2 WHERE id = ?3",
            (at_ns as i64, taint.bits(), id),
        )?;
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO entities (domain, kind, label, taint, created_ns, last_seen_ns, permanent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        (domain, kind, label, taint.bits(), at_ns as i64, permanent),
    )?;
    Ok(conn.last_insert_rowid())
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntityRow {
    pub id: i64,
    pub domain: String,
    pub kind: String,
    pub label: String,
    pub taint: u32,
    pub created_ns: u64,
    pub last_seen_ns: u64,
    pub permanent: bool,
}

/// FR-KNO-10: entities the cold worker can build co-occurrence candidates
/// from. `since_ns = 0` means every entity; otherwise only those last seen
/// at or after `since_ns`.
pub fn list_entities(conn: &Connection, since_ns: u64) -> Result<Vec<EntityRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, domain, kind, label, taint, created_ns, last_seen_ns, permanent
         FROM entities WHERE last_seen_ns >= ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([since_ns as i64], |row| {
            Ok(EntityRow {
                id: row.get(0)?,
                domain: row.get(1)?,
                kind: row.get(2)?,
                label: row.get(3)?,
                taint: row.get::<_, i64>(4)? as u32,
                created_ns: row.get::<_, i64>(5)? as u64,
                last_seen_ns: row.get::<_, i64>(6)? as u64,
                permanent: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRow {
    pub src: i64,
    pub dst: i64,
    pub kind: String,
    pub weight: f32,
    pub reinforced_ns: u64,
    pub hypothesis: bool,
}

/// FR-KNO-10: every edge, for the cold worker's APPNP propagation input.
pub fn list_edges(conn: &Connection) -> Result<Vec<EdgeRow>, StorageError> {
    let mut stmt =
        conn.prepare("SELECT src, dst, kind, weight, reinforced_ns, hypothesis FROM edges")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EdgeRow {
                src: row.get(0)?,
                dst: row.get(1)?,
                kind: row.get(2)?,
                weight: row.get(3)?,
                reinforced_ns: row.get::<_, i64>(4)? as u64,
                hypothesis: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Creates `(src, dst, kind)` if absent, otherwise replaces its
/// weight/reinforced_ns/hypothesis. Re-upserting an edge the cold worker
/// still believes in is exactly how its 72h hypothesis TTL gets reset
/// (Architecture.md §7.2).
pub fn upsert_edge(conn: &Connection, edge: &EdgeRow) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO edges (src, dst, kind, weight, reinforced_ns, hypothesis)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(src, dst, kind) DO UPDATE SET
             weight = excluded.weight,
             reinforced_ns = excluded.reinforced_ns,
             hypothesis = excluded.hypothesis",
        (
            edge.src,
            edge.dst,
            &edge.kind,
            edge.weight,
            edge.reinforced_ns as i64,
            edge.hypothesis,
        ),
    )?;
    Ok(())
}

/// FR-KNO-10: "prune unreinforced edges after 72h" -- deletes hypothesis
/// edges whose `reinforced_ns` predates `older_than_ns`. Confirmed
/// (non-hypothesis) edges are never pruned by this call.
pub fn prune_hypothesis_edges(conn: &Connection, older_than_ns: u64) -> Result<u64, StorageError> {
    let pruned = conn.execute(
        "DELETE FROM edges WHERE hypothesis = 1 AND reinforced_ns < ?1",
        [older_than_ns as i64],
    )?;
    Ok(pruned as u64)
}

pub struct FocusHistoryEntry<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    pub pid: u32,
    pub root_pid: u32,
    pub t_start_ns: u64,
    pub t_end_ns: u64,
    pub dwell_ms: u64,
}

pub fn insert_focus_history(
    conn: &Connection,
    e: &FocusHistoryEntry<'_>,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO focus_history (app_id, title, pid, root_pid, t_start_ns, t_end_ns, dwell_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            e.app_id,
            e.title,
            e.pid,
            e.root_pid,
            e.t_start_ns as i64,
            e.t_end_ns as i64,
            e.dwell_ms as i64,
        ),
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct FocusHistoryRow {
    pub app_id: String,
    pub title: String,
    pub pid: u32,
    pub root_pid: u32,
    pub t_start_ns: u64,
    pub t_end_ns: u64,
    pub dwell_ms: u64,
}

/// FR-STO-06: `QueryFocusHistory(t, ±window)` — the deictic-snap query
/// ("what was I looking at when I said this"). Prefers a segment that
/// actually contains `t_ns`; failing that, the segment whose nearest edge
/// is closest to `t_ns`, among those overlapping `[t_ns - window_ns, t_ns +
/// window_ns]`. `None` if nothing overlaps at all.
pub fn query_focus_history(
    conn: &Connection,
    t_ns: u64,
    window_ns: u64,
) -> Result<Option<FocusHistoryRow>, StorageError> {
    let lo = t_ns.saturating_sub(window_ns) as i64;
    let hi = t_ns.saturating_add(window_ns) as i64;
    let t = t_ns as i64;
    conn.query_row(
        "SELECT app_id, title, pid, root_pid, t_start_ns, t_end_ns, dwell_ms
         FROM focus_history
         WHERE t_start_ns <= ?2 AND t_end_ns >= ?1
         ORDER BY
             CASE WHEN t_start_ns <= ?3 AND t_end_ns >= ?3 THEN 0 ELSE 1 END,
             MIN(ABS(t_start_ns - ?3), ABS(t_end_ns - ?3))
         LIMIT 1",
        (lo, hi, t),
        |row| {
            Ok(FocusHistoryRow {
                app_id: row.get(0)?,
                title: row.get(1)?,
                pid: row.get(2)?,
                root_pid: row.get(3)?,
                t_start_ns: row.get::<_, i64>(4)? as u64,
                t_end_ns: row.get::<_, i64>(5)? as u64,
                dwell_ms: row.get::<_, i64>(6)? as u64,
            })
        },
    )
    .optional()
    .map_err(StorageError::from)
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexMeta {
    pub embedding_model_id: String,
    pub dim: i64,
    pub index_kind: String,
    pub p99_ms: f64,
    pub updated_ns: i64,
}

/// FR-STO-11: which embedding model (if any) produced `collection`'s
/// currently-stored vectors. `None` means the collection has never been
/// indexed yet (nothing to compare a re-index decision against).
pub fn get_index_meta(
    conn: &Connection,
    collection: &str,
) -> Result<Option<IndexMeta>, StorageError> {
    conn.query_row(
        "SELECT embedding_model_id, dim, index_kind, p99_ms, updated_ns
         FROM index_meta WHERE collection = ?1",
        [collection],
        |row| {
            Ok(IndexMeta {
                embedding_model_id: row.get(0)?,
                dim: row.get(1)?,
                index_kind: row.get(2)?,
                p99_ms: row.get(3)?,
                updated_ns: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(StorageError::from)
}

/// FR-STO-11: records which model/index-kind `collection` is currently
/// indexed with — called after every insert (so the *next* model change
/// has something to compare against) and after a background re-index
/// completes (so the mismatch that triggered it clears).
pub fn upsert_index_meta(
    conn: &Connection,
    collection: &str,
    embedding_model_id: &str,
    dim: i64,
    index_kind: &str,
    p99_ms: f64,
    updated_ns: i64,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO index_meta (collection, embedding_model_id, dim, index_kind, p99_ms, updated_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(collection) DO UPDATE SET
             embedding_model_id = excluded.embedding_model_id,
             dim = excluded.dim,
             index_kind = excluded.index_kind,
             p99_ms = excluded.p99_ms,
             updated_ns = excluded.updated_ns",
        (collection, embedding_model_id, dim, index_kind, p99_ms, updated_ns),
    )?;
    Ok(())
}

/// The entities [`forget_by_app`] will delete. The caller reads these
/// *before* forgetting, so it can delete the matching LanceDB rows while
/// the ids are still known (a failed forget is then safely retryable).
pub fn entity_ids_for_app(conn: &Connection, app_id: &str) -> Result<Vec<i64>, StorageError> {
    collect_ids(conn, "SELECT id FROM entities WHERE label = ?1", [app_id])
}

/// The entities [`forget_since`] will delete (created at or after the cutoff).
pub fn entity_ids_created_since(
    conn: &Connection,
    since_ns: u64,
) -> Result<Vec<i64>, StorageError> {
    collect_ids(
        conn,
        "SELECT id FROM entities WHERE created_ns >= ?1",
        [since_ns as i64],
    )
}

/// FR-STO-12/FR-PRV-03 ("forget"): deletes every row tied to `app_id`
/// (entities and their edges, chunk metadata and keyword rows, focus
/// history, event counters) in one transaction, and returns the deleted
/// entities' ids. LanceDB rows are the caller's job.
pub fn forget_by_app(conn: &Connection, app_id: &str) -> Result<Vec<i64>, StorageError> {
    let tx = conn.unchecked_transaction()?;
    let entity_ids = entity_ids_for_app(&tx, app_id)?;
    delete_entities(&tx, &entity_ids)?;
    tx.execute("DELETE FROM focus_history WHERE app_id = ?1", [app_id])?;
    tx.execute("DELETE FROM event_counters WHERE key = ?1", [app_id])?;
    tx.commit()?;
    Ok(entity_ids)
}

/// Same, but for everything recorded at or after `since_ns` (an absolute
/// UTC-ns cutoff — `neuroosctl forget --since 1h` becomes
/// `since_ns = now - 1h` at the call site). Entities created in the window are deleted
/// outright; an older entity keeps its row but loses every chunk and
/// focus segment from the window — including a segment that started
/// before the cutoff but was still on screen after it.
pub fn forget_since(conn: &Connection, since_ns: u64) -> Result<Vec<i64>, StorageError> {
    let since = since_ns as i64;
    let tx = conn.unchecked_transaction()?;
    let entity_ids = entity_ids_created_since(&tx, since_ns)?;
    delete_entities(&tx, &entity_ids)?;
    tx.execute("DELETE FROM chunks_fts WHERE t_ns >= ?1", [since])?;
    tx.execute("DELETE FROM focus_history WHERE t_end_ns >= ?1", [since])?;
    tx.execute("DELETE FROM event_counters WHERE first_ns >= ?1", [since])?;
    tx.commit()?;
    Ok(entity_ids)
}

/// Deletes `entity_ids` and every row that references them. Edges go
/// first: their foreign keys would otherwise reject the entity delete.
pub(crate) fn delete_entities(conn: &Connection, entity_ids: &[i64]) -> Result<(), StorageError> {
    let mut edges = conn.prepare("DELETE FROM edges WHERE src = ?1 OR dst = ?1")?;
    let mut meta = conn.prepare("DELETE FROM chunks_meta WHERE entity_id = ?1")?;
    let mut entities = conn.prepare("DELETE FROM entities WHERE id = ?1")?;
    for id in entity_ids {
        edges.execute([id])?;
        meta.execute([id])?;
        entities.execute([id])?;
    }
    delete_chunks_fts(conn, entity_ids)
}

fn collect_ids<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<i64>, StorageError> {
    let mut stmt = conn.prepare(sql)?;
    let ids = stmt
        .query_map(params, |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

/// One row of `chunks_fts` (BUG-007(b)), mirroring a LanceDB chunk.
pub struct FtsChunk<'a> {
    pub chunk_id: &'a str,
    pub entity_id: i64,
    pub domain: &'a str,
    pub taint: u32,
    pub t_ns: u64,
    pub text: &'a str,
}

pub fn insert_chunk_fts(conn: &Connection, c: &FtsChunk<'_>) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO chunks_fts (text, aliases, chunk_id, entity_id, domain, taint, t_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            c.text,
            search_aliases(c.text),
            c.chunk_id,
            c.entity_id,
            c.domain,
            c.taint,
            c.t_ns as i64,
        ),
    )?;
    Ok(())
}

/// Search-only spellings of `text` that tokenization would otherwise lose:
/// dotted acronyms joined ("J.A.R.V.I.S" -> "JARVIS", since the tokenizer
/// sees single letters), and "reddit subreddit" for an `r/<name>` mention.
pub fn search_aliases(text: &str) -> String {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let letters: Vec<&str> = word.split('.').filter(|p| !p.is_empty()).collect();
        if letters.len() >= 3 && letters.iter().all(|p| p.chars().count() == 1) {
            out.push(letters.concat());
        }
        if word.starts_with("r/") && word.len() > 2 {
            out.push("reddit subreddit".to_string());
        }
    }
    out.join(" ")
}

/// The `index_meta` row to write alongside a chunk (see
/// [`insert_chunk_with_index_meta`]); the borrowed counterpart of
/// [`IndexMeta`], which is what a *read* returns.
pub struct IndexMetaWrite<'a> {
    pub collection: &'a str,
    pub embedding_model_id: &'a str,
    pub dim: i64,
    pub index_kind: &'a str,
    pub p99_ms: f64,
    pub updated_ns: i64,
}

/// M15: writes a chunk's keyword row and its family's `index_meta` row in
/// one transaction. They used to be two independent statements, so a
/// failure on the second (a disk error, a `SQLITE_BUSY`) left a searchable
/// chunk behind whose family's recorded index state no longer matched what
/// had just been written -- which is exactly what FR-STO-11's re-index
/// decision reads.
pub fn insert_chunk_with_index_meta(
    conn: &Connection,
    chunk: &FtsChunk<'_>,
    meta: &IndexMetaWrite<'_>,
) -> Result<(), StorageError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        insert_chunk_fts(conn, chunk)?;
        upsert_index_meta(
            conn,
            meta.collection,
            meta.embedding_model_id,
            meta.dim,
            meta.index_kind,
            meta.p99_ms,
            meta.updated_ns,
        )
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            // The rollback itself failing would mask the real cause, so it
            // is logged rather than returned (rules.md R0-6: no payloads).
            if let Err(rollback) = conn.execute_batch("ROLLBACK") {
                tracing::error!(error = %rollback, "failed to roll back a partial chunk write");
            }
            Err(err)
        }
    }
}

/// Removes every keyword row for `entity_ids` (forget/GC, alongside the
/// matching LanceDB rows).
pub fn delete_chunks_fts(conn: &Connection, entity_ids: &[i64]) -> Result<(), StorageError> {
    let mut stmt = conn.prepare("DELETE FROM chunks_fts WHERE entity_id = ?1")?;
    for id in entity_ids {
        stmt.execute([id])?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct FtsHit {
    pub chunk_id: String,
    pub entity_id: i64,
    pub domain: String,
    pub taint: u32,
    pub t_ns: u64,
    pub text: String,
    /// `-bm25()`: higher is a stronger keyword match.
    pub score: f64,
}

/// Question words, pronouns and auxiliaries: they carry no signal about
/// *which* activity is meant, and in OR mode they'd match everything.
const FTS_STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "am", "an", "and", "any", "anything", "are", "as", "at", "be",
    "been", "before", "by", "can", "could", "did", "do", "does", "doing", "during", "for", "from",
    "had", "has", "have", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "of",
    "on", "or", "so", "some", "than", "that", "the", "their", "them", "then", "there", "these",
    "this", "those", "to", "was", "we", "were", "what", "when", "where", "which", "while", "who",
    "whom", "why", "will", "with", "would", "you", "your",
];

/// Keyword terms of a free-text question: lowercased alphanumeric runs,
/// minus stopwords, each double-quoted so no FTS5 operator in user text
/// is ever interpreted.
pub fn fts_terms(question: &str) -> Vec<String> {
    question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|t| !t.is_empty() && !FTS_STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// BUG-007(b): BM25 keyword search over chunk text, any-term (OR) match,
/// best first. Empty when the question has no non-stopword terms.
pub fn search_chunks_fts(
    conn: &Connection,
    question: &str,
    limit: usize,
) -> Result<Vec<FtsHit>, StorageError> {
    let terms = fts_terms(question);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let expr = terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut stmt = conn.prepare(
        "SELECT chunk_id, entity_id, domain, taint, t_ns, text, -bm25(chunks_fts)
         FROM chunks_fts WHERE chunks_fts MATCH ?1
         ORDER BY bm25(chunks_fts) LIMIT ?2",
    )?;
    let rows = stmt.query_map((expr, limit as i64), |r| {
        Ok(FtsHit {
            chunk_id: r.get(0)?,
            entity_id: r.get(1)?,
            domain: r.get(2)?,
            taint: r.get(3)?,
            t_ns: r.get::<_, i64>(4)? as u64,
            text: r.get(5)?,
            score: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One aggregated activity row (BUG-007's `QueryActivity`).
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityRow {
    pub app_id: String,
    pub text: String,
    pub first_ns: u64,
    pub last_ns: u64,
    pub dwell_ms: u64,
    /// M9 / ADR-0012: `TaintFlags::bits()` for `text`'s provenance.
    pub taint: u32,
}

fn upper(until_ns: u64) -> i64 {
    if until_ns == 0 {
        i64::MAX
    } else {
        until_ns as i64
    }
}

/// Focused window titles overlapping `[since_ns, until_ns]`, grouped per
/// (app, title), most total dwell first.
pub fn activity_windows(
    conn: &Connection,
    since_ns: u64,
    until_ns: u64,
    limit: usize,
) -> Result<Vec<ActivityRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT app_id, title, MIN(t_start_ns), MAX(t_end_ns), SUM(dwell_ms)
         FROM focus_history
         WHERE t_end_ns >= ?1 AND t_start_ns <= ?2 AND title != ''
         GROUP BY app_id, title
         ORDER BY SUM(dwell_ms) DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map((since_ns as i64, upper(until_ns), limit as i64), |r| {
        Ok(ActivityRow {
            app_id: r.get(0)?,
            text: r.get(1)?,
            first_ns: r.get::<_, i64>(2)? as u64,
            last_ns: r.get::<_, i64>(3)? as u64,
            dwell_ms: r.get::<_, i64>(4)? as u64,
            // M9 / ADR-0012: `focus_history` is C1's own observation of
            // this desktop and has no taint column -- empty is a positive
            // statement about first-party telemetry, not a missing value.
            // If a non-C1 writer ever lands focus rows, revisit this.
            taint: 0,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Media first seen playing in `[since_ns, until_ns]`, oldest first (the
/// `media_playback` chunks, which name player, title and artist).
pub fn activity_media(
    conn: &Connection,
    since_ns: u64,
    until_ns: u64,
    limit: usize,
) -> Result<Vec<ActivityRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT text, t_ns, taint FROM chunks_fts
         WHERE domain = 'media_playback' AND t_ns >= ?1 AND t_ns <= ?2
         ORDER BY t_ns LIMIT ?3",
    )?;
    let rows = stmt.query_map((since_ns as i64, upper(until_ns), limit as i64), |r| {
        let t: i64 = r.get(1)?;
        Ok(ActivityRow {
            app_id: String::new(),
            text: r.get(0)?,
            first_ns: t as u64,
            last_ns: t as u64,
            dwell_ms: 0,
            // M9 / ADR-0012: the chunk's real taint, which this path used
            // to discard -- a media chunk derived from a C7-fetched page
            // is EXTERNAL_UNTRUSTED and must stay so (R0-3).
            taint: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    /// M15: a chunk's keyword row and its family's `index_meta` row were
    /// two separate statements, so a failure on the second left a
    /// searchable chunk whose family claims no (or a stale) index. Both
    /// now land in one transaction.
    #[test]
    fn a_failed_index_meta_write_rolls_back_the_keyword_row() {
        let conn = open_in_memory().unwrap();
        conn.execute_batch("DROP TABLE index_meta").unwrap();
        let err = insert_chunk_with_index_meta(
            &conn,
            &FtsChunk {
                chunk_id: "c1",
                entity_id: 1,
                domain: "window_focus",
                taint: 0,
                t_ns: 1,
                text: "a searchable chunk",
            },
            &IndexMetaWrite {
                collection: "attention",
                embedding_model_id: "m",
                dim: 384,
                index_kind: "flat",
                p99_ms: 0.0,
                updated_ns: 1,
            },
        );
        assert!(
            err.is_err(),
            "the missing index_meta table must be an error"
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "the keyword row must not survive the rollback");
    }

    #[test]
    fn a_chunk_and_its_index_meta_are_written_together() {
        let conn = open_in_memory().unwrap();
        insert_chunk_with_index_meta(
            &conn,
            &FtsChunk {
                chunk_id: "c1",
                entity_id: 1,
                domain: "window_focus",
                taint: 0,
                t_ns: 1,
                text: "a searchable chunk",
            },
            &IndexMetaWrite {
                collection: "attention",
                embedding_model_id: "m",
                dim: 384,
                index_kind: "flat",
                p99_ms: 0.0,
                updated_ns: 1,
            },
        )
        .unwrap();
        let chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        let meta: String = conn
            .query_row(
                "SELECT index_kind FROM index_meta WHERE collection = 'attention'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(chunks, 1);
        assert_eq!(meta, "flat");
    }

    /// M17: a migration was a bare `execute_batch` followed by a separate
    /// `INSERT` into `schema_migrations`. A failure anywhere in between --
    /// a crash, a disk error, a statement that is wrong on this SQLite
    /// build -- left the tables the batch had already created with no
    /// version row to say so, so every later `open()` re-ran the whole
    /// migration and died on "table ... already exists". The database
    /// could never be opened again.
    #[test]
    fn a_migration_that_fails_part_way_leaves_nothing_behind() {
        let conn = Connection::open_in_memory().unwrap();
        let bad: &[(&str, &str)] = &[(
            "0001_bad",
            "CREATE TABLE half_done (x INTEGER);
             CREATE TABLE this_fails (y NOT_A_TYPE PRIMARY KEY AUTOINCREMENT);",
        )];
        assert!(apply_migrations(&conn, bad).is_err());
        assert!(!table_exists(&conn, "half_done"));
        let recorded: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded, 0);
    }

    /// The symptom this is really about: after a failed migration the
    /// database must still be openable.
    #[test]
    fn a_failed_migration_can_be_retried_once_the_fault_is_gone() {
        let conn = Connection::open_in_memory().unwrap();
        let bad: &[(&str, &str)] = &[(
            "0001_initial",
            "CREATE TABLE things (x INTEGER);
             CREATE TABLE this_fails (y NOT_A_TYPE PRIMARY KEY AUTOINCREMENT);",
        )];
        assert!(apply_migrations(&conn, bad).is_err());
        let fixed: &[(&str, &str)] = &[("0001_initial", "CREATE TABLE things (x INTEGER);")];
        apply_migrations(&conn, fixed).expect("the retry must not hit 'table already exists'");
        assert!(table_exists(&conn, "things"));
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn open_applies_migrations_and_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, MIGRATIONS.len() as i64);
        // re-running migrate() on the same connection must not error or
        // re-apply (CREATE TABLE would fail the second time if it did).
        migrate(&conn).unwrap();
    }

    /// M9 / ADR-0012: `activity_media` reads `chunks_fts`, whose rows
    /// carry a real taint, but `ActivityRow` had no taint field so the
    /// value was dropped on the floor -- a media item derived from a
    /// C7-fetched page lost its `EXTERNAL_UNTRUSTED` on the way to C5.
    #[test]
    fn activity_media_carries_each_chunks_taint() {
        let conn = open_in_memory().unwrap();
        let external = neuroos_taint::TaintFlags::EXTERNAL_UNTRUSTED.bits();
        insert_chunk_fts(
            &conn,
            &FtsChunk {
                chunk_id: "local",
                entity_id: 1,
                domain: "media_playback",
                taint: 0,
                t_ns: 1_000,
                text: "vlc played a local file",
            },
        )
        .unwrap();
        insert_chunk_fts(
            &conn,
            &FtsChunk {
                chunk_id: "fetched",
                entity_id: 2,
                domain: "media_playback",
                taint: external,
                t_ns: 2_000,
                text: "browser played a web video",
            },
        )
        .unwrap();

        let rows = activity_media(&conn, 0, 0, 10).unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].taint, 0, "the local chunk is untainted");
        assert_eq!(
            rows[1].taint, external,
            "the fetched chunk must keep EXTERNAL_UNTRUSTED"
        );
    }

    /// `focus_history` has no taint column because it is C1's own
    /// first-party observation; empty is the correct positive value, not a
    /// missing one (ADR-0012).
    #[test]
    fn activity_windows_report_empty_taint() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO focus_history
             (app_id, title, pid, root_pid, t_start_ns, t_end_ns, dwell_ms)
             VALUES ('vlc', 'Show E03', 0, 0, 1000, 2000, 1000)",
            [],
        )
        .unwrap();
        let rows = activity_windows(&conn, 0, 0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].taint, 0);
    }

    #[test]
    fn wal_mode_and_pragmas_apply_on_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.sqlite3");
        let conn = open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(conn);
        assert!(path.exists());
    }

    #[test]
    fn touch_event_counter_creates_then_accumulates() {
        let conn = open_in_memory().unwrap();
        touch_event_counter(&conn, "process_activity", "editor", 100, 50).unwrap();
        touch_event_counter(&conn, "process_activity", "editor", 200, 30).unwrap();
        let (count, total_dwell_ms): (i64, i64) = conn
            .query_row(
                "SELECT count, total_dwell_ms FROM event_counters WHERE domain = ?1 AND key = ?2",
                ("process_activity", "editor"),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(total_dwell_ms, 80);
    }

    #[test]
    fn upsert_entity_creates_once_then_updates_last_seen_and_unions_taint() {
        let conn = open_in_memory().unwrap();
        let id1 = upsert_entity(
            &conn,
            "external",
            "document",
            "doc-42",
            TaintFlags::EXTERNAL_UNTRUSTED,
            1,
            false,
        )
        .unwrap();
        let id2 = upsert_entity(
            &conn,
            "external",
            "document",
            "doc-42",
            TaintFlags::MODEL_GENERATED,
            2,
            false,
        )
        .unwrap();
        assert_eq!(id1, id2);
        let (last_seen, taint_bits): (i64, u32) = conn
            .query_row(
                "SELECT last_seen_ns, taint FROM entities WHERE id = ?1",
                [id1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_seen, 2);
        let taint = TaintFlags::from_bits_truncate(taint_bits);
        assert!(taint.contains(TaintFlags::EXTERNAL_UNTRUSTED));
        assert!(taint.contains(TaintFlags::MODEL_GENERATED)); // union, not replace
    }

    #[test]
    fn insert_focus_history_round_trips() {
        let conn = open_in_memory().unwrap();
        insert_focus_history(
            &conn,
            &FocusHistoryEntry {
                app_id: "org.mozilla.firefox",
                title: "Example",
                pid: 123,
                root_pid: 100,
                t_start_ns: 1_000,
                t_end_ns: 2_000,
                dwell_ms: 1,
            },
        )
        .unwrap();
        let app_id: String = conn
            .query_row("SELECT app_id FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, "org.mozilla.firefox");
    }

    fn seed_two_sessions(conn: &Connection) {
        insert_focus_history(
            conn,
            &FocusHistoryEntry {
                app_id: "editor",
                title: "main.rs",
                pid: 1,
                root_pid: 1,
                t_start_ns: 1_000_000_000,
                t_end_ns: 2_000_000_000,
                dwell_ms: 1_000,
            },
        )
        .unwrap();
        insert_focus_history(
            conn,
            &FocusHistoryEntry {
                app_id: "browser",
                title: "docs",
                pid: 2,
                root_pid: 2,
                t_start_ns: 5_000_000_000,
                t_end_ns: 6_000_000_000,
                dwell_ms: 1_000,
            },
        )
        .unwrap();
    }

    #[test]
    fn query_focus_history_finds_the_session_containing_the_timestamp() {
        let conn = open_in_memory().unwrap();
        seed_two_sessions(&conn);
        let row = query_focus_history(&conn, 1_500_000_000, 1_500_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(row.app_id, "editor");
    }

    #[test]
    fn query_focus_history_finds_the_nearest_session_within_window_when_not_contained() {
        let conn = open_in_memory().unwrap();
        seed_two_sessions(&conn);
        // 2.3s: 0.3s after editor's session ends, well before browser's starts
        let row = query_focus_history(&conn, 2_300_000_000, 1_500_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(row.app_id, "editor");
    }

    #[test]
    fn query_focus_history_outside_every_window_is_none() {
        let conn = open_in_memory().unwrap();
        seed_two_sessions(&conn);
        let row = query_focus_history(&conn, 100_000_000_000, 1_500_000_000).unwrap();
        assert_eq!(row, None);
    }

    #[test]
    fn forget_by_app_deletes_only_that_apps_rows() {
        let conn = open_in_memory().unwrap();
        seed_two_sessions(&conn); // "editor" and "browser"
        let editor_id = upsert_entity(
            &conn,
            "window_focus",
            "window",
            "editor",
            TaintFlags::empty(),
            1,
            false,
        )
        .unwrap();
        upsert_entity(
            &conn,
            "window_focus",
            "window",
            "browser",
            TaintFlags::empty(),
            1,
            false,
        )
        .unwrap();
        touch_event_counter(&conn, "window_focus", "editor", 1, 0).unwrap();

        let deleted = forget_by_app(&conn, "editor").unwrap();
        assert_eq!(deleted, vec![editor_id]);

        let remaining_history: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM focus_history WHERE app_id = 'editor'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining_history, 0);
        let browser_history: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM focus_history WHERE app_id = 'browser'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            browser_history, 1,
            "forgetting one app must not touch another"
        );
        let editor_counters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM event_counters WHERE key = 'editor'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(editor_counters, 0);
    }

    #[test]
    fn forget_since_deletes_only_rows_at_or_after_the_cutoff() {
        let conn = open_in_memory().unwrap();
        seed_two_sessions(&conn); // editor: t_start=1s, browser: t_start=5s
        upsert_entity(
            &conn,
            "window_focus",
            "window",
            "editor",
            TaintFlags::empty(),
            1_000_000_000,
            false,
        )
        .unwrap();
        upsert_entity(
            &conn,
            "window_focus",
            "window",
            "browser",
            TaintFlags::empty(),
            5_000_000_000,
            false,
        )
        .unwrap();

        let deleted = forget_since(&conn, 3_000_000_000).unwrap();
        assert_eq!(deleted.len(), 1); // only "browser"'s entity (created_ns=5s)

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            remaining, 1,
            "only editor's (t_start=1s) session should remain"
        );
        let app_id: String = conn
            .query_row("SELECT app_id FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, "editor");
    }

    #[test]
    fn index_meta_is_none_before_first_write() {
        let conn = open_in_memory().unwrap();
        assert_eq!(get_index_meta(&conn, "attention").unwrap(), None);
    }

    #[test]
    fn index_meta_upsert_then_read_round_trips() {
        let conn = open_in_memory().unwrap();
        upsert_index_meta(
            &conn,
            "attention",
            "bge-small-en-v1.5",
            384,
            "flat",
            1.5,
            1000,
        )
        .unwrap();
        let meta = get_index_meta(&conn, "attention").unwrap().unwrap();
        assert_eq!(meta.embedding_model_id, "bge-small-en-v1.5");
        assert_eq!(meta.dim, 384);
        assert_eq!(meta.index_kind, "flat");
        assert_eq!(meta.updated_ns, 1000);
    }

    #[test]
    fn index_meta_upsert_overwrites_not_duplicates() {
        let conn = open_in_memory().unwrap();
        upsert_index_meta(&conn, "attention", "model-a", 384, "flat", 0.0, 1000).unwrap();
        upsert_index_meta(&conn, "attention", "model-b", 384, "hnsw", 2.0, 2000).unwrap();
        let meta = get_index_meta(&conn, "attention").unwrap().unwrap();
        assert_eq!(meta.embedding_model_id, "model-b");
        assert_eq!(meta.index_kind, "hnsw");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM index_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    fn two_entities(conn: &Connection) -> (i64, i64) {
        let a = upsert_entity(
            conn,
            "window_focus",
            "window",
            "a",
            TaintFlags::empty(),
            1,
            false,
        )
        .unwrap();
        let b = upsert_entity(
            conn,
            "window_focus",
            "window",
            "b",
            TaintFlags::empty(),
            1,
            false,
        )
        .unwrap();
        (a, b)
    }

    #[test]
    fn list_entities_since_ns_filters_out_stale_entities() {
        let conn = open_in_memory().unwrap();
        upsert_entity(
            &conn,
            "window_focus",
            "window",
            "old",
            TaintFlags::empty(),
            100,
            false,
        )
        .unwrap();
        upsert_entity(
            &conn,
            "window_focus",
            "window",
            "new",
            TaintFlags::empty(),
            500,
            false,
        )
        .unwrap();
        let rows = list_entities(&conn, 300).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "new");
    }

    #[test]
    fn list_entities_zero_since_ns_returns_everything() {
        let conn = open_in_memory().unwrap();
        two_entities(&conn);
        assert_eq!(list_entities(&conn, 0).unwrap().len(), 2);
    }

    #[test]
    fn upsert_edge_creates_then_updates_in_place() {
        let conn = open_in_memory().unwrap();
        let (a, b) = two_entities(&conn);
        upsert_edge(
            &conn,
            &EdgeRow {
                src: a,
                dst: b,
                kind: "co_occurs".into(),
                weight: 1.0,
                reinforced_ns: 100,
                hypothesis: true,
            },
        )
        .unwrap();
        upsert_edge(
            &conn,
            &EdgeRow {
                src: a,
                dst: b,
                kind: "co_occurs".into(),
                weight: 2.5,
                reinforced_ns: 200,
                hypothesis: false,
            },
        )
        .unwrap();
        let edges = list_edges(&conn).unwrap();
        assert_eq!(
            edges.len(),
            1,
            "same (src,dst,kind) must update, not duplicate"
        );
        assert_eq!(edges[0].weight, 2.5);
        assert_eq!(edges[0].reinforced_ns, 200);
        assert!(!edges[0].hypothesis);
    }

    #[test]
    fn prune_hypothesis_edges_only_removes_stale_hypotheses() {
        let conn = open_in_memory().unwrap();
        let (a, b) = two_entities(&conn);
        upsert_edge(
            &conn,
            &EdgeRow {
                src: a,
                dst: b,
                kind: "stale_hypothesis".into(),
                weight: 1.0,
                reinforced_ns: 0,
                hypothesis: true,
            },
        )
        .unwrap();
        upsert_edge(
            &conn,
            &EdgeRow {
                src: a,
                dst: b,
                kind: "fresh_hypothesis".into(),
                weight: 1.0,
                reinforced_ns: 1_000,
                hypothesis: true,
            },
        )
        .unwrap();
        upsert_edge(
            &conn,
            &EdgeRow {
                src: a,
                dst: b,
                kind: "old_but_confirmed".into(),
                weight: 1.0,
                reinforced_ns: 0,
                hypothesis: false,
            },
        )
        .unwrap();

        let pruned = prune_hypothesis_edges(&conn, 500).unwrap();
        assert_eq!(pruned, 1);
        let remaining: Vec<String> = list_edges(&conn)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(remaining.contains(&"fresh_hypothesis".to_string()));
        assert!(remaining.contains(&"old_but_confirmed".to_string()));
        assert!(!remaining.contains(&"stale_hypothesis".to_string()));
    }

    // phases.md §7.3 SC: "SQL injection attempts via titles are inert
    // (parameterised)." Every query in this module already binds values
    // via rusqlite's `?N` placeholders (never string-formatted SQL), so
    // this is a real proof of that, not a synthetic worry -- a malicious
    // title/app_id is stored as inert data, never executed.
    #[test]
    fn sql_injection_via_focus_history_title_is_inert() {
        let conn = open_in_memory().unwrap();
        let payload = "'; DROP TABLE focus_history; --";
        insert_focus_history(
            &conn,
            &FocusHistoryEntry {
                app_id: payload,
                title: payload,
                pid: 0,
                root_pid: 0,
                t_start_ns: 0,
                t_end_ns: 1,
                dwell_ms: 1,
            },
        )
        .unwrap();

        // The table must still exist (a real DROP would make this query fail).
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // The payload must be stored as inert literal text, not executed.
        let (stored_app_id, stored_title): (String, String) = conn
            .query_row("SELECT app_id, title FROM focus_history LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(stored_app_id, payload);
        assert_eq!(stored_title, payload);
    }

    #[test]
    fn sql_injection_via_entity_label_is_inert() {
        let conn = open_in_memory().unwrap();
        let payload = "x'; DROP TABLE entities; --";
        upsert_entity(
            &conn,
            "notes",
            "note",
            payload,
            TaintFlags::empty(),
            0,
            false,
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let stored_label: String = conn
            .query_row("SELECT label FROM entities LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_label, payload);
    }

    fn fts_row(conn: &Connection, id: &str, entity_id: i64, text: &str) {
        insert_chunk_fts(
            conn,
            &FtsChunk {
                chunk_id: id,
                entity_id,
                domain: "window_focus",
                taint: 0,
                t_ns: 1,
                text,
            },
        )
        .unwrap();
    }

    #[test]
    fn fts_finds_a_rare_name_token_inside_a_title() {
        let conn = open_in_memory().unwrap();
        fts_row(
            &conn,
            "a",
            1,
            "brave-browser: VaughnValle/lush-pop: A clean and green Linux setup",
        );
        fts_row(
            &conn,
            "b",
            2,
            "vlc: High.School.Return.of.a.Gangster.S01E03.720p",
        );
        let hits = search_chunks_fts(&conn, "Who made the lush-pop repository?", 5).unwrap();
        assert_eq!(hits.first().map(|h| h.chunk_id.as_str()), Some("a"));
        let hits = search_chunks_fts(&conn, "which gangster episodes", 5).unwrap();
        assert_eq!(hits.first().map(|h| h.chunk_id.as_str()), Some("b"));
    }

    #[test]
    fn fts_stems_query_words() {
        let conn = open_in_memory().unwrap();
        fts_row(&conn, "a", 1, "COSMIC Terminal: sudo apt install lightdm");
        let hits = search_chunks_fts(&conn, "Did I installed lightdm?", 5).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn fts_ignores_stopwords_and_fts_syntax_in_questions() {
        let conn = open_in_memory().unwrap();
        fts_row(&conn, "a", 1, "what did I do");
        // only stopwords: no keyword search at all
        assert!(
            search_chunks_fts(&conn, "What did I do?", 5)
                .unwrap()
                .is_empty()
        );
        // FTS5 operators/quotes in user text must not error
        assert!(search_chunks_fts(&conn, "\"NEAR(a b) OR * -x\" AND", 5).is_ok());
    }

    #[test]
    fn fts_rows_are_deleted_by_entity() {
        let conn = open_in_memory().unwrap();
        fts_row(&conn, "a", 7, "lightdm");
        fts_row(&conn, "b", 8, "lightdm again");
        delete_chunks_fts(&conn, &[7]).unwrap();
        let hits = search_chunks_fts(&conn, "lightdm", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity_id, 8);
    }

    fn focus(conn: &Connection, app: &str, title: &str, start: u64, end: u64) {
        insert_focus_history(
            conn,
            &FocusHistoryEntry {
                app_id: app,
                title,
                pid: 0,
                root_pid: 0,
                t_start_ns: start,
                t_end_ns: end,
                dwell_ms: (end - start) / 1_000_000,
            },
        )
        .unwrap();
    }

    #[test]
    fn activity_windows_ranks_titles_by_total_dwell_within_range() {
        const S: u64 = 1_000_000_000;
        let conn = open_in_memory().unwrap();
        focus(&conn, "vlc", "Show E01", 0, 600 * S);
        focus(&conn, "brave", "Docs", 600 * S, 660 * S);
        focus(&conn, "brave", "Docs", 700 * S, 760 * S);
        focus(&conn, "term", "late", 5000 * S, 5100 * S);
        let rows = activity_windows(&conn, 0, 1000 * S, 10).unwrap();
        let texts: Vec<_> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["Show E01", "Docs"]);
        assert_eq!(rows[1].dwell_ms, 120_000);
        assert_eq!(rows[1].first_ns, 600 * S);
        // until_ns = 0 is unbounded
        assert_eq!(activity_windows(&conn, 0, 0, 10).unwrap().len(), 3);
    }

    #[test]
    fn activity_media_lists_media_chunks_oldest_first() {
        let conn = open_in_memory().unwrap();
        for (id, t) in [("b", 20u64), ("a", 10)] {
            insert_chunk_fts(
                &conn,
                &FtsChunk {
                    chunk_id: id,
                    entity_id: 1,
                    domain: "media_playback",
                    taint: 0,
                    t_ns: t,
                    text: &format!("vlc played \"{id}\""),
                },
            )
            .unwrap();
        }
        fts_row(&conn, "w", 2, "not media");
        let rows = activity_media(&conn, 0, 0, 10).unwrap();
        let texts: Vec<_> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["vlc played \"a\"", "vlc played \"b\""]);
    }

    #[test]
    fn aliases_join_dotted_acronyms_and_name_subreddits() {
        assert_eq!(
            search_aliases("J.A.R.V.I.S Animated Theme : r/omarchy - Brave"),
            "JARVIS reddit subreddit"
        );
        assert_eq!(search_aliases("High.School.Return.of.a.Gangster"), "");
        let conn = open_in_memory().unwrap();
        fts_row(
            &conn,
            "a",
            1,
            "J.A.R.V.I.S Animated Theme : r/omarchy - Brave",
        );
        let hits = search_chunks_fts(
            &conn,
            "What Reddit post about a Jarvis theme did I open?",
            5,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].text,
            "J.A.R.V.I.S Animated Theme : r/omarchy - Brave"
        );
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn fts_row_at(conn: &Connection, id: &str, entity_id: i64, domain: &str, t_ns: u64) {
        insert_chunk_fts(
            conn,
            &FtsChunk {
                chunk_id: id,
                entity_id,
                domain,
                taint: 0,
                t_ns,
                text: id,
            },
        )
        .unwrap();
    }

    /// H1: the cold worker's edges reference entities with a foreign key;
    /// forget must remove them first instead of failing on the constraint.
    #[test]
    fn forget_by_app_succeeds_when_edges_reference_the_entity() {
        let conn = open_in_memory().unwrap();
        let (editor, browser) = two_entities(&conn);
        upsert_edge(
            &conn,
            &EdgeRow {
                src: editor,
                dst: browser,
                kind: "co_occurs".into(),
                weight: 1.0,
                reinforced_ns: 1,
                hypothesis: true,
            },
        )
        .unwrap();
        fts_row(&conn, "editor-chunk", editor, "editor text");

        let label: String = conn
            .query_row("SELECT label FROM entities WHERE id = ?1", [editor], |r| {
                r.get(0)
            })
            .unwrap();
        let deleted = forget_by_app(&conn, &label).unwrap();

        assert_eq!(deleted, vec![editor]);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM edges"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks_fts"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM entities"), 1);
    }

    #[test]
    fn forget_since_succeeds_when_edges_reference_the_entity() {
        let conn = open_in_memory().unwrap();
        let old = upsert_entity(&conn, "d", "k", "old", TaintFlags::empty(), 1, false).unwrap();
        let new = upsert_entity(&conn, "d", "k", "new", TaintFlags::empty(), 100, false).unwrap();
        upsert_edge(
            &conn,
            &EdgeRow {
                src: old,
                dst: new,
                kind: "co_occurs".into(),
                weight: 1.0,
                reinforced_ns: 1,
                hypothesis: true,
            },
        )
        .unwrap();

        let deleted = forget_since(&conn, 50).unwrap();

        assert_eq!(deleted, vec![new]);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM edges"), 0);
    }

    /// H2: an entity created before the cutoff (a long-lived app) must
    /// still lose every chunk and focus segment from the forgotten window.
    #[test]
    fn forget_since_removes_recent_rows_of_an_older_entity() {
        let conn = open_in_memory().unwrap();
        let app = upsert_entity(
            &conn,
            "window_focus",
            "window",
            "browser",
            TaintFlags::empty(),
            1_000,
            false,
        )
        .unwrap();
        fts_row_at(&conn, "before", app, "window_focus", 1_000);
        fts_row_at(&conn, "after", app, "window_focus", 9_000);
        // a session that started before the cutoff but was on screen after it
        insert_focus_history(
            &conn,
            &FocusHistoryEntry {
                app_id: "browser",
                title: "straddling",
                pid: 0,
                root_pid: 0,
                t_start_ns: 4_000,
                t_end_ns: 6_000,
                dwell_ms: 0,
            },
        )
        .unwrap();

        let deleted = forget_since(&conn, 5_000).unwrap();

        assert!(deleted.is_empty(), "the entity itself predates the cutoff");
        let remaining: Vec<String> = {
            let mut stmt = conn.prepare("SELECT chunk_id FROM chunks_fts").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(remaining, vec!["before".to_string()]);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM focus_history"), 0);
    }

    /// H4: `PRAGMA incremental_vacuum` only reclaims space when the file
    /// was switched to incremental auto-vacuum.
    #[test]
    fn open_enables_incremental_auto_vacuum_on_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.sqlite3");
        // an existing database created without it is converted too
        drop(Connection::open(&path).unwrap());
        let conn = open(&path).unwrap();
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, 2, "2 = INCREMENTAL");
    }
}
