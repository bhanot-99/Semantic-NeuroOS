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

| ID      | Date found | Phase                 | Severity     | Status                                             | Summary                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| :------ | :--------- | :-------------------- | :----------- | :------------------------------------------------- | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| BUG-001 | 2026-09-28 | P4 (found in P5)      | High         | **Fixed** (2026-09-28)                             | `StorageEngine::query_hybrid`'s `embed()` call was synchronous and blocked the async runtime's own thread on every call, not just callers waiting on the shared `Mutex<StorageEngine>`                                                                                                                                                                                                                                                                      |
| BUG-002 | 2026-09-28 | P4                    | High         | **Fixed** (2026-09-28), target still not fully met | Query latency at 20,000 items was 4-8x over the phases.md budget (flat scan p50=50ms/p99=100ms vs. 13ms/20ms target) — real per-query I/O and fragmentation overhead fixed (verified ~35-42ms now), but flat-scan itself still doesn't hit 13ms at this scale (see write-up; BUG-003 is the real path to meeting it)                                                                                                                                        |
| BUG-003 | 2026-09-28 | P4                    | **Critical** | Open                                               | FR-STO-07's HNSW promotion runs `create_index` synchronously inside the triggering query — measured **118 real seconds** of stall on one query; post-promotion HNSW latency isn't even faster than the flat scan it replaced                                                                                                                                                                                                                                |
| BUG-004 | 2026-09-28 | P5 (root cause in P2) | **Critical** | **Fixed** (2026-09-28)                             | C4's interactive lane (`kMaxQueueDepth = 4`) reported `"lane queue is full"` under a merely sequential, spaced-out real workload — root cause was ring cross-talk, not a scheduler slot leak (see write-up below)                                                                                                                                                                                                                                           |
| BUG-005 | 2026-09-28 | P5 (root cause in P2) | **Critical** | **Fixed** (2026-09-28)                             | `InferenceClient::generate` hardcoded greedy decoding (`temperature: 0.0`, no repetition penalty); on real, longer, substantive prompts this reliably degenerated into a repeated-token loop instead of a real answer                                                                                                                                                                                                                                       |
| BUG-006 | 2026-09-28 | P5                    | High         | Open                                               | Real `storage.sock` `read deadline exceeded` failures persist in `kpi1_eval.rs` (~20/51 questions) even after BUG-001/BUG-002 are fixed — real per-call storage latency (~170-190ms, well over the 100ms deadline) traced to system-wide CPU contention with the real, concurrently-running C4 BitNet inference process (fire-and-forget background distillation, `distill::maybe_spawn_distillation`), not to `neuroos-storage` itself; see write-up below |

## Open blockers (environment / tooling, not product code)

| ID | Date found | Severity | Status | Summary |
| :--- | :--- | :--- | :--- | :--- |
| BLOCKER-001 | 2026-09-27, recurred 2026-09-28 (4th time) | Medium | Open | `target/` (Rust build cache) repeatedly grows to 45-65 GB and fills the disk to 100%, most recently compounded by `cargo llvm-cov`'s own separate `target/llvm-cov-target` (+7 GB). No automated guard exists; `cargo clean` is the only fix so far, and it was needed a **fourth** time this session (grew to 60GB, 16GB free, caused a real linker "Bus error" failure) despite being logged as tech debt three times already |

---

## BUG-001 — `query_hybrid`'s blocking `embed()` starves `storage.sock` (Fixed)

- **Where:** `crates/neuroos-storage/src/engine.rs` (`StorageEngine::query_hybrid`/`store_chunk`/`reindex_family`), `crates/neuroos-storage/src/embed.rs`
- **What happened:** `query_hybrid`/`store_chunk` called `self.embedder.embed(&[text])` synchronously (real ONNX inference, CPU-bound, ~10ms/call) directly inside the async request handler, with no `spawn_blocking`. `StorageEngine` is reached only through one `Arc<Mutex<StorageEngine>>` (`server.rs`), so every request was already serialized by design — the bug was never that serialization, it was that running ~10ms of real CPU work *inline* on the async runtime's own thread froze the whole reactor for that duration: no other task (another connection, the accept loop, a timer) could make any progress at all until it returned.
- **How it was found:** building P5's own-compute p99 test, running repeated real `ask()` calls (200ms apart) against a real `StorageEngine` reproducibly pushed real query latency past the 100ms C3-query deadline (rules.md §5.7).
- **Root cause research + fix (2026-09-28):** checked the real pinned crate versions used here (`Cargo.lock`: `ort` 2.0.0-rc.13, `fastembed` 7.1.0) directly against their vendored/registry source rather than generic docs (older `ort` discussions online claim its `Session` isn't `Send`/`Sync` — not true for this pinned version): `ort` 2.0.0-rc.13's `Session` is `unsafe impl Send + Sync` (`session/mod.rs`), and `fastembed::TextEmbedding`'s fields (`Tokenizer`, `Session`, plain data) are all `Send`, so `Embedder` is `Send`. That makes Tokio's own documented fix directly applicable: `StorageEngine.embedder` is now `Arc<std::sync::Mutex<Embedder>>`, and a new `embed_blocking` helper moves the real `embed()` call onto `tokio::task::spawn_blocking`'s dedicated thread pool, `.await`ing the result — a genuine yield point, so the runtime keeps servicing everything else while the real inference call runs elsewhere. Applied to both hot-path call sites (`store_chunk`, `query_hybrid`) and the background `reindex_family` re-index loop (same bug, lower severity since it doesn't hold the shared mutex, fixed for consistency).
- **Verified:** real `cargo nextest`/`clippy -D warnings` clean on `neuroos-storage`; re-ran the real `kpi1_eval.rs` harness (real model, real ~8h recording) before/after and confirmed `embed()` no longer appears as a blocking stage (isolated timing: consistently ~6-13ms, matching `embed.rs`'s own documented budget, never blocking anything else).
- **Caveat (new finding, logged separately as BUG-006):** `kpi1_eval.rs` still shows real `storage.sock` timeouts after this fix — traced to a *different*, real mechanism (system-wide CPU contention with concurrently-running C4 inference, not `neuroos-storage`'s own code). See BUG-006 below; this bug (the async-runtime-blocking `embed()` call) is fully fixed and verified independently of that.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, reproduced bug"); `reports/phase-05-knowledge.md` §2.8.

## BUG-002 — Query latency at scale misses the PF budget by 4-8x (Fixed, target still not fully met)

- **Where:** `crates/neuroos-storage/src/lance.rs` (`LanceStore`)
- **What happened:** flat (pre-HNSW) vector search over a 20,000-item synthetic corpus measured p50=50ms, p99=100ms, against phases.md §7.3's own target of p50≤13ms, p99≤20ms.
- **How it was found:** `crates/neuroos-storage/tests/pf_benchmark.rs`.
- **Root cause research + fixes (2026-09-28), three real, distinct, verified overhead sources found:**
  1. **Table re-opened on every call.** `LanceStore::ensure_table` called `conn.table_names().execute()` (a real directory listing) + `conn.open_table()` (a real manifest read) on *every* `query`/`insert`/`delete`/etc. call, despite `open()`'s own doc comment already saying tables should only need opening once. Checked LanceDB's own vendored source (`vendor/lancedb-0.39.0-patched/src/table/dataset.rs`) directly: a `Table` is meant to be long-lived (its own test suite, `test_iops_open_strong_consistency`/`test_reload_resets_consistency_timer`, measures real read IOPS to prove repeated calls on the *same* handle cost ~0 extra I/O), and with the default (`read_consistency_interval: None` → `Lazy`) consistency mode used here, a write through a handle calls the wrapper's own `update()`, so a query right after a write on the *same* handle always sees it (`test_update_stores_newer_version`) — safe to cache with no staleness risk, since this process is the only writer. **Fix:** `LanceStore` now opens every family's `Table` once in `open()` and caches it in a `tables: HashMap`; every method does an O(1) lookup instead of re-opening. **Verified** via `pf_benchmark.rs`'s real 20k-item benchmark with raw (non-bucketed — see below) per-query timing: flat-scan queries dropped from ~50-100ms to a consistent ~35-42ms.
  2. **Fragment-file explosion from unbatched real-time ingest.** Real telemetry ingest calls `LanceStore::insert` one chunk at a time (`store_chunk`'s own doc comment: "fine at telemetry ingest rates"), and Lance's on-disk format is append-only, so every single-row insert becomes its own fragment *file*; a flat scan must open every fragment. Directly measured against this machine's own real ~8h recording: one family (`attention`) had accumulated **192 fragment files** from real ingest. **Fix:** `LanceStore::compact_all_families` (`table.optimize(OptimizeAction::Compact{..})`, LanceDB's own documented answer to "small files hurt read/write performance") is now called from `StorageEngine::backup`, riding the existing 6-hourly maintenance cadence (Architecture.md §7.5) rather than the hot ingest path (rules.md AB-11). **Verified** against the real 192-fragment directory captured from a live run: eliminated a cold-start ~18ms outlier down to a consistent ~7ms.
  3. **Five families queried sequentially.** `query_all_families` looped over the 5 domain families one at a time; changed to `futures::future::try_join_all` so independent per-family queries (each its own LanceDB table/files) run concurrently instead of summing. Applied as a correct, harmless structural improvement, but its benefit did **not** show up measurably in this session's own testing — likely because `storage.sock`'s connection handling runs on one `LocalSet`-pinned OS thread (`server.rs`, required by `StorageEngine`'s `!Sync` `rusqlite::Connection`) and LanceDB/DataFusion's query execution doesn't appear to yield control back to the executor mid-poll, so "concurrent" futures on that one thread still ran one at a time in practice. Not reverted (still directionally correct, zero correctness risk — all `lance.rs` tests, including cross-family merge and write-then-read-same-handle tests, still pass), but flagged here rather than claimed as a proven win.
- **Also discovered:** the histogram-based p50/p99 helpers used for reporting (`neuroos_health::{p50_ns,p99_ns}`) return the *bucket upper bound* a value falls into (fixed buckets: ...10ms, 50ms, 100ms, 500ms...), not an interpolated real value — real measurements need `Instant`-based raw timing (as `pf_benchmark.rs` already collects internally) to see genuine improvement across a bucket-coarse range like 35ms vs 50ms.
- **Not fixed, and out of scope here:** even after fixes 1-3, flat-scan queries on the 20,000-item benchmark are ~35-42ms, still over the 13ms/20ms target — LanceDB's own documentation states brute-force search cost is inherent to dataset size on available hardware ("not scalable beyond a few hundred thousand vectors... relies primarily on hardware resources, not software knobs"), and this session's *real* 20k-item corpus post-HNSW-promotion measurement (~12-14ms per query, matching the target) confirms the actual path to meeting phases.md's number here is **BUG-003** (fixing the HNSW promotion stall), not further flat-scan tuning.
- **Cross-refs:** `memory.md` tech debt (2026-09-28); `reports/phase-04-storage.md` §2.4/§2.8 (Phase 4's approved-exception reason #1).

## BUG-006 — Real `storage.sock` timeouts persist under KPI-1 load, traced to inference-side CPU contention

- **Where:** `crates/neuroos-knowledge-query/src/distill.rs` (`maybe_spawn_distillation`, fire-and-forget `tokio::spawn`), not `neuroos-storage`
- **What happens:** re-running the real `kpi1_eval.rs` harness after BUG-001/BUG-002 are fixed still shows ~20/51 questions failing with `evidence retrieval failed: storage.sock request failed: read deadline exceeded` (100ms deadline, `storage_client.rs::STORAGE_QUERY_DEADLINE`) — always the *same* first ~20 questions, then 100% reliable from question 21 onward.
- **Root cause investigation (2026-09-28):** instrumented `StorageEngine::query_hybrid` to time `embed()` and `LanceStore::query_all_families` separately. `embed()` is fine post-BUG-001-fix (~6-13ms). `query_all_families` alone was costing ~170-190ms for the first ~19-20 questions, then dropping to ~30-65ms from question ~21 onward — *not* explained by BUG-002's fixes (table caching and fragment compaction were both already verified independently against this exact real corpus and don't explain a ~20-request warm-up cliff). Given `maybe_spawn_distillation` fires a real, unbounded, un-awaited `tokio::spawn` background job (a full ~256-token real BitNet generation in the separate C4 process, several seconds each) whenever a question's raw evidence exceeds 1,000 tokens, and does so on essentially every early question in this real corpus, the leading hypothesis is that a backlog of concurrent real background-inference jobs is consuming enough of this machine's CPU to starve `neuroos-storage`'s own async runtime/LanceDB execution — not proven with direct instrumentation of `distill.rs` itself, so recorded as a hypothesis pending its own dedicated investigation, not a confirmed root cause.
- **Impact:** KPI-1's own ≥80%-correct gate is still not achievable end-to-end even with BUG-001/BUG-002/BUG-004/BUG-005 all fixed.
- **Fix needed:** its own session — confirm the CPU-contention hypothesis (e.g. instrument `distill.rs`/measure concurrent background-job count), then bound/rate-limit fire-and-forget distillation (a semaphore, or simply awaiting it instead of fire-and-forget if FR-KNO-05's "never blocks the hot path" requirement can be satisfied another way).
- **Cross-refs:** BUG-001 (ruled out as the cause of this specific symptom), BUG-002 (same).

## BUG-003 — HNSW promotion stalls the triggering query for real minutes

- **Where:** `crates/neuroos-storage/src/lance.rs` (`LanceStore::query` → `record_query_latency` → `promote_to_hnsw`)
- **What happens:** FR-STO-07's HNSW auto-promotion calls `table.create_index(...).await` *synchronously inside the query call that crosses the 5ms p99 threshold*. That one query pays the entire index-build cost as its own latency.
- **How it was found:** `tests/pf_benchmark.rs`, building a real HNSW index over 20,000 synthetic 384-dim vectors: **118.05 real seconds** for the single query that triggered promotion. Worse, steady-state post-promotion HNSW latency (p50=50ms, p99=500ms) was not faster than the flat scan it replaced, and its p99 was higher.
- **Impact:** in production, the first "unlucky" real query that pushes a family's rolling p99 over 5ms will hang for however long a real HNSW build takes at that family's real size — a severe, user-facing stall, not just a missed benchmark number. This is more severe than BUG-002 because it is a one-time cliff, not a steady degradation.
- **Fix needed:** move `create_index` off the query's own critical path (background task; serve flat-scan results while it builds). Separately investigate why HNSW isn't beating flat scan at this corpus size/config once it's asynchronous.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, reproduced bug, more severe than the embed()-blocking one"); `reports/phase-04-storage.md` §2.4/§2.8 (Phase 4's approved-exception reason #2).

## BUG-004 — C4's interactive lane reports "queue full" under sequential load (Fixed)

- **Where:** `cpp/neuroos-inference/src/lanes.hpp` (`LaneScheduler`, `kMaxQueueDepth = 4`), `cpp/neuroos-inference/src/server.cpp`
- **What happened:** the interactive lane's bounded queue (depth 4) reported itself full and rejected new generation requests, even though the calling client issues requests strictly sequentially (one at a time, 500ms apart, always awaiting completion before the next) — this should never approach a queue depth of 4 under single-client sequential use.
- **How it was found:** `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`, running 51 real scripted questions against a real ~8h/17,000-event recording. The error appeared repeatedly and persisted even after adding inter-question delays and fixing an unrelated storage timeout.
- **Root cause (found by instrumenting `LaneScheduler::submit`/job completion and re-running the real harness, per the investigation ask):** this was never a scheduler slot leak — every `submit`/`pop`/`finish` in the trace balanced correctly. The real bug is in the shared memfd ring (`cpp/libneuroos/include/libneuroos/shm_ring.hpp`): `orchestrate::ask` reuses one fixed `ring_name` ("knowledge-text") for every question (by design, Architecture.md §5.5), and a ring's `generation_id` only ever advances on an explicit `Cancel` — never on an ordinary new job. So every job on that ring shared `generation_id=0`. When question N+1's client attached a *fresh* `RingReader` (`next_seq_ = 0`) while question N's job was still mid-decode, `RingReader::try_read`'s "stale generation" filter (`generation != cur_gen`) was a no-op — `cur_gen` was still 0 for every slot ever written — so the new reader happily read question N's still-in-flight tokens as if they were its own answer, "finished" in milliseconds, and the client raced ahead and submitted question N+2, N+3, … while N's real ~8s generation was still running. That real, uncontrolled pile-up (not a phantom accounting bug) is what pushed the queue to depth 4. Verified directly: a real run's transcript answer for question 29 ("what autonomous data architecture notes do I have") literally contained the text of question 18's ("what OnePlus phone specs did I look at") evidence — proof of cross-talk on the shared ring.
- **Fix:** added `RingWriter::next_generation()` (same bump as `cancel()`) and call it in `server.cpp`'s `handle_generate`/`handle_distill` for every new job, not just on explicit cancel. Each job now gets a distinct `generation_id`, so `RingReader::try_read`'s existing stale-generation skip actually works: a reader attached mid-stream correctly skips a prior job's leftover slots and waits for its own job's tokens instead of misattributing them.
- **Verified:** re-ran the real `kpi1_eval.rs` harness (real model, real 8h recording) after the fix — 0 occurrences of `"lane queue is full"` across all 51 questions (down from 51/51). Real `cpp_inference_smoke` contract test (GetInfo/AttachRing/Generate/Cancel/preemption/GBNF/oversized-prompt) still passes, confirming `Cancel`'s own generation-bump semantics weren't broken by making bumps routine.
- **Cross-refs:** `memory.md` tech debt (2026-09-28, "Real, measured, KPI-1 blocking"); `reports/phase-05-knowledge.md` §2.8 (Phase 5 gate-failure root cause #1); `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`'s own doc comment.

## BUG-005 — Greedy decoding degenerates into repeated-token loops on real prompts (Fixed)

- **Where:** `crates/neuroos-knowledge-query/src/inference_client.rs` (`InferenceClient::generate`, hardcoded `temperature: 0.0, seed: 0`)
- **What happened:** every real generation call used pure greedy decoding with no repetition penalty. On short, simple test prompts this never surfaced a problem; on real, longer, substantive prompts (real evidence + a real question) it reliably got stuck repeating a fragment (observed: `"---\nbrave-browser: 0\n"` and `"---\ncom.system76.CosmicTerm: 0\n"` repeated to the 128-token generation cap) instead of producing a coherent answer.
- **How it was found:** `crates/neuroos-knowledge-query/tests/kpi1_eval.rs`, the same real KPI-1 run as BUG-004. This is a well-known small-model (BitNet, 2B parameters) failure mode; nothing in this project's prior test suite had a prompt long/complex enough to trigger it.
- **Impact:** even when BUG-004 didn't block the request outright, the answer produced was unusable garbage, not a wrong-but-readable answer.
- **Fix:** added `repetition_penalty` to `GenerateRequest` (proto field 8; proto3 default 0.0 is treated server-side as "unset" and mapped to 1.0/disabled, since 0.0 is not a valid `llama_sampler_init_penalties` "disabled" value). `Context::generate` (`engine.cpp`) now adds `llama_sampler_init_top_k(40)` + `llama_sampler_init_penalties(...)` to the sampler chain when `repetition_penalty != 1.0`. `InferenceClient::generate` now defaults to `temperature=0.7, repetition_penalty=1.1` (llama.cpp's own CLI default) instead of hardcoded `0.0`/none, via a new `generate_with_sampling` that makes both configurable per call.
- **Verified:** re-ran the real `kpi1_eval.rs` harness after the fix — no more single-fragment infinite loops; answers are now multi-line, on-topic, bounded text specific to each question (e.g. the COSMIC-terminal question's answer correctly referenced `com.system76.CosmicTerm`/`CosmicFiles`). Small-model answer *quality* (staying grounded, not repeating whole sentences once or twice within an answer) is still imperfect, as expected for a 2B model — that's a model-capability question, not this bug's degenerate-loop failure mode.
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
isolated BUG-004/BUG-005). **Update 2026-09-28:** BUG-004 and BUG-005 are
now fixed and verified against a real re-run of this same harness (0
`"lane queue is full"` errors, no more degenerate repeated-token loops).
BUG-001 is not fixed and still causes real `storage.sock` timeouts on
~40% of the 51 questions in a fresh re-run, so KPI-1's own ≥80%-correct
gate is still not met overall — repaying BUG-001 is the remaining blocker
before Phase 6 (voice), which depends on the same C4 generation path. See
`reports/phase-05-knowledge.md` for full exit-criteria evidence.
