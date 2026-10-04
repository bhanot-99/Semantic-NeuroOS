# BUGS.md — Bug / Issue / Dead-Code Registry

Full-codebase review, 2026-10-04 (end of Phase 5, before Phase 6). Items are **Open** unless marked **Fixed**. H1–H5 fixed on branch `high_need_bugs` (commit 8c13b53), H6–H10 after it (2026-10-04). After H6–H10: `cargo nextest run --workspace` = 272 passed / 0 failed / 28 skipped; live C4 + real-model tests pass; `cargo clippy --workspace --all-targets -D warnings` exits 0. (Correction: 8c13b53 itself still had one clippy doc-list error, fixed with H6–H10.)

Baseline at review time: `cargo clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` 256 passed / 0 failed / 26 ignored; Python ruff + `mypy --strict` clean, pytest 37/37; shellcheck clean; C++ `-Wall -Wextra -Wconversion` 1 warning. The bugs below are mostly in paths no test exercises.

Severity: 🔴 High · 🟠 Medium · 🟡 Low · ⚫ Dead code · 🧹 Cleanup / rules / tooling.

---

## 🔴 High — privacy, security, correctness

| ID | Location | Issue | Status |
| :--- | :--- | :--- | :--- |
| H1 | `crates/neuroos-storage/src/sqlite.rs` (`forget_by_app`, `forget_since`), `lifecycle.rs` (`gc_expired_entities`), `migrations/0001_initial.sql` | Forget/GC fail with `FOREIGN KEY constraint failed` once any `edges` row references an entity (FK on, no `ON DELETE CASCADE`, edges never deleted first) — reproduced. Statements are not in a transaction, so a failure leaves a partial forget. | **Fixed** (`high_need_bugs`): `sqlite::delete_entities` removes edges/chunks_meta/FTS rows before entities; forget and GC each run in one transaction. Tests: `forget_by_app_succeeds_when_edges_reference_the_entity`, `forget_since_succeeds_…`, `gc_succeeds_when_edges_reference_an_expired_entity`. |
| H2 | `sqlite.rs::forget_since` | `forget --since X` only deletes entities *created* after X; a long-lived entity (e.g. Firefox) keeps every chunk/title from the window in LanceDB + `chunks_fts`. `focus_history` uses `t_start_ns` only (straddling sessions survive). | **Fixed**: `forget_since` also deletes every chunk with `t_ns >= since` (LanceDB + FTS) and focus segments with `t_end_ns >= since`. Tests: `forget_since_removes_recent_rows_of_an_older_entity`, live `forget_since_removes_a_recent_chunk_of_an_older_entity`. |
| H3 | `lance.rs`, `lifecycle.rs::backup` | No Lance old-version cleanup is ever called: "forgotten" vectors+text remain on disk in previous dataset versions, and backups copy the whole versioned directory. | **Fixed**: new `LanceStore::purge_deleted` (compact with `materialize_deletions_threshold = 0`, then prune all old versions) runs after every forget/GC, behind a per-family maintenance lock shared with background compaction/HNSW builds. LanceDB rows are deleted before SQLite so a failed forget is retryable. Tests: `purge_removes_deleted_rows_and_old_versions_from_disk` (byte-level), backup test now checks the backup's LanceDB copy. |
| H4 | `lifecycle.rs::gc_expired_entities` | Retention keyed on entity `created_ns`, not per-chunk/row age: after 14 days an app's whole history (including yesterday's) is wiped. `PRAGMA incremental_vacuum` is a no-op (`auto_vacuum` never set). | **Fixed**: GC is per row: chunks/event counters per domain by `t_ns`/`last_ns`, entities only when `last_seen_ns` is past retention, focus history by `t_end_ns`; returns per-domain cutoffs for LanceDB. `sqlite::open` switches the file to `auto_vacuum = INCREMENTAL` (one-time `VACUUM` for existing files). Tests: `gc_expires_old_rows_of_a_long_lived_entity_but_keeps_recent_ones`, `open_enables_incremental_auto_vacuum_on_a_real_file`. |
| H5 | `crates/neuroos-knowledge-query/src/graph_view.rs` | XSS in `graph_view.html`: entity labels (window/video/file titles) inserted via `innerHTML`; inline `var DATA = <json>` does not escape `</script>`. A crafted page/video title runs script with the whole graph embedded. | **Fixed**: inline JSON escapes `<`, `>`, `&`, U+2028/9 as `\uXXXX`; screen-reader table uses `textContent`; user data substituted last into the template. Tests: `a_label_cannot_break_out_of_the_data_script_block`, `labels_are_never_assigned_through_inner_html`, `placeholder_names_inside_a_label_are_left_alone`. |
| H6 | `knowledge-query/src/assemble.rs` + `cpp/neuroos-inference/src/engine.cpp` (`tokenize(..., parse_special=true)`) | Prompt injection: only `EXTERNAL_UNTRUSTED` text is escaped; window titles / deictic title (attacker-controllable) are inserted raw, so `<\|eot_id\|>System: …` in a title becomes real chat-role tokens. | **Fixed**: `assemble::defuse_control_tokens` splits every `<\|`/`\|>` in untrusted text (window title, app id, question, every evidence chunk, distillation chunks) so it tokenizes as plain characters; the template's own `<\|eot_id\|>` are the only control tokens left. Tests: `a_window_title_cannot_inject_chat_control_tokens`, `an_evidence_chunk_cannot_inject_chat_control_tokens`. |
| H7 | `cpp/neuroos-inference/src/server.cpp`, `lanes.cpp`, `cpp/libneuroos/include/libneuroos/shm_ring.hpp` | Cross-request answer leakage: ring generation is bumped at *submit*, and `RingWriter::write` stamps the header's *current* generation. A second request on the shared `knowledge-text` ring receives the first job's remaining tokens; the first reader sees only stale slots and waits for its 30 s timeout. | **Fixed**: C4 stamps each slot with the job's own `generation_id` (`RingWriter::write_as`, C++ + Rust); C5 reads with `Ring::reader_for_generation(GenerateResponse.generation_id)`. Tests: shm `a_reader_for_one_generation_never_sees_another_jobs_slots`; live `concurrent_generations_on_one_ring_do_not_mix_tokens` (reproduced: zebra got apple's tokens before the fix). |
| H8 | `engine.cpp::Context::generate`, `server.cpp::handle_generate` | Only prompt length is checked against `n_ctx` (512); prompt + generated tokens overflow the context, `llama_decode` fails mid-generation, no EOS is written, client hangs 30 s. | **Fixed**: `Context::generate` stops when the KV cache is full; `LaneScheduler` writes a terminal `FLAG_EOS` slot (plus `FLAG_CANCEL` if cancelled) whenever a job ends without one -- max_tokens, context limit, cancel or error (also covers part of M12). Live test `a_prompt_near_the_context_limit_ends_cleanly` (before: 30 s client timeout). |
| H9 | `knowledge-query/src/orchestrate.rs::ask` | Preamble call uses `?`: any C2 failure aborts the whole ask → degraded answer (violates rules §5.6 and `voice_client.rs`'s own doc). C2 doesn't exist yet, so `neuroosctl ask` cannot work in production. | **Fixed**: the preamble is best-effort (logged on failure); `AskError::Preamble` removed. Tests: `an_unreachable_voice_service_does_not_stop_context_resolution`, `ask_answers_even_when_the_voice_service_is_down`, `an_unreachable_storage_service_is_still_reported` (replaces the test that asserted the old behaviour); new `storage_mocks::spawn_empty`. |
| H10 | `crates/neuroos-monitor/src/sensors/mpris.rs` | When the D-Bus `NameOwnerChanged` stream ends, `run_once` returns `Ok(())` → `unreachable!()` panics → the MPRIS sensor task dies permanently, no reconnect. | **Fixed**: `run_once` returns the stop reason (`MprisSensorError`, new `Disconnected` variant when the bus stream ends); no `unreachable!`. Test `a_dropped_bus_connection_is_an_error_not_a_panic` kills a private `dbus-daemon`. |
| H11 | `crates/neuroos-storage/src/main.rs`, `server.rs` | C1→C3 pipeline not wired in production: C3 never subscribes to `monitor.sock`, has no `IngestRequest` handler, and never starts `spool::watch_forever`, GC or backup. (Known memory.md debt, still open.) | Open |
| H12 | `crates/neuroosctl/src/main.rs`, `deploy/systemd/*` | `neuroosctl forget` / `storage gc` / `storage backup` don't exist → gc/backup timers always fail; FR-PRV-03 forget unreachable from CLI. System-template units use `%h` (→ `/root`) and `/run/user/%i` (username, not UID); `neuroos-knowledge-background` ExecStart binary doesn't exist (no `[project.scripts]`); `ProtectHome=tmpfs` hides `config.toml` from storage/healthd/knowledge; knowledge-query writes `graph_view.html` into an invisible tmpfs; unused `NEUROOS_RUNTIME_DIR` env; `BindReadOnlyPaths` omits onnxruntime. | Open |
| H13 | `python/neuroos-knowledge-background/src/neuroos_bg/main.py` | Loop catches only `IpcError`; `TimeoutError`/`ConnectionResetError`/protobuf `DecodeError` (or any other exception) crash the worker (rules §5.9). | Open |
| H14 | `neuroos_bg/main.py` + `graph.py` | Every 10-min run re-excites every pair with close `last_seen_ns` and sets `reinforced_ns=now`: one co-occurrence counted ~1000× over the 7-day lookback, weights grow unbounded, the 72 h prune never fires. | Open |
| H15 | `crates/neuroos-sandbox` (empty), every service `main()`, `cpp/libneuroos`, `tests/contract/storage_landlock.sh` | Kernel-level file isolation (Architecture.md AP-1, §8.2) is not implemented: no Landlock ruleset anywhere, only systemd unit hardening (itself broken, H12). A service run outside its unit, or a misconfigured unit, can read any file the user owns (e.g. `~/.ssh`, or via the spool symlink in M14). Plan: implement `neuroos-sandbox` with the allowlisted `landlock` crate (+ a C++ equivalent in `libneuroos` for C4/C2), apply the §8.2 per-component read-only/read-write rulesets right after config load, fail closed if Landlock can't be applied (rules §5.5), add a real denial test per component, and fix `storage_landlock.sh`'s false "systemd uses Landlock" claim. Kernel already supports it (`landlock` in `/sys/kernel/security/lsm`). seccomp deferred to Phases 7–8. Do before/at the start of Phase 6 so C2 is born sandboxed. | Open |

---

## 🟠 Medium

| ID | Location | Issue | Status |
| :--- | :--- | :--- | :--- |
| M1 | `crates/neuroos-shm/src/ring.rs`, `cpp/libneuroos/src/shm.cpp` | `Ring::create/open` don't reject `capacity_slots == 0` (div-by-zero) or `slot_size < 32`; reader trusts `utf8_len` from shared memory (no `<= max_payload` clamp) → out-of-bounds read in unsafe code. Header `version` never checked. | Open |
| M2 | `neuroos-health/src/lib.rs::serve`, `neuroos-healthd/src/server.rs`, `cpp/neuroos-inference/src/server.cpp`, `cpp/libneuroos/src/health_server.cpp`, `neuroos-healthd/src/main.rs` | Servers die permanently on the first `accept()`/peer-cred error (`?` / `break`). healthd ignores its own `healthd.sock` bind failure (spawned result dropped). | Open |
| M3 | all components | `set_status`/`incr_error` never called in production → every health endpoint always reports OK; `neuroosctl status` can't show a real problem. | Open |
| M4 | `crates/neuroos-healthd/src/lib.rs::scrape_cycle` | Soak p99 taken from `latency_histograms.values().next()` (random HashMap order); baseline captured only on the first cycle, so a target DOWN then gets a 0 baseline and never breaches. DOWN-on-first-failure contradicts the documented UNKNOWN/DOWN semantics. | Open |
| M5 | `monitor/src/sensors/wayland_cosmic.rs` | `next_id` restarts at 1 after every Wayland reconnect → reused toplevel ids; C3's filter pairs stale `active_since_ns` with new windows (bogus dwell) and never sees `Closed` for the old ones. Backoff never resets. | Open |
| M6 | `monitor/src/sensors/proc.rs::comm_matches` | Empty `app_id` → candidate `""` matches every process → PID 1 → whole-system process tree. Reverse-prefix match should require a 15-char (truncated) comm. | Open |
| M7 | `monitor/src/sensors/mpris.rs` | A watcher task is spawned on every owner change and old ones never end (property streams don't close) → leaked tasks + duplicate events; `JoinSet` never reaped. | Open |
| M8 | `monitor/src/anonymise.rs` | Unsalted FNV-1a hash of titles → committed fixture titles can be confirmed by dictionary guessing. Use a per-run random key. | Open |
| M9 | `knowledge-query/src/orchestrate.rs`, `evidence.rs`, `storage/src/engine.rs::fuse_rrf`, `storage.proto` `ActivityItem` | Taint gaps: `MODEL_GENERATED` never set on answers; activity evidence built with `taint: None` (proto carries none); RRF text-dedupe keeps the first chunk's taint instead of the union. | Open |
| M10 | `knowledge-query/src/distill.rs`, `orchestrate.rs` | Distillation cache is write-only (`get` never called) yet burns seconds of C4 CPU per over-threshold question; cache + queued tasks unbounded; `count_tokens` is awaited on the hot path despite the "fire-and-forget" comment. | Open |
| M11 | `knowledge-query/src/inference_client.rs::read_all_tokens` | `from_utf8_lossy` per token piece corrupts multi-byte UTF-8 split across pieces; no `CancelRequest` on timeout/error (C4 keeps generating); `JoinError` mapped to `GenerationTimedOut`. | Open |
| M12 | `cpp/neuroos-inference/src/lanes.cpp` | No EOS/`FLAG_CANCEL` written on cancel, error or max_tokens (client hangs); `cancel()` neither dequeues queued jobs (full prompt prefill still runs) nor bumps the generation, contrary to its doc. Ring write failures (piece > 224 B) silently dropped. | Partly fixed by H8 (terminal EOS/CANCEL slot on every job end); `cancel()` still doesn't dequeue queued jobs; oversize pieces still dropped. Open |
| M13 | `storage/src/ingest/filter.rs`, `adapters/mod.rs` | Unbounded growth: `pid_roots`, `counters` (`proc:<pid>` keys) never pruned; identical "build job under X" chunk embedded every 5 s during builds; every file event (incl. `.git`/`target` churn) embedded as a chunk; doc claims `neuroos-` comm self-exclusion that isn't implemented. | Open |
| M14 | `storage/src/spool.rs` | One bad file aborts `ingest_existing` (`?`) so later files are never ingested; no spool file/text size limit; symlinks followed (compromised C7 → C3 reads arbitrary user files); `url`/`content_type` ignored; duplicate `doc_id` duplicates chunks. | Open |
| M15 | `storage/src/engine.rs::store_chunk`, `lance.rs::records_to_batch` | Empty embedding (`unwrap_or_default`) → Arrow `FixedSizeList` builder panic. Lance insert + FTS insert + index_meta not atomic. | Open |
| M16 | `storage/src/embed.rs`, `engine.rs::reindex_family` | `unsafe set_var("ORT_DYLIB_PATH")` runs from a spawned task while other threads run (SAFETY comment false); reindex loads a second model (~2× C3 RSS, 300 MiB cap). | Open |
| M17 | `storage/src/sqlite.rs::migrate` | Migrations not run in a transaction → a crash mid-migration leaves a DB that can never open again ("table already exists"). | Open |
| M18 | `knowledge-query/src/assemble.rs` | 64-token deictic budget and the system preamble are never measured (512-token "hard cap" not enforced); `truncate_evidence` re-tokenizes the whole growing string per chunk (one C4 round trip each). | Open |
| M19 | `neuroos-shm/src/ring.rs`, `cpp/libneuroos/include/libneuroos/shm_ring.hpp` | Seqlock lacks fences (writer `fence(Release)` after the odd store; reader `fence(Acquire)` before the second load). Correct on x86, wrong on weakly-ordered CPUs (ARM). | Open |
| M20 | `crates/neuroos-ipc/src/client.rs`, all clients | No client reconnects (AB-10): `connect_with_reconnect` is test-only, has no jitter and a 2 s (not 10 s) cap. | Open |

---

## 🟡 Low

| ID | Location | Issue | Status |
| :--- | :--- | :--- | :--- |
| L1 | `storage/src/server.rs`, `knowledge-query/src/server.rs` | Error messages embed raw rusqlite/FTS/Lance text (may contain user content, rules §5.3); unsupported requests return `Internal, retryable=true`; no caps on `top_k`/`limit`. | Open |
| L2 | all IPC servers (Rust + C++) | No per-connection idle/read timeout; unbounded connections/threads. | Open |
| L3 | `cpp/neuroos-inference/src/server.cpp` | Unknown request bodies get no reply (`continue`) → client hangs to deadline; any client can create unlimited named rings, never freed. | Open |
| L4 | `cpp/libneuroos/src/framing.cpp`, `shm.cpp` | `read_envelope_with_fd` leaks the fd on body-read error and on repeated cmsgs, no `MSG_CMSG_CLOEXEC`; `Ring::open` leaks the fd when `mmap` throws. | Open |
| L5 | `cpp/neuroos-inference/src/config.cpp`, `engine.cpp` | Malformed `[inference]` silently falls back to defaults; unknown keys ignored (typo'd `model_sha256` silently skips the integrity check); `tokenize_count` uses `add_special=false` vs `true` in generate (off by one); 64-bit seed truncated to 32 bits. | Open |
| L6 | `crates/neuroos-common/src/paths.rs` tests | `set_var("NEUROOS_CONFIG")` test is not thread-safe under `cargo test` (SAFETY comment false). Same pattern in `neuroosctl` tests (`XDG_RUNTIME_DIR`): `run_graph_open_round_trips_and_writes_the_html_file` flaked once under `cargo test` on 2026-10-04 (passes serially and under `cargo nextest`, the project's runner). | Open |
| L7 | `crates/neuroos-health/src/lib.rs`, `histogram.rs` | Own `SystemTime`-based `now_ns` (rules say `jiff`/`neuroos_common::now_ns`); histogram uses 3 separate mutexes (inconsistent snapshot); `sum_ns` can overflow. | Open |
| L8 | `crates/neuroos-healthd/src/scrape.rs` | Connect + write + read each get the full timeout (up to 3×), contrary to "one deadline". | Open |
| L9 | `scripts/check-egress.sh` | Requires real internet DNS (`getent hosts example.com`) to pass — violates rules §7.4, fails offline. | Open |
| L10 | `scripts/fetch-models.sh` | No download timeout; `PLACEHOLDER` sha256 entries accepted unverified. | Open |
| L11 | `monitor/src/sensors/folders.rs`, `proc.rs` | Unbounded `mpsc` channel for inotify events; `collect_subtree` is O(n²). | Open |
| L12 | `storage/src/adapters/mod.rs` | `adapt_resource` discards the `ResourceSample` data; `focus_history.pid/root_pid` always 0. | Open |
| L13 | `storage/src/sqlite.rs::upsert_entity` | `.ok()` swallows real SELECT errors as "not found"; no UNIQUE index on `(domain, label)`; FTS delete per entity id is a full scan. | Open |
| L14 | `monitor/src/dump.rs` | `--record` silently overwrites an existing dump (`File::create`); `BufWriter` is pointless because `write_frame` flushes every frame. | Open |
| L15 | `python/neuroos-knowledge-background/pyproject.toml` | `protobuf<5` pin emits Python 3.14 deprecation warnings; wheel ships no generated `_pb2` files. | Open |
| L16 | `crates/neuroos-ipc/src/server.rs` | Parent runtime dir created with default umask (not 0700); socket file mode not set. | Open |

---

## ⚫ Dead code

| ID | Location | Item | Status |
| :--- | :--- | :--- | :--- |
| D1 | Cargo manifests | Unused dependencies: `thiserror` (neuroosctl), `prost` (neuroos-health), `clap` (neuroos-knowledge-query), `tracing-subscriber` + `wayland-protocols-wlr` (neuroos-monitor), `tempfile` dev-dep (neuroos-shm). | Open |
| D2 | Rust / Python | Never used in production: `paths::models_dir`, `paths::backups_dir`, `IngestFilter::collapse_pid` / `title_for` / `pid_roots`, `orchestrate::ask_context` (test-only duplicate of `ask`), `pruning.is_stale`, `DistillationCache::get`, `memfd_as_fd` wrapper, `FLAG_CANCEL` / `kFlagCancel`. | Open |
| D3 | `storage/src/migrations/0001_initial.sql`, `sqlite.rs`, `lifecycle.rs` | Tables `chunks_meta` and `aggregates_daily` never written; every `DELETE FROM chunks_meta` is dead. Removal needs a new migration. | Open |
| D4 | `knowledge-query/src/graph_view.rs` | First `svg.call(d3.zoom()…)` binding is overridden by `zoomBehaviour`. | Open |
| D5 | stubs | Comment-only / empty placeholders: kernel `audit.rs`, `hmac.rs`, `registry.rs`, `runner.rs`, `tier.rs`, `skills/mod.rs` (not declared as modules); fetcher `fetch.rs`, `spool.rs`, `ssrf.rs`; empty `main()` in kernel/confirm/fetcher; `neuroos-sandbox` (empty crate); `monitor/src/sensors/wayland_wlr.rs`; `cpp/libneuroos/src/libneuroos.cpp` + `include/libneuroos/libneuroos.hpp`; 9 one-line `cpp/neuroos-voice/src/*.cpp` (Phase 6); `scripts/install.sh`, `uninstall.sh`, `migrate_v1_to_v2.py`; `ui/graph-view/template.html`, `ui/tokens/tokens.json` (invalid JSON). Owner to decide: delete vs keep as scaffolding. | Open |

---

## 🧹 Cleanup, rule violations, tooling, docs

| ID | Location | Issue | Status |
| :--- | :--- | :--- | :--- |
| C1 | 12 places (common, healthd ×2, health, monitor, storage ×2, knowledge-query ×3, neuroosctl, testkit) | Copy-pasted `extern "C" getuid()` — replace with `rustix::process::getuid()`. Also violates rules §6 (`unsafe` only in shm/sandbox/FFI); no crate has `#![deny(unsafe_code)]`. | Open |
| C2 | health, lance, distill, healthd | Poison-recovering `lock()` helper duplicated 4×. | Open |
| C3 | neuroosctl, knowledge-query clients | Connect/write/read/match request boilerplate repeated ~9×. | Open |
| C4 | knowledge-query/storage tests | `dev_models_dir` / `dev_onnxruntime_dylib` / `current_uid` helpers duplicated across test files; storage engine tests repeat the same 3-event ingest setup 4×. | Open |
| C5 | monitor sensors (idle, cosmic, folders, mpris), storage engine | `unreachable!()` and `std::panic::resume_unwind` in non-test code (rules §5). | Open |
| C6 | `justfile`, `.clang-tidy`, `cpp/CMakeLists.txt` | `just lint-cpp` calls `clang-tidy` (not installed; only `clang-tidy-18`, which can't find std headers); `.clang-tidy` `WarningsAsErrors: ''` and CMake sets no `-Wall -Werror` (rules: warnings are errors); `cargo-audit` not installed. | Open |
| C7 | `README.md`, `tests/contract/storage_landlock.sh`, `config/config.example.toml`, `knowledge-query/graph_view.rs` | README status says "Phase 0 not yet started"; landlock test claims systemd uses Landlock (it uses mount namespaces — no Landlock anywhere); example config documents only `[monitor]`; graph view hard-codes hex colours instead of `ui/tokens/tokens.json` (design.md rule). | Open |
| C8 | `crates/neuroos-storage/src/lance.rs` | `compact_all_families` doc says compaction is off the hot path / backup-only, but BUG-007(c) added per-256-insert background compaction — stale doc. | Open |
| C9 | `crates/neuroos-storage/Cargo.toml`, `neuroos-knowledge-query/Cargo.toml` | Dev-dependencies link other components' crates (storage→monitor, knowledge-query→storage/monitor), bending AB-1 for tests. | Open |
| C10 | `cpp/neuroos-inference/CMakeLists.txt` | Writes `bitnet-lut-kernels.h` into the `third_party/bitnet.cpp` submodule worktree (dirties it). | Open |

---

## Open decisions for the owner

1. Fix order (High = H1–H15): **A** High only · **B** High + Medium · **C** dead code / cleanup only · **D** everything (A → B → C → Low).
2. Stubs (D5): delete, or keep as scaffolding for later phases?
3. Dead tables (D3): add a migration dropping `chunks_meta` / `aggregates_daily`, or keep for later?
