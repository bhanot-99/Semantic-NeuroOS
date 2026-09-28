# BUGS.md — Bug / Issue / Blocker Registry

This file tracks every real bug, blocker, and unresolved issue discovered
while building and testing NeuroOS. It is the single dedicated place to
look for "what's currently broken and why" — separate from `memory.md`
(overall project state, decisions, sprint tracking) and from individual
`reports/phase-NN-*.md` files (a snapshot at the time each phase's gate
was evaluated).

**Update protocol:**
- Add an entry the moment a real bug, blocker, or gate failure is found
  during testing or execution — not just at phase-end.
- Never delete an entry. When something is fixed, set its Status to
  **Fixed** and add a one-line note with the fix's commit/PR.
- Cross-link to the relevant `memory.md` tech-debt row and phase report
  where applicable, rather than duplicating the full writeup.
- Severity: **Critical** (breaks a core user-facing promise or causes
  data loss/corruption), **High** (real, measured, blocks a phase gate),
  **Medium** (real but narrow blast radius), **Low** (cosmetic/edge case).

---

## Open bugs

| ID | Date found | Phase | Severity | Status | Summary |
| :--- | :--- | :--- | :--- | :--- | :--- |
| BUG-001 | 2026-09-28 | P4 (found in P5) | High | Open | `StorageEngine::query_hybrid`'s `embed()` call is synchronous and starves every connection on `storage.sock`'s shared `LocalSet`, causing real query timeouts under repeated real load |
| BUG-002 | 2026-09-28 | P4 | High | Open | Query latency at 20,000 items is 4-8x over the phases.md budget (flat scan p50=50ms/p99=100ms vs. 13ms/20ms target) |
| BUG-003 | 2026-09-28 | P4 | **Critical** | Open | FR-STO-07's HNSW promotion runs `create_index` synchronously inside the triggering query — measured **118 real seconds** of stall on one query; post-promotion HNSW latency isn't even faster than the flat scan it replaced |
| BUG-004 | 2026-09-28 | P5 (root cause in P2) | **Critical** | Open | C4's interactive lane (`kMaxQueueDepth = 4`) reports `"lane queue is full"` under a merely sequential, spaced-out real workload — a resource-accounting bug, not genuine contention |
| BUG-005 | 2026-09-28 | P5 (root cause in P2) | **Critical** | Open | `InferenceClient::generate` hardcodes greedy decoding (`temperature: 0.0`, no repetition penalty); on real, longer, substantive prompts this reliably degenerates into a repeated-token loop instead of a real answer |

## Open blockers (environment / tooling, not product code)

| ID | Date found | Severity | Status | Summary |
| :--- | :--- | :--- | :--- | :--- |
| BLOCKER-001 | 2026-09-27, recurred 2026-09-28 (3rd time) | Medium | Open | `target/` (Rust build cache) repeatedly grows to 45-65 GB and fills the disk to 100%, most recently compounded by `cargo llvm-cov`'s own separate `target/llvm-cov-target` (+7 GB). No automated guard exists; `cargo clean` is the only fix so far, and it was needed a third time despite being logged as tech debt after the first occurrence |

---

## BUG-001 — `query_hybrid`'s blocking `embed()` starves `storage.sock`

- **Where:** `crates/neuroos-storage/src/engine.rs` (`StorageEngine::query_hybrid`), `crates/neuroos-storage/src/server.rs` (the shared `LocalSet`)
- **What happens:** `query_hybrid` calls `self.embedder.embed(&[text])` synchronously (real ONNX inference, CPU-bound), with no `spawn_blocking`, directly inside `storage.sock`'s async request handler. Because `StorageEngine` is `!Sync` (D-19), every connection is `spawn_local`'d onto one shared `LocalSet` — so one connection's blocking `embed()` call can starve every other connection sharing that `LocalSet`, including unrelated, normally-fast `QueryFocusHistory` (pure SQL) calls.
- **How it was found:** building P5's own-compute p99 test, running repeated real `ask()` calls (200ms apart) against a real `StorageEngine` reproducibly pushed real query latency past the 100ms C3-query deadline (rules.md §5.7). The very first call alone reliably succeeds; it is specifically *repeated* real load that triggers it. Confirmed not a deadlock: raising the deadline to 100s, all 5 test calls completed within ~14s total.
- **Impact:** at real personal-scale data volume (confirmed again by BUG-002/BUG-004/BUG-005's own KPI-1 run against a real ~17,000-event recording), storage queries fail before an answer can even be attempted.
- **Fix needed:** make `query_hybrid`'s embedding step non-blocking (`Arc<Mutex<Embedder>>` + `spawn_blocking`, or a dedicated embedding worker task).
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, reproduced bug"); `reports/phase-05-knowledge.md` §2.8.

## BUG-002 — Query latency at scale misses the PF budget by 4-8x

- **Where:** `crates/neuroos-storage/src/lance.rs` (`LanceStore::query`)
- **What happens:** flat (pre-HNSW) vector search over a 20,000-item synthetic corpus measured p50=50ms, p99=100ms, against phases.md §7.3's own target of p50≤13ms, p99≤20ms.
- **How it was found:** `crates/neuroos-storage/tests/pf_benchmark.rs`, a real benchmark built this session because P4-S03's own completed-work-log entry had already flagged this target as "not yet benchmarked."
- **Impact:** every real question answered once meaningful data has accumulated is slower than designed.
- **Fix needed:** profile whether LanceDB's flat scan is inherently this slow at this scale on this hardware, or whether something else (per-query file IO, no result caching) is adding avoidable overhead.
- **Cross-refs:** `memory.md` tech debt (2026-09-28); `reports/phase-04-storage.md` §2.4/§2.8 (Phase 4's approved-exception reason #1).

## BUG-003 — HNSW promotion stalls the triggering query for real minutes

- **Where:** `crates/neuroos-storage/src/lance.rs` (`LanceStore::query` → `record_query_latency` → `promote_to_hnsw`)
- **What happens:** FR-STO-07's HNSW auto-promotion calls `table.create_index(...).await` *synchronously inside the query call that crosses the 5ms p99 threshold*. That one query pays the entire index-build cost as its own latency.
- **How it was found:** `tests/pf_benchmark.rs`, building a real HNSW index over 20,000 synthetic 384-dim vectors: **118.05 real seconds** for the single query that triggered promotion. Worse, steady-state post-promotion HNSW latency (p50=50ms, p99=500ms) was not faster than the flat scan it replaced, and its p99 was higher.
- **Impact:** in production, the first "unlucky" real query that pushes a family's rolling p99 over 5ms will hang for however long a real HNSW build takes at that family's real size — a severe, user-facing stall, not just a missed benchmark number. This is more severe than BUG-002 because it is a one-time cliff, not a steady degradation.
- **Fix needed:** move `create_index` off the query's own critical path (background task; serve flat-scan results while it builds). Separately investigate why HNSW isn't beating flat scan at this corpus size/config once it's asynchronous.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, reproduced bug, more severe than the embed()-blocking one"); `reports/phase-04-storage.md` §2.4/§2.8 (Phase 4's approved-exception reason #2).

## BUG-004 — C4's interactive lane reports "queue full" under sequential load

- **Where:** `cpp/neuroos-inference/src/lanes.hpp` (`LaneScheduler`, `kMaxQueueDepth = 4`), `cpp/neuroos-inference/src/server.cpp`
- **What happens:** the interactive lane's bounded queue (depth 4) reports itself full and rejects new generation requests, even though the calling client issues requests strictly sequentially (one at a time, 500ms apart, always awaiting completion before the next) — this should never approach a queue depth of 4 under single-client sequential use.
- **How it was found:** `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`, running 51 real scripted questions against a real ~8h/17,000-event recording. The error appeared repeatedly and persisted even after adding inter-question delays and fixing an unrelated storage timeout.
- **Impact:** the generation engine becomes unusable after only a handful of real requests, with no real concurrency involved — this alone can make most of a real question set fail.
- **Fix needed:** instrument `LaneScheduler::submit` and job completion (and the ring registry `attach_ring` re-uses) to find what is failing to release a queue slot or ring after a prior job completes or errors.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, measured, KPI-1 blocking"); `reports/phase-05-knowledge.md` §2.8 (Phase 5 gate-failure root cause #1); `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`'s own doc comment.

## BUG-005 — Greedy decoding degenerates into repeated-token loops on real prompts

- **Where:** `crates/neuroos-knowledge-query/src/inference_client.rs` (`InferenceClient::generate`, hardcoded `temperature: 0.0, seed: 0`)
- **What happens:** every real generation call uses pure greedy decoding with no repetition penalty. On short, simple test prompts this never surfaces a problem; on real, longer, substantive prompts (real evidence + a real question) it reliably gets stuck repeating a fragment (observed: `"---\nbrave-browser: 0\n"` and `"---\ncom.system76.CosmicTerm: 0\n"` repeated to the 128-token generation cap) instead of producing a coherent answer.
- **How it was found:** `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`, the same real KPI-1 run as BUG-004. This is a well-known small-model (BitNet, 2B parameters) failure mode; nothing in this project's prior test suite had a prompt long/complex enough to trigger it.
- **Impact:** even when BUG-004 doesn't block the request outright, the answer produced is unusable garbage, not a wrong-but-readable answer.
- **Fix needed:** make temperature/repetition-penalty configurable per call (or just non-zero by default), verified against BitNet's own recommended sampling settings.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, measured, KPI-1 blocking"); `reports/phase-05-knowledge.md` §2.8 (Phase 5 gate-failure root cause #2).

## BLOCKER-001 — Repeated disk-full incidents from `target/` growth

- **Where:** local build environment, not product code.
- **What happens:** the Rust build cache (`target/`) grows to 45-65 GB across a session's worth of builds across this workspace's heavy dependency tree (arrow/datafusion/lance), filling the disk to 100% free space. The third occurrence (2026-09-28) was compounded by `cargo llvm-cov` allocating its own separate `target/llvm-cov-target` (+7 GB) on top of an already-large `target/`, and once hit 0MB free hard enough to fail the coding agent's own shell tool, not just `cargo`.
- **Fix so far:** `cargo clean` each time (safe, fully regenerable), then an immediate targeted rebuild of anything time-sensitive (e.g. a release binary a running background process depends on) before continuing.
- **Fix needed:** an automated guard — e.g. a `df`-based pre-flight check in `just ci` or before `cargo llvm-cov`/heavy builds — since "remember to watch `df -h`" has now failed to prevent a recurrence twice in a row.
- **Cross-refs:** `memory.md` tech debt (2026-09-27, recurred 2026-09-28).

---

## Phase gate outcomes referencing this file

### Phase 4 (Semantic Storage Engine) — Passed with approved exceptions

Every story done; the soak-replay gate (`tests/soak_replay.rs`) passes for
real on all 4 committed Phase 3 dumps; crash-consistency, forget,
SQL-injection, and Landlock-sandboxing tests all pass for real; coverage
measured at 91.31%/87.77%/90.09% (lines/regions/functions), comfortably
over the 80% bar. The phase is **not** a clean pass because of
**BUG-002** and **BUG-003**: both were measured for real, for the first
time, during this phase's own closing work, and neither is fixed.
See `reports/phase-04-storage.md` for full exit-criteria evidence.

### Phase 5 (Knowledge Engine) — Failed gate

Every story done; own-compute p99, KPI-2 (deictic accuracy, 100%), and
taint property-test coverage (100%) all pass for real. The phase's own
headline metric, **KPI-1** (≥50 scripted questions over real recorded
telemetry, ≥80% correct), measured **0/51 usable answers** on its first
real run — caused by **BUG-004** and **BUG-005** (both found during this
same run), compounded by **BUG-001** (which alone caused every question
to fail on the very first attempt, before the diagnostic re-run that
isolated BUG-004/BUG-005). None of the three are fixed. See
`reports/phase-05-knowledge.md` for full exit-criteria evidence and the
recommendation to repay these before starting Phase 6 (voice), which
depends on the same C4 generation path.
