//! BUG-006 diagnostic: why do the first ~21 `query_hybrid` calls after an
//! ingest burst miss C5's 100 ms deadline, while every later call makes it?
//!
//! The KPI-1 harness (`neuroos-knowledge-query/tests/kpi1_eval.rs`) ingests
//! this machine's own ~6,100-event raw recording and then asks 59 scripted
//! questions. 21 of them fail with `storage.sock request failed: read
//! deadline exceeded`, contiguously from the very first question, and then
//! the remaining 36 succeed. Lifting the deadline makes all 59 pass, so the
//! failure is pure latency -- never a hang, an error or a wrong answer.
//!
//! BUG-006 was originally closed against a different root cause (unbounded
//! concurrent distillation starving the CPU, capped by
//! `MAX_CONCURRENT_DISTILLATIONS`). That explanation does not fit a failure
//! window that is *worst at the very first query* -- D-19's own note records
//! that "the very first call alone reliably succeeds" -- so this measures
//! C3 in isolation instead: no socket, no C4, no distillation, nothing but
//! ingest followed by repeated `query_hybrid`.
//!
//! **Root cause found (2026-10-07):** HNSW promotion could not fire until a
//! family had logged `MIN_SAMPLES_BEFORE_PROMOTION_CHECK` (20) query
//! samples, but flat scan over this corpus costs ~150 ms -- so every query
//! before the floor was reached necessarily missed the 100 ms deadline, and
//! the 21st finally promoted. Two independent checks confirmed it: dropping
//! the floor to 2 moved the recovery point from query 24 to query 6, and
//! letting the store settle for 45 s before querying changed nothing at all
//! (so it was never a warm-up or a compaction backlog). The fix lets a
//! single sample past the deadline promote on its own.
//!
//! This started as a measurement and is now the regression guard: it prints
//! the per-query latency curve, then asserts the misses stay within the
//! small budget the promotion mechanism inherently costs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neuroos_monitor::dump::DumpReader;
use neuroos_storage::engine::StorageEngine;
use neuroos_storage::test_support::{dev_models_dir, dev_onnxruntime_dylib};

/// rules.md §5.7 / `STORAGE_QUERY_DEADLINE` in C5a: the deadline a C3 query
/// has to meet. Duplicated as a literal rather than imported, because C3
/// must not depend on C5a (AB-1).
const C3_QUERY_DEADLINE: Duration = Duration::from_millis(100);

/// How many queries to run after the burst. The original failure window was
/// 21 of 59 questions, so this has to be comfortably past it.
const QUERIES: usize = 45;

/// How many of those may miss the deadline after the fix.
///
/// Promotion is driven by a *measured* p99 (FR-STO-07), so one query has to
/// be slow for C3 to learn anything at all, and the detached `create_index`
/// it fires then competes for CPU with the next few. Measured on this
/// machine: 5 misses (query 1 triggers, 2-5 overlap the ~1 s build, 6+ run
/// at ~55 ms). The budget allows headroom for a slower machine without
/// letting the old 24-miss behaviour back in.
const MAX_QUERIES_OVER_DEADLINE: usize = 8;

/// Per-family data-file counts under a LanceDB store directory. Fragment
/// *file* count (not row count) is what drives LanceDB scan cost, and is
/// what BUG-007(c)'s background compaction exists to keep down -- so it is
/// the first thing to look at when scan latency decays over time.
fn fragments(lance_dir: &std::path::Path) -> String {
    let mut out: Vec<String> = std::fs::read_dir(lance_dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    let path = e.path();
                    let name = path.file_name()?.to_string_lossy().to_string();
                    let files = std::fs::read_dir(path.join("data"))
                        .map(|d| d.filter_map(Result::ok).count())
                        .unwrap_or(0);
                    (files > 0).then(|| format!("{name}={files}"))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    if out.is_empty() {
        "none".to_owned()
    } else {
        out.join(" ")
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn raw_dump_dir() -> PathBuf {
    repo_root().join(".dev-cache/telemetry-raw")
}

/// The same question texts KPI-1 asks, so the measured queries are real
/// retrieval work (varied embeddings, varied FTS terms) rather than one
/// query repeated into a warm cache.
fn query_texts() -> Vec<String> {
    let path = raw_dump_dir().join("questions.txt");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split("##").next().unwrap_or(l).trim().to_owned())
        .collect()
}

/// BUG-006, measured in isolation. `#[ignore]`d and machine-local by
/// nature: it needs the real embedding model and this machine's own raw
/// recording, which by design is never committed.
#[tokio::test]
#[ignore = "needs the real embedder model and this machine's own .dev-cache/telemetry-raw/*.bin; see module docs"]
async fn only_the_promotion_trigger_and_its_index_build_miss_the_deadline() {
    let dump_dir = raw_dump_dir();
    let mut dumps: Vec<PathBuf> = std::fs::read_dir(&dump_dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "bin"))
                .collect()
        })
        .unwrap_or_default();
    dumps.sort();
    let texts = query_texts();
    if dumps.is_empty() || texts.is_empty() {
        eprintln!(
            "skipping: need {}/*.bin and questions.txt (machine-local by nature)",
            dump_dir.display()
        );
        return;
    }

    let sqlite_dir = tempfile::tempdir().unwrap();
    let lance_dir = tempfile::tempdir().unwrap();
    let mut engine = StorageEngine::open(
        &sqlite_dir.path().join("meta.sqlite3"),
        lance_dir.path(),
        &dev_models_dir(),
        &dev_onnxruntime_dylib(),
    )
    .await
    .expect("real embedder model + onnxruntime should load");

    // --- the burst, exactly as KPI-1 does it -------------------------------
    let ingest_start = Instant::now();
    let mut events = 0u64;
    for dump in &dumps {
        let mut reader = DumpReader::open(dump).await.unwrap();
        while let Some(event) = reader.read_event().await.unwrap() {
            let _ = engine.ingest(&event).await;
            events += 1;
        }
    }
    eprintln!(
        "ingested {events} real events from {} dump(s) in {:?}",
        dumps.len(),
        ingest_start.elapsed()
    );
    eprintln!(
        "fragments right after the burst: {}",
        fragments(lance_dir.path())
    );

    // Does the latency decay because background compaction is still draining?
    // NEUROOS_BUG006_SETTLE_SECS=N waits N seconds before querying, so the
    // same curve can be measured against a fully settled store.
    if let Some(secs) = std::env::var("NEUROOS_BUG006_SETTLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        eprintln!("settling for {secs}s before querying...");
        tokio::time::sleep(Duration::from_secs(secs)).await;
        eprintln!(
            "fragments after settling:       {}",
            fragments(lance_dir.path())
        );
    }
    eprintln!();

    // --- the measurement ---------------------------------------------------
    let mut latencies = Vec::with_capacity(QUERIES);
    for i in 0..QUERIES {
        let text = &texts[i % texts.len()];
        let start = Instant::now();
        let hits = engine.query_hybrid(text, 6).await.unwrap_or_default();
        let elapsed = start.elapsed();
        latencies.push(elapsed);
        eprintln!(
            "[{:>2}] {:>8.1} ms  {}  hits={:<2} {}",
            i + 1,
            elapsed.as_secs_f64() * 1000.0,
            if elapsed > C3_QUERY_DEADLINE {
                "OVER "
            } else {
                "under"
            },
            hits.len(),
            text.chars().take(58).collect::<String>(),
        );
    }

    let over: Vec<usize> = latencies
        .iter()
        .enumerate()
        .filter(|(_, d)| **d > C3_QUERY_DEADLINE)
        .map(|(i, _)| i + 1)
        .collect();
    let first_half = &latencies[..QUERIES / 2];
    let second_half = &latencies[QUERIES / 2..];
    let mean = |ds: &[Duration]| {
        ds.iter().map(Duration::as_secs_f64).sum::<f64>() / ds.len() as f64 * 1000.0
    };
    eprintln!(
        "\n{} / {QUERIES} queries over the {:?} deadline: {:?}\nmean first half {:.1} ms, mean second half {:.1} ms",
        over.len(),
        C3_QUERY_DEADLINE,
        over,
        mean(first_half),
        mean(second_half),
    );

    assert!(
        over.len() <= MAX_QUERIES_OVER_DEADLINE,
        "BUG-006 regression: {} of {QUERIES} queries missed the {C3_QUERY_DEADLINE:?} deadline \
         (budget {MAX_QUERIES_OVER_DEADLINE}). Over: {over:?}. Before the fix this was 24, all \
         contiguous from the first query, because HNSW promotion waited for \
         MIN_SAMPLES_BEFORE_PROMOTION_CHECK samples while flat scan cost ~150 ms. If this count \
         has grown again, check whether promotion still fires on the first over-deadline query.",
        over.len(),
    );
    // The misses must also be *front-loaded*: the cost belongs to the
    // promotion trigger and its index build, so once those are done latency
    // has to drop and stay down. This is a relative check on purpose --
    // absolute per-query budgets are not robust when the whole live suite is
    // sharing the machine (C14), and a stray outlier mid-run under that load
    // is not a regression. The before/after gap was 179 ms vs 68 ms, so a
    // real regression (promotion never firing) shows up here as the two
    // halves converging, not as one slow query.
    assert!(
        mean(second_half) * 1.5 < mean(first_half),
        "BUG-006 regression: latency is no longer front-loaded (first half {:.1} ms, \
         second half {:.1} ms). That shape means promotion is not firing early any more -- \
         before the fix both halves sat high until query 24.",
        mean(first_half),
        mean(second_half),
    );
    eprintln!(
        "\nOK: {} miss(es) of {QUERIES} (budget {MAX_QUERIES_OVER_DEADLINE}), front-loaded as expected",
        over.len()
    );
}
