# memory.md — Project State Tracker

> **Crucial rule:** this file is the single source of truth for project state. It MUST be read at the start of every work session and updated:
> - at the end of every work session,
> - whenever a story changes column (Ready → In Progress → Done),
> - whenever the file being worked on changes,
> - whenever a decision, blocker, open question or deviation appears,
> - at every phase gate.
>
> Keep entries short and factual. Newest entries go on top. Dates are absolute (YYYY-MM-DD). Never delete history; strike through or mark superseded instead.

---

## 1. Status Snapshot

| Field | Value |
| :--- | :--- |
| Last updated | 2026-09-27 |
| Project stage | Phase 0 done (owner signed off); Phase 1 (healthd) done, merged (PR #2); Phase 2 (Inference) done, merged (PR #3); Phase 3 (Monitor) done and merged (PR #4) except the ≥8h recording, which was stopped mid-attempt (found a real bug, see §8) and will be redone fresh next session; **Phase 4 (Storage) in progress on `p4/s01-storage`: P4-S01…S03 done (ingest filter, domain adapters+taint, real embedder+LanceDB+hybrid query, all live-verified)** |
| Current phase | **Phase 4 — Semantic Storage Engine (C3)** — building everything not blocked on the recording (owner's explicit direction); soak-replay gate stays blocked until fresh dumps exist |
| Current sprint | Sprint 4 |
| Current story | P4-S04 next (`QueryFocusHistory`) — see §4 sprint board for the full backlog |
| Overall progress | 3 / 11 phases fully done and merged (Phase 0, 1, 2); Phase 3 merged, recording redo pending; Phase 4 in progress on branch `p4/s01-storage` |
| Health | 🟢 On track | 
| Next milestone | M1 done; Phase 3 sensing merged; Phase 4 storage engine underway, soak-replay gate blocked on a fresh recording |

---

## 2. Currently Working On

| Field | Value |
| :--- | :--- |
| Story | P4-S04 (`QueryFocusHistory`) next; P4-S01…S03 done this session |
| File(s) being edited | `crates/neuroos-storage/src/{ingest/filter.rs, adapters/mod.rs, sqlite.rs, embed.rs, lance.rs, engine.rs}` |
| Branch | `p4/s01-storage` (not yet pushed/PR'd) |
| Started | 2026-09-27 |
| Goal of this session | Build as much of Phase 4 as possible without depending on the (currently stopped, to-be-redone) recording |
| Next concrete step | **Recording stopped, will redo fresh next session.** The first attempt (started 2026-09-27 15:55) hit the `resource_and_focus_loop` bug (see §8 tech debt) partway through — its dumps (`.dev-cache/telemetry-raw/dump-20260927-{155501,185501}.bin`, ~4h combined) have real idle/window/mpris/process-tree events but zero real `system_resource` samples. Owner stopped it (laptop shutdown) rather than restart immediately; next session, start clean with the already-fixed release binary: `cargo build --release -p neuroos-monitor` (rebuilds fast, already done once), then `.dev-cache/telemetry-raw/record-loop.sh`. The two old dumps are still on disk if worth keeping as partial data — otherwise `rm .dev-cache/telemetry-raw/dump-2026*.bin` before the fresh run so filenames don't confuse which is complete. Once ≥3 fresh dumps totaling ≥8h exist: anonymise each (`neuroos-monitor anonymize <in> tests/fixtures/telemetry/<name>.bin`), commit, flip §5's Phase 3 row to ✅ Done and `reports/phase-03-monitor.md`'s status to Passed. Meanwhile Phase 4 continues on `p4/s01-storage` for everything not blocked on real dumps (see §4 sprint board) — owner explicitly asked to build as much of Phase 4 as possible without waiting on the recording. Blueprints still need moving to `docs/blueprints/` (deferred since P0-S01, still not done). |

---

## 3. Completed Work Log

Newest first. One line per meaningful unit of work. Format: `YYYY-MM-DD · [Phase/Story] · what was done · evidence/link`.

| Date | Phase / Story | Completed | Evidence |
| :--- | :--- | :--- | :--- |
| 2026-09-27 | P4-S01…S03 | Built the first 3 Phase 4 stories on new branch `p4/s01-storage`: real `neuroos-taint` (TaintFlags + union-only propagate()), the 4-stage ingest filter (PPID collapse, self-observation exclusion, promotion gate — N>=3 focus sessions or >5s dwell, MPRIS demotion), SQLite schema + migration runner (meta.sqlite3, WAL), domain adapters mapping filtered events to 10 of Architecture.md §7.3's 13 domains (the other 3 have non-C1 sources), a real FastEmbed embedder, a real LanceDB vector store (one table per domain family), and `StorageEngine` tying all of it into one real ingest/query pipeline. Live-verified end to end, not mocked: a real focus session's title got embedded and stored, then a real `query_hybrid("revenue numbers")` call found it by vector similarity. Found and fixed 3 real things: fastembed needs 3 more tokenizer files beyond tokenizer.json (config.json/tokenizer_config.json/special_tokens_map.json), fetched and verified for real; `ort` 2.0.0-rc.13 (fastembed's pinned dep) rejects ONNX Runtime < 1.24.x, so re-fetched 1.30.0 instead of the initially-chosen 1.19.2; lancedb 0.38/0.39 both fail to compile with `default-features = false` (an `Error::Http` variant referenced unconditionally in job.rs but `#[cfg(feature="remote")]`-gated in error.rs — a real upstream bug), worked around by pinning lancedb =0.15 (predates that code). Also found and fixed a **pre-existing Phase 3 bug** while debugging the live pipeline test: `neuroos-monitor`'s `resource_and_focus_loop` recreated `sampler` fresh every tick, silently resetting its delta-tracking state so `ResourceSample`/`system_resource` has never actually published a real event since Phase 3 — this means the recording that was already ~4h in when this was found has zero real resource samples; owner stopped it (laptop shutdown) rather than restart immediately, fresh attempt with the now-fixed binary pending next session. | `crates/neuroos-storage/src/{ingest,adapters,sqlite,embed,lance,engine}.rs`; `models/manifest.toml` (ONNX Runtime + tokenizer files) |
| 2026-09-27 | Phase 3 (all stories) | Built all 7 stories from scratch on `p3/s01-monitor`: real typed `telemetry.proto` schema (was a generic-bytes placeholder), `EventBus` (tokio broadcast, bounded+drop-counted), `PrivacyState` (pause+exclusion gate, property-tested), push-event COSMIC toplevel sensor (upgraded from the P0-S08 one-shot spike), `ext_idle_notify_v1` idle sensor, zbus-based MPRIS watcher, `/proc` resource sampler + process-tree snapshot, `notify`-based folder watcher, `monitor.sock` push server + new `monitor.control.sock` request/response channel, `--record`/anonymiser, and `neuroosctl pause/resume/monitor-status`. Verified live end to end against the real reference desktop throughout, not mocks: real WindowOpened events with a real resolved PID (ADR-0009 — found neither `zcosmic_toplevel_info_v1` nor `com.system76.CosmicComp`'s D-Bus interface expose a PID, approximated via `/proc` matching instead), a real MPRIS "Playing" event from VLC round-tripped over a raw socket client, a real `MonitorStatusRequest`/`neuroosctl pause`/`resume` round trip. Found and fixed a real bug via testing: `spawn_blocking`'s infinite dispatch loops (Wayland sensors) hang tokio's multi-thread `Runtime::drop` forever if a test tries to return normally after spawning one — fixed with `std::process::exit(0)` for the (correctly) `#[ignore]`d live tests and a `recv_timeout`-based cooperative-cancellation redesign for `folders.rs`'s own always-on test. Measured PF: capture pipeline p99 = 0.008 ms (budget 1.5 ms; first attempt measured a tight burst and got 9.995 ms — a benchmark methodology bug, not a real one), RSS ≈ 8.9 MiB (budget 25 MiB), idle CPU 0%. Coverage 82.29% line / 89.59% function via `cargo llvm-cov nextest --run-ignored all` (nextest's per-test-process isolation is what makes including the `exit(0)`-using live tests in a coverage run safe). Wrote `reports/phase-03-monitor.md`; 6 of 7 exit criteria met — the ≥8h anonymised real-usage recordings criterion is deliberately not started (needs the owner's real elapsed hours + explicit consent given it captures real window titles/paths/media metadata for hours at a time). | `reports/phase-03-monitor.md`; `docs/adr/0009-toplevel-pid-resolution-heuristic.md` |
| 2026-09-27 | Phase 2 gate | All 6 exit criteria verified with evidence; `reports/phase-02-inference.md` written. Cancellation latency measured (0 tokens after cancel), interactive preemption measured under real background load (4 tokens in 230-453ms while a 400-token background job ran), weights confirmed read-only mmap via `/proc/<pid>/maps` (`r--s`), spike S-05 confirmed `MemoryDenyWriteExecute=true` safe (real `--bench` run under `systemd-run --user`), C++ coverage 75.4% excluding the `--bench` CLI tool (67.5% including it — gcov measurement gap, not a real gap, see report §2.4). ADR-0008 records the FR-INF-07 target miss and the decision to proceed. | `reports/phase-02-inference.md` |
| 2026-09-27 | P2-S05/S06 | Real GBNF grammar test found and fixed a process-crashing bug: llama.cpp's grammar sampler throws on rule completion inside `llama_sampler_accept`, uncaught on a detached worker thread → `std::terminate`. Fixed with try/catch treating the exception as the grammar's own "done" signal (rules.md §8). Real benchmark harness (`neuroos-inference --bench`) measured against the actual model at 128/512/1024 contexts + thread sweep: decode 55.9-72.1 ms/token, prefill 8.25-8.87 ms/token — both miss FR-INF-07's targets (45ms/2.5ms); RSS 1.22-1.33 GiB within the 1,590 MiB budget. | `reports/bench/inference-1790486839.json`; `docs/adr/0008-inference-benchmark-vs-fr-inf-07-targets.md` |
| 2026-09-27 | P2-S01…S04 | Real `neuroos-inference` linking BitNet's own llama.cpp fork directly (not shelling out): `Model`/`Context` (mmap+sha256-verified weights, per-lane KV cache with prefix reuse), two-lane scheduler (`lanes.cpp`, background yields not aborts under interactive load), named memfd ring registry with real SCM_RIGHTS fd handoff. Verified end-to-end against the real downloaded BitNet model: real streamed completions, real cancellation, real preemption under load. Found and fixed 2 more real bugs: a `generation_id` map-key collision (two jobs sharing a ring without an intervening cancel share the same id; fixed with identity-checked erase) and an oversized prompt being silently dropped instead of rejected (fixed: synchronous rejection in `server.cpp` before queuing). | `cpp/neuroos-inference/src/{engine,lanes,ring,server}.cpp`; `tests/contract/cpp_inference_smoke.cpp` |
| 2026-09-27 | Phase 2 prereq | `libneuroos` gained real C++ IPC infra it didn't have (framing/UDS/SO_PEERCRED, SCM_RIGHTS fd passing, health server, OpenSSL sha256, paths helpers) — Phase 0 only built the memfd ring in C++. Found and fixed a real bug via testing: `read_envelope_with_fd` read the 4-byte length prefix with a plain `recv()` before switching to `recvmsg()`, silently dropping the SCM_RIGHTS fd (Linux only delivers ancillary data to the `recvmsg()` call that reads the accompanying bytes). Also found and fixed a real shutdown segfault: `std::exit(0)` on SIGTERM raced static destructors against still-live detached worker threads; reverted to `std::quick_exit(0)`. | `cpp/libneuroos/src/{framing,uds,health_server,sha256,paths}.cpp`; `tests/contract/cpp_ipc_smoke.cpp` |
| 2026-09-27 | P1-S04 | `neuroosctl status` implemented for real (was a one-line stub left over from the prior session's WIP commit despite `neuroos-health::percentile` already having been moved out specifically to support it). Subcommand `status` connects to `healthd.sock`, sends `AggregateStatusRequest`, prints a text table or (`--json`) JSON; percentiles computed via the shared `neuroos_health::{p50_ns,p99_ns}` from whichever named latency histogram sorts first (deterministic pick). Manually verified end to end against a real `neuroos-healthd` + 8-mock `health_mock_farm`. 5 new unit/integration tests. | `crates/neuroosctl/src/main.rs` |
| 2026-09-27 | Phase 1 gate | Fixed `just ci` (was red: 2 clippy `collapsible_if` errors in `neuroos-testkit/src/health_mocks.rs`, 1 unused import, fmt drift — all left over from the prior session's WIP commit that was never run through `just ci`). Wired the orphaned `tests/contract/healthd_pf.sh` PF test into `justfile` (`just test-healthd-pf`, included in `just test`) — proved real RSS 9.4 MiB (budget ≤ 15 MiB) with 9 targets. Measured coverage 84.99% region / 83.35% line across `neuroos-healthd`+`neuroosctl` (≥ 80% bar). Verified soak breach math against fixed vectors and against a real running `neuroos-healthd --soak` process. Wrote `reports/phase-01-healthd.md`; all P1 exit criteria met, no owner sign-off items (unlike Phase 0). | `reports/phase-01-healthd.md`; `just ci` green, 77 tests |
| 2026-09-26 | P0-S10 | Real `deny.toml`: license allowlist (rules.md §4), network-crate ban (`reqwest`/`hyper`/`ureq`/`curl`/`hickory-resolver`/etc., `wrappers = ["neuroos-fetcher"]` — only the fetcher may ever depend on these) wired into `just ci` for the first time. **Found two real issues wiring it for real**: `cosmic-protocols` (P0-S08's COSMIC dependency) is GPL-3.0-only (new exception, ADR-0007); internal workspace path deps needed explicit `version = "0.1.0"` to satisfy the wildcard-dependency check. Proved the ban actually fires: added `reqwest` to a non-fetcher crate as a throwaway test → real `error[banned]` for both `reqwest` and its transitive `hyper-util`, reverted. | `docs/adr/0007-cosmic-protocols-gpl-exception.md` |
| 2026-09-26 | P0-S09 | `models/manifest.toml`: all 10 real models Architecture.md §7.1 names (BitNet, whisper-tiny.en-q5_1, Silero VAD, 3× openWakeWord, Kokoro int8 + one voice, bge-small-en-v1.5 + tokenizer), with real URLs and real sha256 hashes computed from actual downloads. `scripts/fetch-models.sh` (Python-in-bash, stdlib `tomllib`) downloads, verifies, is idempotent (skips already-correct files), and **proven to reject bad data**: tested both a corrupted cached file (silently re-fetched and fixed) and a deliberately-wrong manifest hash (fetch aborted, no file installed, non-zero exit). Wired as `just fetch-models` (not in `just ci` — several GB) and `just lint-manifest` (schema check, no network, in `just ci`). | commit on `p0/s01-just-ci-green` |
| 2026-09-26 | P0-S08 | Spike S-03: `zcosmic_toplevel_info_v1` proven live against this machine's real COSMIC session (5 real windows correctly listed: app_id/title/Activated state). Ported into `crates/neuroos-monitor/src/sensors/wayland_cosmic.rs` as real code (`list_toplevels()`), not a stub — 2 unit tests + 1 `#[ignore]`d live-session integration test (passed for real). Spike S-04: added `microsoft/BitNet` as a pinned git submodule (`cpp/third_party/bitnet.cpp`, nested `3rdparty/llama.cpp` pinned too), downloaded the real `BitNet-b1.58-2B-4T-gguf` i2_s model (~1.1 GiB), built with AVX2 confirmed active (`-march=native` → `__AVX2__`). Real throughput: 17.89 t/s decode / 17.07 t/s prefill, 8 threads (R-02 first signal). Also fixed `justfile`'s cpp find/lint/fmt recipes to exclude `cpp/third_party` (were sweeping the vendored submodule source). | `docs/adr/0006-cosmic-toplevel-and-bitnet-spikes.md` |
| 2026-09-26 | P0-S07 | Spike S-02: memfd seqlock ring in Rust (`crates/neuroos-shm`) and C++ (`cpp/libneuroos/.../shm_ring.hpp`), matching layouts. Found and fixed two real bugs via testing (see ADR-0005, D-12): (1) seqlock alone doesn't prove slot identity — added an explicit `seq` field to the slot; (2) plain non-atomic field access is UB under the memory model even though the seqlock makes it hardware-safe — every field now goes through `Relaxed` atomics. Proven: 10M messages zero-corruption (release), zero ThreadSanitizer races on both Rust (nightly) and C++ (`tsan` CMake preset) after the fix, cross-language interop both directions (`tests/contract/shm_interop.sh`, in `just ci` via `just test-shm`). | `docs/adr/0005-shm-ring-race-freedom.md`; commit on `p0/s01-just-ci-green` |
| 2026-09-26 | P0-S06 | Spike S-01 (unit mode, risk R-01) run for real on the reference machine: `systemd-run --user -p PrivateNetwork=yes` (no sudo) and, with the owner's help, `sudo systemd-run --uid=... -p PrivateNetwork=yes` (system-scope). Both work; user units need no manual env wiring and correctly report `SO_PEERCRED`. Wrote ADR-0001 (meta), ADR-0002 (unit mode, reverses Architecture.md §8.1's stated preference — see D-11), ADR-0003 (IPC transport, documents D-04), ADR-0004 (build order, documents D-02/D-03). | `docs/adr/0001-*.md` … `0004-*.md` |
| 2026-09-26 | P0-S05 | Filled in all `deploy/systemd/*` unit templates (hardening baseline, per-component `MemoryMax`/`BindPaths` from PRD §6.2 / Architecture §8.2), `sysusers.d`, `tmpfiles.d`. Wrote `docs/threat-model.md` v0 (STRIDE per trust boundary). Implemented `scripts/check-egress.sh` for real using unprivileged `unshare --net --mount` (no sudo needed). **Found a real gap**: `PrivateNetwork=true` alone doesn't block DNS (systemd-resolved's NSS module uses a local socket); fixed with `InaccessiblePaths=-/run/systemd/resolve` on every unit except the fetcher, documented as an Architecture.md §8.1 addendum. Added `just lint-systemd` (systemd-analyze verify) and `just test-security`. | commit on `p0/s01-just-ci-green`; `Architecture.md` §8.1 amended |
| 2026-09-26 | P0-S04 | `neuroos-health`: latency histogram (log-scale ns buckets, matches `LatencyHistogram` proto), `/proc/self/status` RSS reader, `HealthServer` serving `HealthRequest`→`HealthResponse` over UDS (built on neuroos-ipc). Wired into `neuroos-monitor` main.rs as the "one line" proof: `tokio::spawn(health.serve(path, uids))`. 94.4% line coverage. | commit on `p0/s01-just-ci-green` |
| 2026-09-26 | P0-S03 | `neuroos-ipc`: framing (u32-LE length prefix), UDS server/client, `SO_PEERCRED` allowlist check, connect/read/write deadlines, reconnect-with-backoff (10s budget). 16 tests (unit, proptest, 2 real-UDS integration: echo + reconnect-after-restart). 87.5% line / 88% region coverage. Branch coverage needs nightly rustc (cargo-llvm-cov `--branch`) — deferred, see §8 tech debt. | commit on `p0/s01-just-ci-green` |
| 2026-09-26 | P0-S02 | proto v1 contracts (envelope, common, health, first-cut per-component messages) + codegen for Rust (prost)/C++ (protoc+CMake)/Python (protoc). Cross-language round-trip test (`tests/contract/roundtrip.sh`) proves Rust→C++→Python→Rust byte-identical encoding. | commit `402f312` on `p0/s01-just-ci-green` |
| 2026-09-26 | P0-S01 | `just ci` (fmt-check, lint, build, test across Rust/C++/Python) green on clean checkout; `just build`/`just test`/`just bench` also pass. Rust workspace (16 crates, toolchain 1.97.1 pinned), cpp CMake build (libneuroos, neuroos-inference exe, neuroos-voice lib), python uv project all wired. cargo-deny deferred to P0-S10 (kept as separate `just deny` recipe, not in `ci`). | commit `c48b5fc` on branch `p0/s01-just-ci-green` |
| 2026-09-26 | Phase 0 | Full repo folder/file tree scaffolded per Architecture.md §14 (empty stubs, one-line comments); `.obsidian/`, reference mp4/pngs gitignored. | commit `981a4ba` on `main` |
| 2026-09-26 | Planning | Voice redesign: single Kokoro voice, Piper removed, conversation mode added (PRD FR-VOI-05/06/09–12, Architecture §13 #16, design §9.2, phases P6). | PRD.md, Architecture.md, design.md, phases.md |
| 2026-09-26 | Planning | Created the 6 foundation documents (PRD, Architecture, rules, phases, design, memory) from blueprints V3.2 + V2.7, and the `reports/` folder with README and template. | `PRD.md`, `Architecture.md`, `rules.md`, `phases.md`, `design.md`, `memory.md`, `reports/` |

---

## 4. Current Sprint Board

**Sprint 1 (2026-09-26 → 2026-09-27) — done:** healthd core, cgroup reader, soak engine, `neuroosctl status`. All 4 P1 stories done, Phase 1 gated (`reports/phase-01-healthd.md`).

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P1-S01 | healthd scrapes every configured health socket every 30 s, marks unreachable ones DOWN | 5 | Done |
| P1-S02 | Operator sees each unit's cgroup memory against its budget | 3 | Done |
| P1-S03 | Soak mode flags RSS growth > 5% or p99 drift > 10% | 5 | Done |
| P1-S04 | `neuroosctl status` shows the aggregate report | 3 | Done |

**Sprint 2 (2026-09-27) — done:** real `neuroos-inference` linking BitNet's llama.cpp fork, all 6 P2 stories done, Phase 2 gated (`reports/phase-02-inference.md`).

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P2-S01 | Send a prompt, receive a streamed completion | 8 | Done |
| P2-S02 | Generated text through a zero-copy shared-memory ring | 5 | Done |
| P2-S03 | Cancel a generation, stops within one decode step | 3 | Done |
| P2-S04 | Background distillation never delays interactive by more than one decode step | 5 | Done |
| P2-S05 | Force JSON output matching a grammar | 3 | Done |
| P2-S06 | Measured TTFT/prefill/decode numbers replacing blueprint projections | 5 | Done |

**Sprint 3 (2026-09-27) — code done, gate pending:** Phase 3 — Desktop Telemetry Monitor (C1 `neuroos-monitor`), per phases.md §6. All 7 stories done; the phase's ≥8h real-recording exit criterion is separate from story completion — see `reports/phase-03-monitor.md` §6.

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P3-S01 | Focus changes with app_id, title, PID, UTC-ns timestamps | 8 | Done |
| P3-S02 | Idle/active transitions | 2 | Done |
| P3-S03 | MPRIS playback events | 3 | Done |
| P3-S04 | Resource samples + process tree snapshot | 5 | Done |
| P3-S05 | Excluded apps and paused periods never leave C1 | 3 | Done |
| P3-S06 | Record and replay a telemetry dump | 3 | Done |
| P3-S07 | File activity for configured git/notes/ICS folders | 5 | Done |

**Sprint 4 (2026-09-27, in progress) — Phase 4 — Semantic Storage Engine (C3 `neuroos-storage`), per phases.md §7, branch `p4/s01-storage`.** Owner asked to build everything not blocked on the ≥8h recording (P4's soak-replay gate) while a fresh recording attempt is pending next session.

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P4-S01 | Noisy events never become persistent nodes (4-stage ingest filter) | 8 | Done |
| P4-S02 | Events stored in the right domain with the right taint | 5 | Done |
| P4-S03 | Top-k relevant chunks for a question in ~13ms (embedder) | 8 | Done (embedder + LanceDB + hybrid query all real and live-verified; latency not yet benchmarked) |
| P4-S04 | Window focused at a given moment ± 1.5s (`QueryFocusHistory`) | 3 | Backlog (SQLite `focus_history` table + rows already exist from P4-S02; the read-query API itself not yet written) |
| P4-S05 | Slow collection auto-promotes to HNSW | 5 | Backlog (P1) |
| P4-S06 | Fetched documents ingested as untrusted (`external_documents`, C7 spool) | 3 | Backlog |
| P4-S07 | Old data expires and backups exist (lifecycle: GC, 6-hourly backup) | 5 | Backlog |
| P4-S08 | Model upgrade re-indexes without downtime | 5 | Backlog |
| P4-S09 | Forget a time range or an app (FR-STO-12/FR-PRV-03) | 3 | Backlog |
| P4-S10 | (Conditional) SQLite v1 migrator | 5 | Dropped — OQ-03's default ("drop unless a sample DB is provided") applies; no sample DB provided |

Columns: Backlog → Ready → In Progress → In Review → Testing → Done.

---

## 5. Phase Tracker

| Phase | Name | Status | Started | Finished | Report |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 0 | Foundation, Contracts & Spikes | ✅ Done (owner signed off 2026-09-27 on the 4 flagged deviations — report §2.6/§4) | 2026-09-26 | 2026-09-27 | `reports/phase-00-foundation.md` |
| 1 | Health Aggregator (healthd) | ✅ Done (all exit criteria met, no owner sign-off items) | 2026-09-26 | 2026-09-27 | `reports/phase-01-healthd.md` |
| 2 | Neural Inference Engine (C4) | ✅ Done (all 7 exit criteria met) | 2026-09-27 | 2026-09-27 | `reports/phase-02-inference.md` |
| 3 | Desktop Telemetry Monitor (C1) | 🟨 Merged (PR #4); done except the recording — all 7 stories done, 6/7 exit criteria met with evidence. First recording attempt stopped mid-way (found a real bug, see §8); a fresh attempt with the fixed binary is pending next session | 2026-09-27 | — | `reports/phase-03-monitor.md` |
| 4 | Semantic Storage Engine (C3) | 🟦 In progress — P4-S01…S03 done on `p4/s01-storage` (ingest filter, domain adapters+taint, real embedder+LanceDB+hybrid query); soak-replay gate blocked on the recording | 2026-09-27 | — | `reports/phase-04-storage.md` |
| 5 | Knowledge Engine (C5) | ⬜ Not started | — | — | `reports/phase-05-knowledge.md` |
| 6 | Voice & Audio Pipeline (C2) | ⬜ Not started | — | — | `reports/phase-06-voice.md` |
| 7 | SafetyGate Kernel (C6) | ⬜ Not started | — | — | `reports/phase-07-kernel.md` |
| 8 | External Fetcher (C7) | ⬜ Not started | — | — | `reports/phase-08-fetcher.md` |
| 9 | System Integration & 24 h Soak | ⬜ Not started | — | — | `reports/phase-09-integration-soak.md` |
| 10 | Hardening, Packaging & v1.0 | ⬜ Not started | — | — | `reports/phase-10-release.md` |

Status legend: ⬜ Not started · 🟦 In progress · 🟨 In gate review · ✅ Done · 🟥 Blocked

---

## 6. Decisions Log

Short record of decisions. Anything architectural also gets an ADR in `docs/adr/`.

| Date | ID | Decision | Rationale | ADR |
| :--- | :--- | :--- | :--- | :--- |
| 2026-09-27 | D-18 | Focused-toplevel PID is resolved by matching `app_id` against `/proc`'s process list, not a protocol query. | Verified live: neither `zcosmic_toplevel_info_v1` nor `com.system76.CosmicComp`'s D-Bus interface expose a PID; no Wayland compositor protocol does, by design (client sandboxing). | ADR-0009 |
| 2026-09-27 | D-17 | Coverage measurements that need to include a live-desktop `#[ignore]`d test use `cargo llvm-cov nextest --run-ignored all`, not plain `cargo llvm-cov` / `cargo test`. | Some live tests must call `std::process::exit(0)` at the end (their sensor blocks an OS thread forever by design; tokio's multi-thread `Runtime::drop` would otherwise hang waiting for it) — under `cargo test`'s single-process-per-binary harness that kills every sibling test sharing the process. `cargo-nextest` isolates each test into its own process, so it's immune; it's also already what `just test-rust` uses. | — |
| 2026-09-27 | D-16 | `monitor.control.sock` (new, C1↔`neuroosctl`) carries pause/resume/status request-response, separate from `monitor.sock`. | `monitor.sock` is documented (Architecture.md §5.2) as a pure server-push pattern to C3; FR-PRV-01's `neuroosctl pause` needs request/response, so it got its own socket rather than mixing patterns on one. | — |
| 2026-09-27 | D-15 | Proceed to Phase 3 without a hardware/model change despite FR-INF-07's decode (45ms/tok) and prefill (2.5ms/tok) targets being missed (measured 55.9-72.1ms/tok decode, 8.25-8.87ms/tok prefill). | Cold-context worst-case benchmark; real per-turn cost is lower once KV-cache prefix reuse applies on the second+ request per lane; a full spoken sentence is still sub-1.5s of decode, covered by Phase 6's preamble design; 8 threads already near-saturates this CPU. | ADR-0008 |
| 2026-09-27 | D-14 | `Context::generate`'s KV-cache reuse compares the new prompt's tokens against whatever's resident from the lane's last call and only re-decodes the differing suffix, rather than hardcoding a 128-token system-prompt boundary. | Generalizes Architecture.md §9.1's "128-token system block" lever to whatever prefix actually repeats between consecutive requests on a lane — works correctly regardless of whether the system prompt is exactly 128 tokens. | — |
| 2026-09-26 | D-01 | V3.2 blueprint is primary; conflicts resolved per Architecture.md §13. | V3.2 is the newer, more detailed specification. | ADR-0001 |
| 2026-09-26 | D-02 | healthd is a standalone binary, built first. | Out-of-process resilience (V3.2). | ADR-0004 |
| 2026-09-26 | D-03 | Build C1 (monitor) before C3 (storage). | The Phase 4 soak-replay gate needs real captured telemetry dumps. | ADR-0004 |
| 2026-09-26 | D-04 | IPC = filesystem UDS + u32-LE length-prefixed protobuf; memfd + SCM_RIGHTS for the token ring. | Works under PrivateNetwork/PrivateDevices; one contract for 3 languages. | ADR-0003 |
| 2026-09-26 | D-13 | `cosmic-protocols` gets a `deny.toml` license exception for GPL-3.0-only. | It's the only binding for `zcosmic_toplevel_info_v1`; no non-GPL alternative exists. Fine for a personal, undistributed install (OQ-06); revisit if distribution posture changes. | ADR-0007 |
| 2026-09-26 | D-12 | memfd ring slot layout extends Architecture.md §5.5 with an explicit `seq: u64` field; every slot field (including payload bytes) accessed via `Relaxed` atomics, not plain reads/writes. `slot_size` must be a multiple of 8. | Both found as real bugs via testing (stress test + ThreadSanitizer) during spike S-02 (P0-S07) — see ADR-0005 for the full story. | ADR-0005 |
| 2026-09-26 | D-11 | **Reverses Architecture.md §8.1's stated preference:** use **user-scope systemd units** (`systemctl --user`), not system template units with `User=%i`, for every Zone 2/3 component. | Spike S-01 (P0-S06): on the reference machine, `PrivateNetwork=true` works fine in a user unit (contradicts the assumed Ubuntu 24.04 restriction); user units auto-inherit `XDG_RUNTIME_DIR`/`WAYLAND_DISPLAY`/`DBUS_SESSION_BUS_ADDRESS` correctly, system units don't (no reliable way to discover `WAYLAND_DISPLAY`); `SO_PEERCRED` reports the real UID either way. | ADR-0002 |
| 2026-09-26 | D-05 | Fetcher → storage notification via inotify on the spool dir. | Fetcher UID cannot reach the user runtime dir. | — |
| 2026-09-26 | D-06 | Token ring carries token_id + detokenized UTF-8 piece. | C2 has no tokenizer. | — |
| 2026-09-26 | D-07 | Rust edition 2024 (toolchain 1.97.x). | Current stable toolchain on the reference machine. | — |
| 2026-09-26 | D-08 | 1-week sprints, 11 phases (0–10), ≈ 22 sprints to v1.0. | AI-assisted pace, component-by-component delivery. | — |
| 2026-09-26 | D-10 | Every hardened unit (except `neuroos-fetcher.service`) adds `InaccessiblePaths=-/run/systemd/resolve` to its baseline, beyond the Architecture.md §8.1 block as originally written. | `PrivateNetwork=true` alone does not block DNS; `resolve` NSS module bypasses the netns via a local socket. Found empirically, verified fix with `unshare`. | — |
| 2026-09-26 | D-09 | Remove Piper. One Kokoro-82M voice for all speech; preambles and status lines are pre-rendered Kokoro clips cached in memory; 8 s follow-up conversation window. Voice RSS 485 → 405 MiB, total 2,420 → 2,340 MiB. | Owner wants one consistent, human-quality voice; noticed tone differences between engines. | — |

---

## 7. Open Questions

Mirror of [PRD.md](PRD.md) §12. Close here and in the PRD at the same time.

| ID | Question | Default | Needed by | Status |
| :--- | :--- | :--- | :--- | :--- |
| OQ-01 | Wake phrase ("hey jarvis" or custom)? | "hey jarvis" | Phase 6 | Open |
| OQ-02 | Final list of 13 domain adapters? | Architecture.md §7.3 proposal | Phase 4 | Open |
| OQ-03 | Does a NeuroOS v1 SQLite DB exist to migrate? | Drop migrator unless a sample DB is provided | Phase 4 | Open |
| OQ-04 | Neural (PyTorch) or parametric Hawkes? | Parametric | Phase 5 | Open |
| OQ-05 | Calendar source: ICS files or Evolution Data Server? | ICS | Phase 7 | Open |
| OQ-06 | Personal install or public distribution? | Personal | Phase 10 | Open |
| OQ-07 | Notes vault path? | `~/Notes` (configurable) | Phase 3 | Open |

---

## 8. Blockers, Risks to Watch, Known Issues

| Date | Type | Item | Owner | Status |
| :--- | :--- | :--- | :--- | :--- |
| 2026-09-26 | Risk | R-01: `PrivateNetwork=true` in systemd user units vs Ubuntu 24.04 userns restriction and `SO_PEERCRED` under `PrivateUsers`. | Architect | **Resolved** — spike S-01 (P0-S06, ADR-0002): premise didn't hold on the reference machine; user units work and are now the recommended mode (D-11). |
| 2026-09-26 | Risk | R-02: BitNet decode speed on Zen 3 unverified. | Architect | **Resolved** — Phase 2's own real benchmark (ADR-0008): 55.9-72.1 ms/token decode, 8.25-8.87 ms/token prefill, 8 threads. Misses FR-INF-07's targets (45ms/2.5ms); ADR-0008 records the decision to proceed anyway (real per-turn cost is lower with KV-cache reuse; sub-1.5s sentences covered by Phase 6's preamble design). |

Tech debt register (add as it appears):

| Date | Item | Introduced in | Plan to repay |
| :--- | :--- | :--- | :--- |
| 2026-09-27 | **Real bug, found and fixed on `p4/s01-storage`**: `neuroos-monitor`'s `resource_and_focus_loop` reassigned `sampler` to a fresh `ResourceSampler` every tick (to satisfy `spawn_blocking`'s `move` closure), which reset its delta-tracking state every time — `sample()` always saw "first call" and returned `None`, so FR-MON-04's `ResourceSample`/`system_resource` domain has **never actually published a real event**, live or in tests, since Phase 3 was built and merged (PR #4, already on `main`). Fixed by reading `/proc` directly instead of via `spawn_blocking` (small/fast enough not to need it), keeping `sampler` borrowed in place across ticks. **Consequence:** the background telemetry recording running since 2026-09-27 15:55 (`.dev-cache/telemetry-raw/`, for Phase 3's §6.4 exit criterion) was started from the *pre-fix* binary — its dumps have real idle/window/mpris/process-tree events but zero real `system_resource` samples. Owner needs to decide: accept the gap in these fixtures, or restart recording with the fixed release binary (`cargo build --release -p neuroos-monitor`, rebuilt already). | Phase 3 (P3 main.rs), found on Phase 4 branch | Owner decision pending (see above); the code fix itself is already committed |
| 2026-09-27 | wlroots (`zwlr_foreign_toplevel_manager_v1`) fallback sensor not built — C1 only gets window-focus events on COSMIC; a Sway/other-wlroots install would get none. Tagged P1 in phases.md §6.1, and untestable on this reference machine (COSMIC-only). | P3 | whenever a non-COSMIC reference machine is available, or if OQ-06 changes |
| 2026-09-27 | `neuroos-monitor@.service`'s `ProtectHome` was changed to `read-only` (from the shared `tmpfs` baseline) for the folder sensor's sake, but the unit is still system-template style (`User=%i`) like every other component — same ADR-0002 debt row below, now also true of C1's unit. | P0-S05, still open at end of P3 | next session touching `deploy/systemd/` |
| 2026-09-27 | `neuroos-monitor`'s `async fn main()` (the `tokio::spawn` orchestration wiring) is ~46% line-covered — proven correct by live manual verification this session (real sockets, real events, real `neuroosctl` round trips), not by an automated test, since that requires running the compiled binary as a subprocess rather than calling functions in-process. | P3 | only if `main.rs` grows real logic beyond wiring |
| 2026-09-26 | `deploy/systemd/*.service` (Zone 2/3 components) are still system templates (`User=%i`) from P0-S05; ADR-0002 (P0-S06) recommends reworking them to user-scope units instead. Mechanical rework (drop `User=`/`Group=`/`neuroos@%i.target` plumbing, retarget `WantedBy=`), not new design. | P0-S05, superseded by P0-S06/ADR-0002 | next session touching `deploy/systemd/` |
| 2026-09-26 | `deny.toml` empty stub; `cargo deny check` not wired into `just ci` (license/bans policy undefined, only `just deny` exists standalone). | P0-S01 | P0-S10 |
| 2026-09-26 | System has `clang-format-18`/`clang-tidy` (no unversioned `clang-format` alias); justfile calls `clang-format-18` explicitly. `cargo-nextest` and `shellcheck` installed manually this session (were missing from env, see memory.md §10). | P0-S01 | none needed — document only |
| 2026-09-26 | All 10 real models (1.4 GiB total) fetched and verified into `.dev-cache/models/` (gitignored, inside the repo root — moved here from an initial out-of-repo path per owner's instruction: never store large files outside the project root) via `NEUROOS_MODELS_DIR` override — not `/opt/neuroos/models` (needs root, not done this session). Kokoro voice choice (`af_heart`) is this manifest's pick, not an OQ/ADR decision — revisit if the owner wants a different default voice. | P0-S09 | `scripts/install.sh` (not yet written) does the real `/opt/neuroos/models` install; revisit voice choice whenever voice UX is actually designed (Phase 6) |
| 2026-09-26 | `cpp/third_party/bitnet.cpp` submodule needs `git submodule update --init --recursive` after a fresh clone (not automatic, not yet documented in a README quick-start — README.md itself predates this and is still a P0-S01 stub). Its `build/` (807M) and `models/` (1.2G, the real downloaded GGUF) are untracked, left in place per owner's choice this session. ~~Not wired into `cpp/CMakeLists.txt` or `just build` yet — Phase 2 does that.~~ **Repaid in Phase 2**: `neuroos-inference/CMakeLists.txt` now `add_subdirectory`s `3rdparty/llama.cpp` directly (not bitnet.cpp's own top CMakeLists.txt, to avoid its forced `LLAMA_BUILD_SERVER=ON`). | P0-S08 | note in README when it's written for real |
| 2026-09-27 | `cpp/libneuroos/include/libneuroos/expected.hpp` is a small hand-written `Expected<T,E>`, not the real `tl::expected` (Architecture.md's file tree names it as "vendored") — this dev environment has no outbound network access to fetch it. Covers every call site this codebase uses today (construct/check/read); no monadic `and_then`/`map` chaining. | P2 (prerequisite infra) | revisit only if a future call site needs monadic chaining the current type can't do |
| 2026-09-27 | `neuroos-inference/src/bench.cpp`'s coverage isn't measurable with this session's ad-hoc `gcov` setup: the `--bench` CLI mode only flushes coverage counters on a normal `return` from `main`, and this session's own timeout-based test harness kept killing it mid-sweep on the -O0 coverage build (real runs take minutes at that speed). Functionally proven via committed `reports/bench/*.json` from the real optimized build; only the coverage *report* has a gap. | P2-S06 | add a `SIGTERM` handler to `--bench` mode if per-phase coverage measurement becomes routine; or switch to `gcovr` (see also the lesson in `reports/phase-02-inference.md` §5 about `libneuroos`'s static-lib coverage not merging cleanly across binaries with raw `gcov`) |
| 2026-09-27 | `deploy/systemd/neuroos-inference@.service` is still the system-template style (`User=%i`) ADR-0002 recommends reworking to a user-scope unit — same pre-existing debt as `neuroos-healthd@.service` (row above), now also true of C4's unit. `MemoryDenyWriteExecute` was flipped to `true` this phase (spike S-05, verified safe) but the unit-mode rework itself wasn't in Phase 2's scope. | P0-S05, still open at end of P2 | next session touching `deploy/systemd/` |
| 2026-09-26 | Rust ThreadSanitizer runs (used to verify `neuroos-shm`, ADR-0005) need a local `nightly` toolchain + `rust-src` component, installed this session but not part of the pinned `rust-toolchain.toml` (D-07) or `just ci`. C++ TSan (`tsan` CMake preset) needed `setarch $(uname -m) -R` to work around an unrelated ASLR/mmap-placement TSan issue on this machine. | P0-S07 | document only; re-derive the exact commands from ADR-0005 if `neuroos-shm`'s concurrency logic changes |
| 2026-09-26 | `neuroos-ipc` branch coverage not measured: `cargo llvm-cov --branch` needs `-Z coverage-options=branch`, nightly-only. Line/region coverage (87.5%/88%) already exceeds the 80% bar. | P0-S03 | install/pin a nightly toolchain for coverage only, or accept line coverage as the working proxy — owner to decide |
| 2026-09-26 | Real cross-UID `SO_PEERCRED` rejection (IT: `sudo -u nobody` or second local user) not exercised — sandbox has no second UID/root. `peercred::is_allowed` decision function is unit-tested directly instead. | P0-S03 | exercise for real during Phase 0 hardening pass, if a suitable CI runner is available |
| 2026-09-26 | No C++ tests/executable entry point yet for `neuroos-voice` (only a static lib; Architecture.md §14 lists no `main.cpp` for it). ~~`neuroos-inference` has a stub `main.cpp` returning 0.~~ **Repaid in Phase 2**: `neuroos-inference` is a real service now. | P0-S01 | Phase 6 (voice) |

---

## 9. Retrospective Notes

| Sprint | Went well | To improve | Action |
| :--- | :--- | :--- | :--- |
| — | — | — | — |

---

## 10. Environment Facts (reference machine)

| Item | Value (verified 2026-09-26) |
| :--- | :--- |
| CPU | AMD Ryzen 7 5800H, 8C/16T, AVX2 |
| RAM | 15 GiB |
| OS | Pop!_OS 24.04 LTS, COSMIC on Wayland |
| Toolchains | rustc/cargo 1.97.1, CMake 3.28.3, GCC 13.3.0, clang-tidy 18.1.3, clang-format-18 18.1.3, Python 3.12.3, uv 0.12.1, just 1.42.4, protoc 3.21.12 (libprotoc), cargo-deny 0.20.2, cargo-nextest, shellcheck 0.9.0 (all verified 2026-09-26, P0-S01) |
| C++ deps (Phase 2, 2026-09-27) | `libspdlog-dev` 1.12.0, `libtoml11-dev` 3.8.1, `libgtest-dev` 1.14.0 (installed via apt, owner ran `sudo apt-get install`; gtest ended up unused — this codebase's C++ test convention is plain standalone binaries, see `tests/contract/cpp_*_smoke.cpp`), `libssl-dev` (already present, used for OpenSSL EVP sha256), `gcov`/`llvm-cov-18` (already present, used for the one-off C++ coverage measurement) |
| Audio | PipeWire present |
| Not yet installed / checked | ONNX Runtime, `libpipewire-0.3-dev`, `espeak-ng` |

---

## 11. Session Log

Newest first. One entry per work session.

| Date | Session summary | Stories touched | Next step |
| :--- | :--- | :--- | :--- |
| 2026-09-27 | Built all of Phase 3 (Desktop Telemetry Monitor, C1) from scratch on new branch `p3/s01-monitor`, all 7 stories. Real typed `telemetry.proto` schema replacing the Phase-0 placeholder; `EventBus` (tokio broadcast, bounded+drop-counted), `PrivacyState` (pause+exclusion gate, proptested); push-event COSMIC toplevel sensor (upgrading the P0-S08 one-shot spike), `ext_idle_notify_v1` idle sensor, zbus MPRIS watcher, `/proc` resource+process-tree sampler, `notify`-based folder watcher; `monitor.sock` push server + new `monitor.control.sock` request/response channel; `--record`/anonymiser; `neuroosctl pause/resume/monitor-status`. Verified everything live against the real reference desktop, not mocks: real WindowOpened with a real resolved PID, a real MPRIS "Playing" event from VLC read over a raw socket client, real `neuroosctl` pause/resume/status round trips. Found and fixed 2 real things via testing: no Wayland/D-Bus interface on this machine exposes a toplevel's PID (ADR-0009, PID now a `/proc`-match heuristic), and `spawn_blocking`'s infinite dispatch loops hang tokio's multi-thread `Runtime::drop` on teardown (fixed with `std::process::exit(0)` in the correctly-`#[ignore]`d live tests, and a proper `recv_timeout`-based cooperative-cancellation redesign for `folders.rs`'s own always-on test). PF: capture pipeline p99 0.008ms (budget 1.5ms — first attempt measured a bursty-publish artifact at 9.995ms, fixed the benchmark itself), RSS ~8.9 MiB (budget 25 MiB), idle CPU 0%. Coverage 82.29% line via `cargo llvm-cov nextest --run-ignored all` (plain `cargo llvm-cov`/`cargo test` can't safely include the `exit(0)`-using live tests; nextest's per-test-process isolation can). Wrote `reports/phase-03-monitor.md`; 6 of 7 exit criteria met. Owner chose to start the ≥8h real-usage recording as a background process now (`.dev-cache/telemetry-raw/record-loop.sh`) rather than record it manually later — everything else in Phase 3 is complete. Opened a PR for `p3/s01-monitor` → `main` (Phase 1's and Phase 2's branches were already merged via PRs #2/#3, contrary to this file's earlier stale note). | P3-S01…S07 | Once ≥3 anonymised dumps totaling ≥8h are committed (`.dev-cache/telemetry-raw/dump-*.bin` → `neuroos-monitor anonymize` → `tests/fixtures/telemetry/`), flip Phase 3's row to ✅ Done and start Phase 4 (Semantic Storage Engine, C3). |
| 2026-09-27 | Built all of Phase 2 (Neural Inference Engine, C4) from scratch on new branch `p2/s01-inference`: real `neuroos-inference` linking BitNet's own llama.cpp fork directly (not shelling out), `Model`/`Context` (mmap+sha256-verified weights, per-lane KV cache with generalized prefix reuse), two-lane scheduler (background yields not aborts), named memfd ring registry with real SCM_RIGHTS handoff, GBNF grammar, inference.sock dispatch. First built prerequisite C++ IPC infra in `libneuroos` (framing/UDS/SO_PEERCRED/SCM_RIGHTS/health-server/sha256/paths — Phase 0 only had the memfd ring in C++). Verified everything end to end against the real downloaded BitNet model, not mocks: real streamed completions, measured cancellation latency (0 tokens after cancel), measured interactive preemption under a real 400-token background load, real GBNF-constrained output, real sha256/corrupt-model FI cases. Found and fixed 4 real bugs via testing: SCM_RIGHTS fd silently dropped (recv vs recvmsg ordering), a `generation_id` map-key collision erasing a different job's cancel flag, a grammar-completion crash in llama.cpp's sampler (uncaught exception on a worker thread), and a shutdown segfault (introduced by this session's own coverage-measurement attempt, then reverted). Ran a real benchmark (`--bench`) against the model: decode 55.9-72.1ms/tok, prefill 8.25-8.87ms/tok, both missing FR-INF-07's targets — wrote ADR-0008 recording the decision to proceed anyway. Measured C++ coverage via a new `gcov`-based CMake preset: 75.4% of actual service code. Verified `MemoryDenyWriteExecute=true` is safe (spike S-05) and flipped the systemd unit. Wrote `reports/phase-02-inference.md`; all 7 exit criteria met. | P2-S01…S06 | Start Phase 3 (Desktop Telemetry Monitor, C1); owner still needs to review/merge `p2/s01-inference`. |
| 2026-09-27 | Picked up Phase 1 from a prior session's WIP commit (healthd core: scrape loop, cgroup reader, soak engine, `healthd.sock` server — all real, well-tested — but `just ci` was red and `neuroosctl status` (P1-S04) was still a one-line stub). Fixed `just ci` (3 clippy `collapsible_if` errors, 1 unused import, fmt drift). Implemented `neuroosctl status` (text table + `--json`) for real, with 5 new tests, verified manually end to end against a live healthd + mock farm. Wired the orphaned `tests/contract/healthd_pf.sh` PF test into `justfile`/`just ci` — proved real RSS 9.4 MiB (≤ 15 MiB budget). Measured coverage 84.99%/83.35% (region/line) across `neuroos-healthd`+`neuroosctl`. Ran a real `neuroos-healthd --soak` process to confirm CSV output beyond the unit tests. Wrote `reports/phase-01-healthd.md`; Phase 1 gated clean (no owner sign-off items, unlike Phase 0). | P1-S01…S04 | Start Phase 2 (Neural Inference Engine, C4); owner still needs to review/merge `p1/s01-healthd` and sign off on Phase 0's 4 flagged deviations. |
| 2026-09-26 | Built all of P0-S02 through P0-S10 on `p0/s01-just-ci-green`, one story per commit, per owner's direction ("build all from S02 to S10 in this branch"). Real work throughout, not stubs: proto codegen + cross-language round-trip (S02); `neuroos-ipc` framing/UDS/SO_PEERCRED/deadlines/reconnect, 16 tests (S03); `neuroos-health` (S04); systemd hardening baseline + found/fixed a real `PrivateNetwork`-doesn't-block-DNS gap (S05); spike S-01 run for real, reverses the unit-mode preference (S06, ADR-0002); memfd seqlock ring in Rust+C++, found/fixed 2 real concurrency bugs via stress-testing and ThreadSanitizer (S07, ADR-0005); COSMIC toplevel spike against the live desktop + real bitnet.cpp submodule build with a downloaded model and a real tokens/s figure (S08, ADR-0006); model manifest + fetch script proven against 10 real downloaded/verified models (S09); `cargo-deny` wired for real, found `cosmic-protocols` is GPL-3.0 (S10, ADR-0007). `just ci` green after every story. | P0-S02 … P0-S10 | Verify Phase 0 exit criteria, write the phase report, decide on merging to `main`. |
| 2026-09-26 | Scaffolded full repo tree per Architecture.md §14 (empty stubs); committed + pushed to `main` (`981a4ba`). Created branch `p0/s01-just-ci-green`; wired Rust workspace, cpp CMake build, python uv project; installed missing toolchains (clang-format-18, shellcheck, cargo-nextest) with owner's help; `just ci`/`build`/`test`/`bench` all green; committed `c48b5fc`. | P0-S01 | Merge `p0/s01-just-ci-green` to `main` (owner to confirm PR vs direct merge), then start P0-S02. |
| 2026-09-26 | Read both blueprints; reconciled conflicts; produced PRD, Architecture, rules, phases, design, memory and the reports folder. | Planning | Owner reviews the documents and answers any open questions they can; then start P0-S01. |

---

## 12. Update Protocol (for the AI assistant)

1. **Session start:** read §1, §2, §4 and §8; read the current phase in `phases.md`; state the story you will work on.
2. **While working:** keep §2 accurate (story, files, next step) whenever the focus changes.
3. **Story done:** move it to Done in §4, add a line to §3 with evidence, update §1.
4. **New decision / question / blocker:** add to §6 / §7 / §8 immediately.
5. **Session end:** add a line to §11 and set "Next concrete step" in §2.
6. **Phase gate:** verify every exit criterion, write the phase report in `reports/`, set the phase to ✅ in §5, set the next phase as current in §1, reset §4 with the next sprint's stories.
