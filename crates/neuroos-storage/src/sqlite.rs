//! `meta.sqlite3` (Architecture.md §7.2): WAL mode, `synchronous=NORMAL`,
//! single writer (this process). Owns `focus_history`, `event_counters`,
//! `aggregates_daily`, `entities`, `edges`, `chunks_meta`, `index_meta`.
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

use neuroos_taint::TaintFlags;

/// Every `.sql` file under `src/migrations/`, embedded at compile time
/// (`include_str!`, not a runtime file read — this binary must work from
/// wherever it's installed, not just a checkout).
const MIGRATIONS: &[(&str, &str)] =
    &[("0001_initial", include_str!("migrations/0001_initial.sql"))];

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
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
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

fn migrate(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT PRIMARY KEY,
            applied_ns INTEGER NOT NULL
        );",
    )?;
    for (version, sql) in MIGRATIONS {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )?;
        if already_applied {
            continue;
        }
        conn.execute_batch(sql)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_ns) VALUES (?1, ?2)",
            (version, neuroos_common::now_ns()),
        )?;
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

/// FR-STO-12/FR-PRV-03 ("forget"): deletes every row tied to `app_id`
/// (entities, their chunk metadata, focus history, event counters) and
/// returns the deleted entities' ids, so the caller can also purge the
/// matching LanceDB rows (`entity_id IN (...)`).
pub fn forget_by_app(conn: &Connection, app_id: &str) -> Result<Vec<i64>, StorageError> {
    let entity_ids = collect_ids(conn, "SELECT id FROM entities WHERE label = ?1", [app_id])?;
    conn.execute(
        "DELETE FROM chunks_meta WHERE entity_id IN (SELECT id FROM entities WHERE label = ?1)",
        [app_id],
    )?;
    conn.execute("DELETE FROM entities WHERE label = ?1", [app_id])?;
    conn.execute("DELETE FROM focus_history WHERE app_id = ?1", [app_id])?;
    conn.execute("DELETE FROM event_counters WHERE key = ?1", [app_id])?;
    Ok(entity_ids)
}

/// Same, but for everything at or after `since_ns` (an absolute UTC-ns
/// cutoff — `neuroosctl forget --since 1h` becomes `since_ns = now - 1h`
/// at the call site, not in here, so this stays a pure "delete after X"
/// primitive).
pub fn forget_since(conn: &Connection, since_ns: u64) -> Result<Vec<i64>, StorageError> {
    let since = since_ns as i64;
    let entity_ids = collect_ids(
        conn,
        "SELECT id FROM entities WHERE created_ns >= ?1",
        [since],
    )?;
    conn.execute(
        "DELETE FROM chunks_meta WHERE entity_id IN (SELECT id FROM entities WHERE created_ns >= ?1)",
        [since],
    )?;
    conn.execute("DELETE FROM entities WHERE created_ns >= ?1", [since])?;
    conn.execute("DELETE FROM focus_history WHERE t_start_ns >= ?1", [since])?;
    conn.execute("DELETE FROM event_counters WHERE first_ns >= ?1", [since])?;
    Ok(entity_ids)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn open_applies_migrations_and_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 1);
        // re-running migrate() on the same connection must not error or
        // re-apply (CREATE TABLE would fail the second time if it did).
        migrate(&conn).unwrap();
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
}
