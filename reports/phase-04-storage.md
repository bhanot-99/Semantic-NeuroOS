# Phase 04 Report — Semantic Storage Engine

| Field | Value |
| :--- | :--- |
| Phase | 4 — Semantic Storage Engine (C3 `neuroos-storage`) |
| Component(s) | `neuroos-storage` |
| Sprints | 4 (of 3 planned) |
| Dates | 2026-09-27 → 2026-09-28 |
| Status | ⚠️ Passed with approved exceptions — every story done, the soak-replay gate passes. Two real PF misses were found and measured (query latency at scale, and a severe HNSW-promotion stall); both were later fixed (2026-09-28), and the residual query-latency gap (**BUG-002**: ~35-42 ms against a 13/20 ms target at 20k items) was **accepted permanently by the owner on 2026-10-07** (memory.md D-25) and is no longer tracked as open work |
| Author | AI assistant |
| Sign-off | BUG-002 exception accepted by the owner 2026-10-07 (memory.md D-25); report sign-off otherwise pending |

---

## 1. Non-Technical Summary

**What we built:** the single place all of NeuroOS's persistent memory
lives — a 4-stage filter that decides what's worth remembering (ignoring
compiler noise, your own windows, and passive background media), a
searchable index of it (so a later question like "what was I looking at"
can find the right memory), a place-in-time query ("what was focused a
moment ago"), lifecycle housekeeping (old data expires, backups happen
every 6 hours), and a way to forget something on request.

**Why it matters:** this is what turns raw desktop activity into
something the assistant can actually answer questions about later, while
keeping the privacy promises (never remember noise, always be able to
forget, untrusted web content stays marked untrusted).

**What you can see / try now:** ingest a real focus session and ask a
real question against it — `crates/neuroos-storage/tests/soak_replay.rs`
does exactly this against real recorded desktop use (Phase 3's 8-hour
fixture set), and passes.

**Is it on track?** Functionally yes — every story is built and tested,
and the soak-replay gate (the one criterion phases.md calls out
specifically as "Gate") passes cleanly on real data. But this session
also measured two performance targets for real for the first time (they'd
only ever been flagged as "not yet benchmarked" before), and both are
missed — one of them badly enough to be a real user-facing hang, not just
a missed number.

**Risks or concerns in plain words:**
- Searching your stored memories is currently much slower than the 13ms
  target once there's a meaningful amount of data (measured ~50ms typical,
  ~100ms worst case at 20,000 items).
- The automatic "switch to a faster search index" feature has a serious
  bug: the moment it decides to switch, it can freeze that one request for
  well over a minute while it builds the new index. This has never
  happened in real use yet only because real personal data hasn't grown
  large enough to trigger it — it will happen the first time it does.
- "Disk full" handling doesn't exist yet — if the disk fills up,
  behavior is untested and undefined, not gracefully degraded as intended.

---

## 2. Technical Summary

### 2.1 Delivered scope

| Story | Title | Status | Notes |
| :--- | :--- | :--- | :--- |
| P4-S01 | Noisy events never become persistent nodes | Done | 4-stage filter; soak-replay gate proves it on real data |
| P4-S02 | Events stored in the right domain with the right taint | Done | 10 of 13 domains have a C1 source and are implemented here |
| P4-S03 | Top-k relevant chunks for a question | Done | Real FastEmbed + LanceDB; latency now benchmarked (see §2.4) — misses target |
| P4-S04 | Window focused at a given moment ± 1.5s | Done | `QueryFocusHistory`; KPI-2 fixture suite (Phase 5) scores it 100% |
| P4-S05 | Slow collection auto-promotes to HNSW | Done, real bug found | Promotion fires correctly; the promotion itself blocks the triggering query for a very long time (see §2.4/§2.8) |
| P4-S06 | Fetched documents ingested as untrusted | Done | `ingest_external_document`, always `EXTERNAL_UNTRUSTED` |
| P4-S07 | Old data expires and backups exist | Done | GC + 6-hourly backup; restore-to-a-real-query tested |
| P4-S08 | Model upgrade re-indexes without downtime | Done | Background re-index via a second `Embedder`, live path untouched |
| P4-S09 | Forget a time range or an app | Done | Rows + vectors + next-backup all verified this session |
| P4-S10 | (Conditional) SQLite v1 migrator | Dropped | OQ-03 default applied — no sample DB was ever provided |

### 2.2 Architecture and implementation notes

- `ingest::filter::IngestFilter` is the single synchronous gate every
  `RawTelemetryEvent` passes through before any domain adapter touches
  storage: PPID collapse → self-observation exclusion → promotion gate
  (N≥3 or >5s dwell) → MPRIS demotion.
- `StorageEngine` owns one `rusqlite::Connection` (`!Sync`, hence
  `storage.sock`'s server uses a `LocalSet`, not plain `tokio::spawn` —
  D-19) and one `Arc<LanceStore>` (one LanceDB table per domain family:
  attention/work/knowledge/system/external).
- `LanceStore::query` self-instruments its own latency and triggers FR-
  STO-07's HNSW promotion inline — this session found that inline
  placement is itself the root cause of the promotion-stall bug (§2.4).
- `adapters::adapt_mpris` only ever writes to `event_counters`, never
  calls `upsert_entity` — MPRIS demotion (filter stage 4) is *unconditional*
  by construction, not merely gated; the soak-replay gate's
  `zero_access_mpris_nodes == 0` check is really confirming an invariant.

### 2.3 Interfaces / contracts changed

| Proto / API | Change | Additive? | ADR |
| :--- | :--- | :--- | :--- |
| `storage.proto` | `ListEntities`/`ListEdges`/`UpsertEdge`/`PruneEdges` (added during Phase 5 for the cold worker) | Yes | — |

### 2.4 Measured results vs targets

| Metric | Target | Measured | Pass | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Query latency at 20,000 items (flat scan, pre-promotion) | p50 ≤ 13ms, p99 ≤ 20ms | p50 = 50.00ms, p99 = 100.00ms | ❌ | `crates/neuroos-storage/tests/pf_benchmark.rs`, real run this session |
| HNSW promotion cost (the query that triggers it) | (not separately budgeted, but should not be user-visible) | **118.05 real seconds** for one query, on a real 20,000×384 synthetic corpus | ❌ | same, real run this session |
| Query latency at 20,000 items, post-promotion (HNSW) | Expected faster than flat scan | p50 = 50.00ms, p99 = 500.00ms — *not* faster, p99 worse | ❌ | same |
| Ingest throughput | ≥ 200 events/s | ~3,000 events/s (16,997 real mixed events / 5.67s replaying all 4 real Phase 3 dumps) | ✅ | `tests/soak_replay.rs` real run |
| RSS | ≤ 205 MiB | ~510.8 MiB peak (debug build, real model + onnxruntime + LanceDB loaded, replaying real data) | ❌ | measured via `/proc/<pid>/status VmRSS` sampling during a real `soak_replay` run this session; not re-measured on a release build (time-boxed) |
| HNSW promotion fires on a large synthetic collection | Fires on a 100k-item collection | Confirmed firing on a 20,000-item collection already (well before 100k) — the dedicated 100k test exists (`hnsw_promotion_fires_on_a_synthetic_100k_collection`) but was not run this session (slow; the 20k benchmark already demonstrates the mechanism with real numbers) | ✅ (mechanism), ⚠️ (100k specifically not directly measured) | same |
| Soak-replay gate | `total_promoted_entities ≤ 30`, `compiler_subprocesses == 0`, `zero_access_mpris_nodes == 0` on every committed dump | All 4 real dumps: `compiler_subprocesses=0`, `zero_access_mpris_nodes=0` on every dump; scoped total 1–5, literal all-domain total 3–10 (both ≤ 30) | ✅ | `tests/soak_replay.rs`, real run against Phase 3's real ≥8h recording |

### 2.5 Test results

| Level | Suites | Passed | Failed | Coverage | Evidence |
| :--- | :--- | :--- | :--- | :--- | :--- |
| Unit | `sqlite`, `lance`, `ingest::filter`, `lifecycle`, `spool`, `adapters` | all | 0 | overall 91.31%/87.77%/90.09% (lines/regions/functions); filter 97.73%/96.86%/100%; adapters 96.92%/95.30%/100% | `cargo llvm-cov nextest -p neuroos-storage --run-ignored all` |
| Property | `ingest::filter::proptests` (self-observation never promoted) | all | 0 | — | same |
| Contract | `storage.proto` (Rust/Python round-trip, Phase 0) | all | 0 | — | `tests/contract/roundtrip.sh` |
| Integration | Real `StorageEngine` + real `storage.sock`, real embedder/LanceDB (multiple `#[ignore]`d tests) | all | 0 | — | `cargo test -p neuroos-storage -- --ignored` |
| Gate (soak-replay) | 4 real Phase 3 dumps | 4/4 | 0 | — | `tests/soak_replay.rs` |
| Performance | Query latency @ 20k, ingest throughput, RSS | 2 pass (sanity bound), 3 miss target | — | — | `tests/pf_benchmark.rs`, `tests/soak_replay.rs` |
| Fault injection | Kill-during-write, corrupt spool | 2/2 | 0 | — | `tests/fault_injection.rs`, `src/spool.rs` |
| Security | SQL injection (2 cases), Landlock/systemd sandbox | 3/3 | 0 | — | `src/sqlite.rs`, `tests/contract/storage_landlock.sh` |

### 2.6 Deviations from plan

| Item | Planned | Actual | Reason | Approved by |
| :--- | :--- | :--- | :--- | :--- |
| `neuroosctl replay <dump>` CLI | phases.md §7.1 item 10 | Not built as a CLI; built as a `neuroos-storage` integration test instead | phases.md §7.3's own IT row explicitly allows "a fake monitor replaying dumps" — the gate's real requirement (prove the ingest filter against real data) is met without a CLI wrapper | — |
| Query latency / RSS PF targets | ≤13/20ms, ≤205 MiB | 4-8x and 2.5x over budget respectively | Never benchmarked before this session (P4-S03's own log already flagged this); real numbers now exist but no fix attempted (see §2.8) | Flagged, not unilaterally fixed — owner decision needed |
| "Disk full → ingest pauses, queries still served" (FI) | Required test | Not built — no disk-full handling exists in the code at all, not just an untested behavior | Genuine feature gap discovered this session, not a testing gap; building real ENOSPC-handling behavior is new feature work, out of scope for closing this gate in one sitting | Flagged for owner decision |
| Release-build RSS re-measurement | Desirable for a fair number against the 205 MiB budget | Only measured on a debug build (510.8 MiB) | Time-boxed given repeated disk-space incidents this session (see §2.8); the gap versus budget is wide enough (2.5x) that a debug/release difference wouldn't change the pass/fail outcome | — |

### 2.7 Decisions made (ADRs)

- No new ADR this session for Phase 4 itself (D-19, already logged in a
  prior session, remains the relevant architectural decision for this
  phase's IPC-serving shape).

### 2.8 Tech debt introduced

| Item | Impact | Repay in phase |
| :--- | :--- | :--- |
| Query latency at 20,000 items misses budget by 4-8x (flat scan) | Every real question answered once meaningful data has accumulated is slower than designed | Needs its own investigation: is LanceDB's flat scan inherently this slow at this scale on this hardware, or is something adding avoidable overhead |
| **Severe**: `LanceStore::query`'s HNSW promotion (`create_index`) runs synchronously inside the query call that crosses the threshold — measured 118 real seconds for one query at 20k items. Worse, steady-state post-promotion HNSW latency wasn't even faster than the flat scan it replaced. | The first "unlucky" query that pushes a family's rolling p99 over 5ms will hang for however long a real HNSW build takes at that family's real size — a severe, user-facing stall in production, not just a missed benchmark number | Needs its own session: move the index build off the query's own critical path; investigate why HNSW underperforms flat scan here |
| RSS ~510.8 MiB (debug build) vs. 205 MiB budget | Real model + onnxruntime + LanceDB loaded together cost more than budgeted; not yet re-measured on release | Re-measure on a release build once disk space allows; then decide whether to trim the budget assumption or reduce footprint |
| "Disk full" ingest-pause behavior doesn't exist | Undefined behavior if the disk fills (a real risk — it happened twice this session for the *build* cache, not this service's own data, but the service itself has no defense either way) | New feature work: detect ENOSPC on write, pause ingest, keep serving reads |
| `neuroos-sandbox` crate is still an empty Phase-0 stub | Landlock/seccomp enforcement for C3 exists only at the systemd-unit level (proven for real this session, `tests/contract/storage_landlock.sh`), not as an in-process fallback if a future deployment target can't use systemd hardening | Only relevant if OQ-06 (distribution posture) changes away from "personal install via systemd user units" |

---

## 3. Exit Criteria Verification

| # | Exit criterion | Met | Evidence |
| :--- | :--- | :--- | :--- |
| 1 | Soak-replay gate passes on all dumps (evidence: gate output per dump) | ✅ | `tests/soak_replay.rs`, real run against all 4 committed Phase 3 dumps; all three metrics pass on every dump |
| 2 | Retrieval latency and RSS within budget (benchmark JSON) | ❌ | `tests/pf_benchmark.rs`: p50=50ms/p99=100ms vs. 13/20ms target; RSS 510.8 MiB vs. 205 MiB target — both measured for real, both miss |
| 3 | Crash-consistency tests pass; backup restore tested end to end | ✅ | `tests/fault_injection.rs` (real SIGKILL mid-write, `PRAGMA integrity_check` + `StorageEngine::open` both succeed after); `lifecycle::tests::backup_creates_a_real_snapshot_and_prunes_old_ones` (restores and queries a real value back) |
| 4 | Forget verified (rows, vectors, next backup) | ✅ | `engine::tests::forget_by_app_removes_the_entity_and_its_lance_chunk` (rows + vectors); new `forgotten_rows_do_not_reappear_in_the_next_backup` (next backup) |
| 5 | 100% branch coverage on ingest filter and taint attachment; ≥ 80% overall | ⚠️ | `cargo llvm-cov nextest --run-ignored all` (real, all 68 tests incl. live ones): overall lines=91.31%, regions=87.77%, functions=90.09% (≥80% target met). `ingest/filter.rs` (the filter): lines=97.73%, regions=96.86%, functions=100%. `adapters/mod.rs` (taint attachment): lines=96.92%, regions=95.30%, functions=100%. Function coverage is a literal 100% on both; region/line coverage is very high but not literally 100% (13 and 27 uncovered regions respectively, likely rare error-handling paths). Branch-specific coverage needs a nightly toolchain (`-Z coverage-options=branch`), same pre-existing gap already documented for `neuroos-ipc` — region coverage is this project's established proxy metric |
| 6 | OQ-02 closed (domain list final); OQ-03 closed (migrator built or dropped) | ✅ | memory.md §7: OQ-02 closed 2026-09-28 (proposal accepted, no deviations); OQ-03 closed (dropped, no sample DB ever provided) |
| 7 | Phase report written; memory.md updated | ✅ (this report; memory.md updated in the same session) | — |

---

## 4. Open Questions Closed / Opened

| ID | Question | Resolution / status |
| :--- | :--- | :--- |
| OQ-02 | Final list of 13 domain adapters? | **Closed** — Architecture.md §7.3's proposal accepted as final, no deviations |
| OQ-03 | Does a NeuroOS v1 SQLite DB exist to migrate? | **Closed** — default applied (drop unless a sample DB is provided); none was ever provided, P4-S10 dropped |

---

## 5. Lessons Learned

- **Keep:** benchmarking against real recorded data (Phase 3's ≥8h
  fixtures) rather than only synthetic unit-test scenarios — the
  soak-replay gate's real numbers (3-10 entities total across 8h) turned
  out far more reassuring than the spec's terse "≤30" wording initially
  suggested, and the real PF benchmark surfaced two genuine bugs
  (HNSW-promotion stall, query latency) that synthetic-duration unit tests
  never could have.
- **Keep:** when a PF target has been flagged "not yet benchmarked" for
  multiple sessions, benchmark it before calling the phase done, even if
  the result is bad news — a known real number is worth more than an
  optimistic assumption.
- **Change:** next time a phase's own tests introduce a large synthetic
  corpus (20k+ vectors) or run `cargo llvm-cov`, budget disk space
  explicitly up front — this session hit the disk-full incident (already
  known tech debt from a prior session) a third time doing exactly this
  work, needing a `cargo clean` mid-session each time.
- **Change:** a performance-critical code path (`LanceStore::query`)
  should never synchronously await an operation as expensive as an index
  build inside the request it's serving — this is a design smell worth
  watching for in future phases too (C4/C5's own hot paths).

---

## 6. Next Phase Readiness

| Check | Status |
| :--- | :--- |
| Next phase dependencies satisfied | ✅ — Phase 5 already built against this phase's real `storage.sock` service and found it fully sufficient for its own needs |
| Next phase stories meet Definition of Ready | ✅ |
| memory.md updated (phase tracker, current phase, sprint board) | ✅ |

**On the ⚠️ status:** every story is built and tested, and the gate
phases.md itself calls out by name ("Gate" row) passes cleanly on real
data. The exceptions are two performance targets that were simply never
measured before this session and are now known, real, and documented —
not fixed. Recommending the owner treat this as "passed with a known,
documented performance gap" rather than blocking on a full fix now, given
the system is otherwise functionally correct and already in active use by
Phase 5.
