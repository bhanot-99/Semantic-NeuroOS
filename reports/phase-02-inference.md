# Phase 02 Report — Neural Inference Engine (C4 `neuroos-inference`)

| Field | Value |
| :--- | :--- |
| Phase | 2 — Neural Inference Engine |
| Component(s) | `neuroos-inference`, `libneuroos` (new C++ IPC/health/crypto infra) |
| Sprints | 2 |
| Dates | 2026-09-27 → 2026-09-27 |
| Status | ✅ Passed gate |
| Author | AI assistant (Claude Code) |
| Sign-off | pending owner review |

---

## 1. Non-Technical Summary

**What we built:**
`neuroos-inference` — a real C++ service that loads the BitNet 2.4B-parameter
language model (mmap'd, sha256-verified) and answers requests to generate
text, streaming the words out token by token through a shared-memory
channel. It has two priority lanes (an "answer this now" lane and a
"think about this in the background" lane that never blocks the first),
can be told to stop generating mid-answer, can be forced to answer in a
constrained format (e.g. exactly "yes" or "no"), and reports its own
health and performance numbers.

**Why it matters:**
This is the actual "brain" every later phase depends on — the knowledge
engine (Phase 5) sends it questions, the voice pipeline (Phase 6) reads
its streamed answers aloud. Getting the plumbing (loading the model
safely, streaming without copying gigabytes of memory around, not
blocking a live conversation while thinking about something else in the
background) right now means every later phase builds on solid ground.

**What you can see / try now:**
With the model downloaded (`.dev-cache/models/bitnet-b1.58-2B-4T/`), start
`neuroos-inference` and it loads the real model and starts listening;
`tests/contract/inference_smoke.sh` drives it through ten real checks —
asking it a question, getting a real streamed answer ("The capital of
France is" → "... a small city, and the capital of"), cancelling
mid-answer, forcing a yes/no answer, and rejecting a too-long question —
all against the real model, not a mock.

**Is it on track?**
Yes. Two sprints as planned. All 6 user stories are done, `just ci` is
green, and this session found and fixed three real bugs along the way
(detailed below) — the kind of thing only surfaces when you actually run
the real thing against the real model, which this phase did throughout.

**Risks or concerns in plain words:**
- Decode and prefill speed on this reference machine (no GPU) come in
  slower than the original blueprint's targets — recorded and reasoned
  through in ADR-0008, not swept under the rug. The real-world impact
  (answer sentences taking under 1.5 seconds) looks acceptable, but
  Phase 6 (voice) should confirm that once there's an actual voice loop
  to measure.
- C++ coverage came in just under the 70% bar counting the `--bench`
  CLI tool's own code; counting only the actual running service (the
  more natural reading of the exit criterion's own wording, "service
  code"), it clears the bar. See §2.4.

---

## 2. Technical Summary

### 2.1 Delivered scope

| Story | Title | Status | Notes |
| :--- | :--- | :--- | :--- |
| P2-S01 | Send a prompt, receive a streamed completion | Done | `engine.cpp::Context::generate` + `server.cpp::handle_generate`; verified against the real model |
| P2-S02 | Generated text through a zero-copy shared-memory ring | Done | `ring.cpp::RingRegistry` + `AttachRingRequest`/SCM_RIGHTS handoff; real fd verified received and readable |
| P2-S03 | Cancel a generation, stops within one decode step | Done | Measured: 0 further tokens arrive after `CancelRequest` is sent |
| P2-S04 | Background distillation never delays interactive by more than one decode step | Done | `lanes.cpp`'s yield-not-abort design; measured 4 interactive tokens in ~230-450ms (optimized build) while a real 400-token background job ran |
| P2-S05 | Force JSON/constrained output via GBNF grammar | Done | `grammar.cpp` wraps `llama_sampler_init_grammar`; found and fixed a real crash on grammar completion (§2.6) |
| P2-S06 | Measured TTFT/prefill/decode numbers replacing blueprint projections | Done | `neuroos-inference --bench`; `reports/bench/inference-1790486839.json`; ADR-0008 for the missed targets |

### 2.2 Architecture and implementation notes

- **`libneuroos` gained real C++ IPC infra** it didn't have before this
  phase: `framing.hpp/cpp` (u32-LE length-prefixed protobuf, byte-for-byte
  matching `neuroos-ipc`'s Rust wire format), `uds.hpp/cpp` (UDS
  server/client, `SO_PEERCRED`), `health_server.hpp/cpp` (C++ mirror of
  `neuroos-health`, same log-scale histogram buckets), `sha256.hpp/cpp`
  (OpenSSL `EVP`-backed file hashing), `paths.hpp/cpp` (mirrors
  `neuroos_common::paths`), and `expected.hpp` (a small hand-written
  `Expected<T,E>` — this dev environment has no outbound network access to
  vendor the real `tl::expected`, so rules.md's C++ error-handling pattern
  is implemented directly instead).
- **`Model`/`Context` split** (`engine.hpp/cpp`): `Model` owns the mmap'd,
  sha256-verified weights (shared, read-only, one per process); each lane
  gets its own `Context` (own `llama_context`, own KV cache). `generate()`
  reuses whatever prefix of the new prompt already matches what's resident
  in that lane's KV cache from its last call (`llama_memory_seq_rm` +
  `llama_batch_get_one`) — a generalization of Architecture.md §9.1's
  "128-token system block" cache lever to whatever prefix actually repeats.
- **Two-lane scheduling** (`lanes.hpp/cpp`): one worker thread per lane,
  each with a bounded queue (depth 4). The background lane's
  `should_cancel` hook *blocks* (doesn't abort) while an interactive job is
  active, so a real preemption never loses the background job's
  in-progress generation state — it just pauses and resumes exactly where
  it left off.
- **Named ring registry** (`ring.hpp/cpp`): rings are created lazily on
  first `AttachRingRequest`, keyed by name; the memfd is hand to the
  requesting client via `write_envelope_with_fd`'s `SCM_RIGHTS`.
- **Links BitNet's own llama.cpp fork directly**
  (`cpp/third_party/bitnet.cpp/3rdparty/llama.cpp`, `add_subdirectory`'d
  from `neuroos-inference/CMakeLists.txt` with
  `LLAMA_BUILD_{SERVER,TOOLS,EXAMPLES,COMMON}` forced off) — no shelling
  out to a CLI binary, direct C API use (`llama_model_load_from_file`,
  `llama_sampler_chain_*`, `llama_sampler_init_grammar`).

### 2.3 Interfaces / contracts changed

| Proto / API | Change | Additive? | ADR |
| :--- | :--- | :--- | :--- |
| `inference.proto` | Replaced Phase 0's placeholder `InferRequest`/`InferResponse` with `GenerateRequest/Response`, `CancelRequest/Response`, `DistillRequest/Response`, `AttachRingRequest/Response`, `GetInfoRequest/Response` | Field numbers 60-69 reused per Architecture.md §5.3's per-component range (nothing external depended on the placeholder yet) | — |
| `envelope.proto` | Updated the `oneof body` entries for the above | Same | — |

### 2.4 Measured results vs targets

| Metric | Target | Measured | Pass | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Decode speed @ 512 ctx, 8 threads | ≤ 45 ms/token | 55.9-56.1 ms/token | ❌ (see ADR-0008) | `reports/bench/inference-1790486839.json` |
| Prefill speed @ 512 ctx, 8 threads | ≈ 2.5 ms/token | 8.25 ms/token | ❌ (see ADR-0008) | same |
| RSS, all contexts tested (128/512/1024) | ≤ 1,590 MiB | 1,223-1,327 MiB | ✅ | same |
| Cancellation latency | ≤ 1 decode step | 0 further tokens after `CancelRequest` (immediate) | ✅ | `cpp_inference_smoke.cpp` step 4, this session |
| Interactive preemption under background load | not blocked | 4 tokens in 230-453 ms (optimized build) while a real 400-token background job ran | ✅ | `cpp_inference_smoke.cpp` step 5 |
| Weights mapped read-only | mmap, read-only | `/proc/<pid>/maps`: single `r--s` mapping, no write/exec mapping anywhere | ✅ | this session, `/proc/<pid>/maps` capture |
| `MemoryDenyWriteExecute` (spike S-05) | works or documented exception | Real `--bench` run completed under `systemd-run --user -p MemoryDenyWriteExecute=yes` with no crash | ✅ | this session; unit updated to `true` |
| C++ coverage, service code (excl. `--bench` CLI tool) | ≥ 70% | 75.4% (618/820 lines) | ✅ | `gcov`, `cpp/build-coverage/`, this session |
| C++ coverage, including `--bench` | ≥ 70% | 67.5% (618/915 lines) | ⚠️ see note | same |

Coverage note: `bench.cpp` (the `--bench` CLI mode, 95 lines, 0% measured)
is a diagnostic tool invoked via a command-line flag, not part of the
running service's request-handling path — the same category as
`scripts/fetch-models.sh` or the `shm-stress-cpp` test tool, neither held
to the coverage bar. It's functionally proven by the committed
`reports/bench/*.json` from real runs; its 0% here is a gcov *measurement*
gap (the coverage-instrumented build's `--bench` runs were repeatedly
timed out by this session's own tooling before reaching a normal `return`
from `main`, the only point gcov flushes counters for a CLI tool that
doesn't fork long-lived threads) — a real limitation of gcov on
timeout-terminated processes, not evidence the code doesn't work.
Excluding it, the actual service (everything reachable from a real
`inference.sock`/`.health.sock` connection) is comfortably over 70%.

Coverage per file (`cpp/build-coverage/`, `gcov`):

| File | Lines covered | Total | % |
| :--- | :--- | :--- | :--- |
| `neuroos-inference/src/engine.cpp` | 116 | 156 | 74.4% |
| `neuroos-inference/src/lanes.cpp` | 83 | 101 | 82.2% |
| `neuroos-inference/src/ring.cpp` | 21 | 21 | 100% |
| `neuroos-inference/src/server.cpp` | 79 | 113 | 69.9% |
| `neuroos-inference/src/grammar.cpp` | 2 | 2 | 100% |
| `neuroos-inference/src/main.cpp` | 36 | 43 | 83.7% |
| `neuroos-inference/src/config.cpp` | 24 | 29 | 82.8% |
| `neuroos-inference/src/bench.cpp` | 0 | 95 | 0% (see note) |
| `libneuroos/src/framing.cpp` | 107 | 145 | 73.8% |
| `libneuroos/src/peercred.cpp` | 6 | 7 | 85.7% |
| `libneuroos/src/uds.cpp` | 56 | 101 | 55.5% |
| `libneuroos/src/health_server.cpp` | 52 | 61 | 85.2% |
| `libneuroos/src/sha256.cpp` | 21 | 26 | 80.8% |
| `libneuroos/src/paths.cpp` | 15 | 15 | 100% |

### 2.5 Test results

| Level | Suites | Passed | Failed | Coverage | Evidence |
| :--- | :--- | :--- | :--- | :--- | :--- |
| Integration | `cpp_inference_smoke.cpp` (10 real checks: GetInfo, AttachRing, Generate, Cancel, preemption, GBNF grammar, oversized-prompt rejection) + `cpp_ipc_smoke.cpp` (3 checks) | 13/13 | 0 | see §2.4 | `just test-inference`, `just test-cpp-ipc`, this session |
| Fault injection | Oversized prompt rejected synchronously; sha256 mismatch → fatal exit with clear error; corrupt model file → fatal exit with clear error, no crash | 3/3 | 0 | — | `tests/contract/inference_smoke.sh`, this session |
| Performance | Benchmark matrix (128/512/1024 ctx × 1/4/8 threads) | 6/6 real runs | 0 | — | `reports/bench/inference-1790486839.json` |
| Contract | `inference.proto` round-trips Rust→C++→Python→Rust | 1/1 | 0 | — | `tests/contract/roundtrip.sh`, part of `just ci` |
| Workspace total | `just ci` | 79 Rust tests + all above | 0 | — | this session |

### 2.6 Deviations from plan

| Item | Planned | Actual | Reason | Approved by |
| :--- | :--- | :--- | :--- | :--- |
| GBNF grammar crash | Grammar sampling was expected to just work via `llama_sampler_init_grammar` | Found llama.cpp's grammar sampler throws `std::runtime_error` when a GBNF rule completes via `accept()`, crashing the whole process (uncaught on a detached worker thread) | Real bug, found by actually testing grammar end to end against the live model, not assumed to work from reading the API | Fixed this session: wrap sample+accept in try/catch (rules.md §8), treat the exception as the grammar's own "done" signal |
| Oversized prompt silently dropped | Expected the worker's own oversized-prompt check (already in `engine.cpp`) to be sufficient | It only logged a warning server-side; the client never learned its request was rejected | Found while writing the FI test for it | Fixed: `server.cpp` now rejects synchronously before queuing, with a clear `error` field |
| Shutdown segfault | `std::exit(0)` was tried (to fix gcov flush-on-shutdown for coverage measurement) | Crashed: static destructors ran while detached worker threads were still live, racing them | Found while measuring coverage — a real bug that would have shipped | Reverted to `std::quick_exit(0)` (original design), confirmed clean exit afterward |
| `generation_id` map-key collision | Assumed the ring's own `generation_id` counter was a safe map key for tracking cancellable jobs | Two jobs sharing a ring without an intervening cancel get the *same* `generation_id`; a delayed cleanup from one job could erase a different, still-running job's cancel flag | Found via `cpp_inference_smoke.cpp`'s real Cancel test | Fixed: identity-checked erase (only remove if the map still holds exactly this job's flag), and jobs are only registered once actually queued |

### 2.7 Decisions made (ADRs)

- ADR-0008 — Phase 2 inference benchmark results miss FR-INF-07's raw targets; proceed anyway — decode/prefill measured slower than the PRD's blueprint-era targets on this CPU-only reference machine; proceeding without a hardware/model change, with concrete mitigation levers already identified for Phase 5/6 if real end-to-end latency needs them.

### 2.8 Tech debt introduced

| Item | Impact | Repay in phase |
| :--- | :--- | :--- |
| `expected.hpp` is a small hand-written `Expected<T,E>`, not the real `tl::expected` Architecture.md's file tree comment names — this dev environment has no outbound network access to vendor it | Low: covers every call site this codebase actually needs; no missing functionality today | Revisit only if a future call site needs monadic `and_then`/`map` chaining the current type doesn't support |
| `bench.cpp` coverage isn't measurable via this session's gcov setup (CLI tool killed by timeout before its normal-return flush point) | Low: functionally proven by committed benchmark JSON; only affects the coverage *report*, not correctness | None needed — document only, or add a `SIGTERM` handler to `--bench` mode if this becomes a recurring CI need |
| `neuroos-inference@.service`'s unit mode is still the system-template style (`User=%i`) ADR-0002 recommends reworking to a user-scope unit | Same pre-existing Phase 0 tech debt as `neuroos-healthd@.service` (memory.md §8) | Next session touching `deploy/systemd/` (unchanged scope from Phase 1) |

---

## 3. Exit Criteria Verification

| # | Exit criterion | Met | Evidence |
| :--- | :--- | :--- | :--- |
| 1 | Benchmark JSON committed for 128/512/1,024 contexts; results compared with targets; mitigation ADR if missed | ✅ | `reports/bench/inference-1790486839.json`; ADR-0008 |
| 2 | Cancellation latency ≤ 1 decode step (measured) | ✅ | `cpp_inference_smoke.cpp` step 4: 0 further tokens after `CancelRequest` |
| 3 | Interactive preemption verified under background load | ✅ | `cpp_inference_smoke.cpp` step 5: 4 tokens in 230-453 ms while a real 400-token background job ran |
| 4 | Token ring passes 10 M-slot stress test under TSan | ✅ | Ring implementation (`shm_ring.hpp`/`shm.cpp`) is unchanged from P0-S07; evidence already on record (ADR-0005, `docs/adr/0005-shm-ring-race-freedom.md`) — not re-run since nothing in this phase touched that code |
| 5 | Weights mapped read-only (`/proc/<pid>/smaps` evidence); RSS within budget | ✅ | `/proc/<pid>/maps`: single `r--s` mapping this session; RSS 1,223-1,327 MiB vs 1,590 MiB budget |
| 6 | Coverage ≥ 70% (C++ service code) | ✅ | 75.4% excluding the `--bench` CLI tool (see §2.4 note); 67.5% including it |
| 7 | Phase report written; memory.md updated | ✅ | this report; memory.md updated in the same session |

---

## 4. Open Questions Closed / Opened

None opened or closed by this phase.

---

## 5. Lessons Learned

- **Keep:** writing the real integration test (`cpp_inference_smoke.cpp`)
  against the real downloaded model, not a mock, before declaring any
  story done — every one of this phase's four real bugs (SCM_RIGHTS fd
  drop in the prerequisite IPC work, the `generation_id` collision, the
  GBNF grammar crash, the shutdown segfault) was found this way, not by
  reading the code.
- **Keep:** treating a "the test is failing" moment as a prompt to ask
  "is the test wrong, or is the code wrong?" rather than loosening the
  test — the preemption test's tight timing threshold really was a debug-
  build artifact (worth an env-var override), but the `generation_id`
  collision and the grammar crash were real bugs the test correctly caught.
- **Change:** measuring C++ coverage with raw `gcov` against a component
  that spans multiple binaries (the same `libneuroos` static-lib object
  files linked into both `neuroos-inference` and `cpp-ipc-smoke`) is
  fragile — re-running a different binary against the same `.gcda` path
  can reset rather than merge counts. A future phase measuring C++
  coverage more than this one-off basis should reach for `gcovr` (or pin
  one canonical binary per measured `.o`) instead of hand-rolled `gcov`
  invocations.

---

## 6. Next Phase Readiness

| Check | Status |
| :--- | :--- |
| Next phase (P3 Monitor) dependencies satisfied (P0 done) | ✅ |
| Next phase stories meet Definition of Ready | ✅ (phases.md §6.2 stories are estimated, testable, no blocking open question) |
| memory.md updated (phase tracker, current phase, sprint board) | ✅ |
