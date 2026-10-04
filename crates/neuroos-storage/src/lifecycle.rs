//! Lifecycle jobs (Architecture.md §7.5, P4-S07): retention-based GC and
//! periodic backups. Both are plain functions the caller schedules (daily
//! GC at 03:30 local, backup every 6h per the spec) — no timer/scheduler
//! lives in this crate yet, since `main.rs` (not written this phase) is
//! where that wiring belongs.
use std::path::Path;

use rusqlite::Connection;

use crate::adapters::{
    DOMAIN_APP_LIFECYCLE, DOMAIN_BUILD_JOB, DOMAIN_CALENDAR, DOMAIN_EXTERNAL_DOCUMENTS,
    DOMAIN_GIT_ACTIVITY, DOMAIN_IDLE_PRESENCE, DOMAIN_MEDIA_PLAYBACK, DOMAIN_NOTES,
    DOMAIN_PROCESS_ACTIVITY, DOMAIN_SYSTEM_RESOURCE, DOMAIN_WINDOW_FOCUS,
};
use crate::sqlite::StorageError;

/// Architecture.md §7.3's per-domain retention. `None` = permanent (never
/// GC'd by age — notes/git/calendar are meant to last).
pub fn retention_days_for_domain(domain: &str) -> Option<u32> {
    match domain {
        DOMAIN_GIT_ACTIVITY | DOMAIN_NOTES | DOMAIN_CALENDAR => None,
        DOMAIN_BUILD_JOB | DOMAIN_EXTERNAL_DOCUMENTS => Some(90),
        DOMAIN_WINDOW_FOCUS
        | DOMAIN_APP_LIFECYCLE
        | DOMAIN_IDLE_PRESENCE
        | DOMAIN_PROCESS_ACTIVITY
        | DOMAIN_MEDIA_PLAYBACK
        | DOMAIN_SYSTEM_RESOURCE => Some(14),
        _ => Some(14), // conservative default for any domain not listed above
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcSummary {
    pub entities_deleted: usize,
    pub focus_history_deleted: usize,
}

/// What one GC run removed from SQLite, and what the caller must still
/// remove from LanceDB: rows of `entity_ids`, and for each
/// `(domain, cutoff_ns)` every chunk of that domain older than the cutoff.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcOutcome {
    pub summary: GcSummary,
    pub entity_ids: Vec<i64>,
    pub chunk_cutoffs: Vec<(&'static str, u64)>,
}

/// Retention is per row (Architecture.md §7.2/§7.3), not per entity:
/// for each domain with a finite retention, chunks and event counters
/// older than the cutoff expire, and an entity expires only once it has
/// not been *seen* within the retention window (a long-lived app that is
/// still in use keeps its entity and its recent history). Raw
/// `focus_history` keeps 14 days. Everything happens in one transaction.
pub fn gc_expired_entities(conn: &Connection, now_ns: u64) -> Result<GcOutcome, StorageError> {
    const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;
    let mut outcome = GcOutcome::default();
    let tx = conn.unchecked_transaction()?;

    let domains = [
        DOMAIN_WINDOW_FOCUS,
        DOMAIN_APP_LIFECYCLE,
        DOMAIN_IDLE_PRESENCE,
        DOMAIN_PROCESS_ACTIVITY,
        DOMAIN_BUILD_JOB,
        DOMAIN_MEDIA_PLAYBACK,
        DOMAIN_SYSTEM_RESOURCE,
        DOMAIN_EXTERNAL_DOCUMENTS,
    ];
    for domain in domains {
        let Some(days) = retention_days_for_domain(domain) else {
            continue; // permanent
        };
        let cutoff_ns = now_ns.saturating_sub(days as u64 * NS_PER_DAY);
        let cutoff = cutoff_ns as i64;
        let mut stmt =
            tx.prepare("SELECT id FROM entities WHERE domain = ?1 AND last_seen_ns < ?2")?;
        let ids: Vec<i64> = stmt
            .query_map((domain, cutoff), |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        crate::sqlite::delete_entities(&tx, &ids)?;
        tx.execute(
            "DELETE FROM chunks_fts WHERE domain = ?1 AND t_ns < ?2",
            (domain, cutoff),
        )?;
        tx.execute(
            "DELETE FROM event_counters WHERE domain = ?1 AND last_ns < ?2",
            (domain, cutoff),
        )?;
        outcome.summary.entities_deleted += ids.len();
        outcome.entity_ids.extend(ids);
        outcome.chunk_cutoffs.push((domain, cutoff_ns));
    }

    let focus_cutoff = now_ns.saturating_sub(14 * NS_PER_DAY) as i64;
    outcome.summary.focus_history_deleted = tx.execute(
        "DELETE FROM focus_history WHERE t_end_ns < ?1",
        [focus_cutoff],
    )?;
    tx.commit()?;

    // GC vacuum (Architecture.md §7.5's "incremental" VACUUM step; the
    // file is in incremental auto-vacuum mode, see `sqlite::open`).
    conn.execute_batch("PRAGMA incremental_vacuum;")?;

    Ok(outcome)
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error(transparent)]
    Sqlite(#[from] StorageError),
    #[error(transparent)]
    Rusqlite(#[from] rusqlite::Error),
    #[error("backup I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Architecture.md §7.5: `wal_checkpoint(TRUNCATE)` → `VACUUM INTO` a
/// snapshot, plus a recursive copy of the LanceDB directory (Lance's
/// on-disk format is append-only/versioned — existing files are never
/// mutated in place, so copying the live directory is safe) — both into
/// `backups_root/<label>/`, then prunes to the newest `keep` backups.
/// `label` is the caller's timestamp string (kept as a parameter so tests
/// don't depend on wall-clock formatting).
pub async fn backup(
    conn: &Connection,
    lance_dir: &Path,
    backups_root: &Path,
    label: &str,
    keep: usize,
) -> Result<std::path::PathBuf, BackupError> {
    let dest = backups_root.join(label);
    std::fs::create_dir_all(&dest).map_err(|source| BackupError::Io {
        path: dest.clone(),
        source,
    })?;

    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let sqlite_backup_path = dest.join("meta.sqlite3");
    conn.execute(
        "VACUUM INTO ?1",
        [sqlite_backup_path.to_string_lossy().as_ref()],
    )?;

    let lance_backup_path = dest.join("lancedb");
    copy_dir_recursive(lance_dir, &lance_backup_path)?;

    prune_old_backups(backups_root, keep)?;
    Ok(dest)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<(), BackupError> {
    std::fs::create_dir_all(to).map_err(|source| BackupError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    if !from.exists() {
        return Ok(()); // nothing written to LanceDB yet
    }
    for entry in std::fs::read_dir(from).map_err(|source| BackupError::Io {
        path: from.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BackupError::Io {
            path: from.to_path_buf(),
            source,
        })?;
        let dest_path = to.join(entry.file_name());
        let file_type = entry.file_type().map_err(|source| BackupError::Io {
            path: entry.path(),
            source,
        })?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path).map_err(|source| BackupError::Io {
                path: entry.path(),
                source,
            })?;
        }
    }
    Ok(())
}

/// Keeps only the `keep` most-recently-named backup directories (label
/// strings sort chronologically when they're UTC timestamps, per
/// Architecture.md §7.1's `backups/<UTC timestamp>/`).
fn prune_old_backups(backups_root: &Path, keep: usize) -> Result<(), BackupError> {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(backups_root)
        .map_err(|source| BackupError::Io {
            path: backups_root.to_path_buf(),
            source,
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    if entries.len() > keep {
        for old in &entries[..entries.len() - keep] {
            std::fs::remove_dir_all(old).map_err(|source| BackupError::Io {
                path: old.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use neuroos_taint::TaintFlags;

    #[test]
    fn retention_matches_the_architecture_table() {
        assert_eq!(retention_days_for_domain(DOMAIN_NOTES), None);
        assert_eq!(retention_days_for_domain(DOMAIN_GIT_ACTIVITY), None);
        assert_eq!(retention_days_for_domain(DOMAIN_WINDOW_FOCUS), Some(14));
        assert_eq!(retention_days_for_domain(DOMAIN_BUILD_JOB), Some(90));
    }

    #[test]
    fn gc_deletes_only_expired_non_permanent_entities() {
        let conn = crate::sqlite::open_in_memory().unwrap();
        const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;
        let now = 100 * NS_PER_DAY;

        // window_focus (14d retention): one old (20d ago), one fresh (1d ago)
        crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "old-app",
            TaintFlags::empty(),
            now - 20 * NS_PER_DAY,
            false,
        )
        .unwrap();
        crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "fresh-app",
            TaintFlags::empty(),
            now - NS_PER_DAY,
            false,
        )
        .unwrap();
        // notes (permanent): old, must survive regardless of age
        crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_NOTES,
            "file",
            "/notes/old.md",
            TaintFlags::empty(),
            now - 90 * NS_PER_DAY,
            true,
        )
        .unwrap();

        let outcome = gc_expired_entities(&conn, now).unwrap();
        assert_eq!(outcome.summary.entities_deleted, 1);
        assert_eq!(outcome.entity_ids.len(), 1);

        let remaining_labels: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT label FROM entities ORDER BY label")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(remaining_labels, vec!["/notes/old.md", "fresh-app"]);
    }

    #[test]
    fn gc_deletes_expired_focus_history_rows() {
        let conn = crate::sqlite::open_in_memory().unwrap();
        const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;
        let now = 100 * NS_PER_DAY;
        crate::sqlite::insert_focus_history(
            &conn,
            &crate::sqlite::FocusHistoryEntry {
                app_id: "old",
                title: "",
                pid: 0,
                root_pid: 0,
                t_start_ns: now - 20 * NS_PER_DAY,
                t_end_ns: now - 20 * NS_PER_DAY + 1000,
                dwell_ms: 1,
            },
        )
        .unwrap();
        crate::sqlite::insert_focus_history(
            &conn,
            &crate::sqlite::FocusHistoryEntry {
                app_id: "fresh",
                title: "",
                pid: 0,
                root_pid: 0,
                t_start_ns: now - NS_PER_DAY,
                t_end_ns: now - NS_PER_DAY + 1000,
                dwell_ms: 1,
            },
        )
        .unwrap();

        let outcome = gc_expired_entities(&conn, now).unwrap();
        assert_eq!(outcome.summary.focus_history_deleted, 1);
        let remaining: String = conn
            .query_row("SELECT app_id FROM focus_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, "fresh");
    }

    const DAY: u64 = 86_400 * 1_000_000_000;

    fn fts_at(conn: &rusqlite::Connection, id: &str, entity_id: i64, t_ns: u64) {
        crate::sqlite::insert_chunk_fts(
            conn,
            &crate::sqlite::FtsChunk {
                chunk_id: id,
                entity_id,
                domain: DOMAIN_WINDOW_FOCUS,
                taint: 0,
                t_ns,
                text: id,
            },
        )
        .unwrap();
    }

    /// H1: GC must not fail on the edges' foreign key.
    #[test]
    fn gc_succeeds_when_edges_reference_an_expired_entity() {
        let conn = crate::sqlite::open_in_memory().unwrap();
        let now = 100 * DAY;
        let old = crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "old-app",
            TaintFlags::empty(),
            now - 20 * DAY,
            false,
        )
        .unwrap();
        let fresh = crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "fresh-app",
            TaintFlags::empty(),
            now - DAY,
            false,
        )
        .unwrap();
        crate::sqlite::upsert_edge(
            &conn,
            &crate::sqlite::EdgeRow {
                src: old,
                dst: fresh,
                kind: "co_occurs".into(),
                weight: 1.0,
                reinforced_ns: now,
                hypothesis: true,
            },
        )
        .unwrap();

        let outcome = gc_expired_entities(&conn, now).unwrap();

        assert_eq!(outcome.entity_ids, vec![old]);
        let edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, 0);
    }

    /// H4: retention is per row, not per entity. An app first seen 20 days
    /// ago and still used yesterday keeps its entity and yesterday's chunk;
    /// only the 20-day-old chunk expires.
    #[test]
    fn gc_expires_old_rows_of_a_long_lived_entity_but_keeps_recent_ones() {
        let conn = crate::sqlite::open_in_memory().unwrap();
        let now = 100 * DAY;
        let app = crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "browser",
            TaintFlags::empty(),
            now - 20 * DAY,
            false,
        )
        .unwrap();
        // seen again yesterday
        crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_WINDOW_FOCUS,
            "window",
            "browser",
            TaintFlags::empty(),
            now - DAY,
            false,
        )
        .unwrap();
        fts_at(&conn, "old-chunk", app, now - 20 * DAY);
        fts_at(&conn, "fresh-chunk", app, now - DAY);

        let outcome = gc_expired_entities(&conn, now).unwrap();

        assert!(outcome.entity_ids.is_empty());
        assert!(
            outcome
                .chunk_cutoffs
                .contains(&(DOMAIN_WINDOW_FOCUS, now - 14 * DAY)),
            "the LanceDB side gets the same per-domain cutoff"
        );
        let chunks: Vec<String> = {
            let mut stmt = conn.prepare("SELECT chunk_id FROM chunks_fts").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(chunks, vec!["fresh-chunk".to_string()]);
    }

    #[tokio::test]
    async fn backup_creates_a_real_snapshot_and_prunes_old_ones() {
        let sqlite_dir = tempfile::tempdir().unwrap();
        let lance_dir = tempfile::tempdir().unwrap();
        let backups_root = tempfile::tempdir().unwrap();

        let conn = crate::sqlite::open(&sqlite_dir.path().join("meta.sqlite3")).unwrap();
        crate::sqlite::upsert_entity(
            &conn,
            DOMAIN_NOTES,
            "file",
            "/n",
            TaintFlags::empty(),
            1,
            true,
        )
        .unwrap();
        std::fs::write(lance_dir.path().join("marker.lance"), b"fake lance data").unwrap();

        for i in 0..3 {
            let label = format!("2026-01-0{}", i + 1);
            backup(&conn, lance_dir.path(), backups_root.path(), &label, 2)
                .await
                .unwrap();
        }

        let mut remaining: Vec<String> = std::fs::read_dir(backups_root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec!["2026-01-02", "2026-01-03"],
            "keep=2 should prune the oldest"
        );

        let latest = backups_root.path().join("2026-01-03");
        assert!(latest.join("meta.sqlite3").exists());
        assert!(latest.join("lancedb/marker.lance").exists());

        // the backup SQLite file is a real, independently-openable database
        let restored = crate::sqlite::open(&latest.join("meta.sqlite3")).unwrap();
        let label: String = restored
            .query_row("SELECT label FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label, "/n");
    }
}
