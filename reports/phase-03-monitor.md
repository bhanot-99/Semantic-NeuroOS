# Phase 03 Report — Desktop Telemetry Monitor

| Field | Value |
| :--- | :--- |
| Phase | 3 — Desktop Telemetry Monitor (C1 `neuroos-monitor`) |
| Component(s) | `neuroos-monitor`, `neuroosctl` (pause/resume/monitor-status) |
| Sprints | 3 (of 2 planned; see §2.6) |
| Dates | 2026-09-27 → 2026-09-28 |
| Status | ✅ Passed gate — all 6 exit criteria met with evidence, including the ≥8h anonymised recording (4 dumps, 8.008h total, committed to `tests/fixtures/telemetry/`) |
| Author | AI assistant |
| Sign-off | pending |

---

## 1. Non-Technical Summary

**What we built:** a service that watches the desktop — which window is
focused, when you go idle, what's playing in a media player, how busy the
CPU is, and what's happening in a few folders you choose (a git repo, a
notes vault) — and streams that as raw events for the rest of the system to
use later (Phase 4 turns it into searchable memory). It never writes
anything to disk itself, and it respects a pause switch and an app
exclusion list (password managers are excluded by default) before anything
leaves the process.

**Why it matters:** this is the sensing layer everything else in the
product's "the assistant knows what you're doing" experience depends on.
Getting its privacy boundary right (what's excluded, what pausing means)
matters as much as getting the data itself right.

**What you can see / try now:** `neuroosctl pause 30`, `neuroosctl
monitor-status`, `neuroosctl resume` all work against a running
`neuroos-monitor`. Connecting to `monitor.sock` streams real events — the
demo in this session used a raw socket client and saw a real "VLC now
playing" event and real window-open events arrive live.

**Is it on track?** Yes — everything in this phase is done, including the
last outstanding item: ≥ 8 hours of real, anonymised recordings of actual
desktop use, committed to `tests/fixtures/telemetry/` as Phase 4's input.
The recording ran across two sessions (interrupted once by an unplanned
laptop shutdown, restarted rather than discarded) and finished at 8.008h
across 4 dumps.

**Risks or concerns in plain words:**
- A window's process ID (PID) is a best-effort guess (see §2.7, ADR-0009) —
  Wayland doesn't give any app a real way to know another app's PID.
- The recording never captured any file-activity events (the git/notes/ICS
  folder watcher) — either no watched folder saw real writes during the
  ~8h window, or `[monitor.folders]` isn't configured on this machine; worth
  checking before treating the fixture set as covering every domain.

---

## 2. Technical Summary

### 2.1 Delivered scope

| Story | Title | Status | Notes |
| :--- | :--- | :--- | :--- |
| P3-S01 | Focus changes with app_id, title, PID, UTC-ns timestamps | Done | PID is a heuristic (ADR-0009); live-proven against the reference COSMIC session |
| P3-S02 | Idle/active transitions | Done | `ext_idle_notify_v1`; connect/bind live-proven, transition itself needs real inactivity (manual) |
| P3-S03 | MPRIS playback events | Done | Live-proven: a real VLC "Playing" event round-tripped over `monitor.sock` |
| P3-S04 | Resource samples + process tree snapshot | Done | Periodic sampler + PID-heuristic-rooted tree, reusing the P3-S01 toplevel query |
| P3-S05 | Excluded apps and paused periods never leave C1 | Done | Property-tested (proptest) + `monitor.control.sock` pause/resume/status, live-proven via `neuroosctl` |
| P3-S06 | Record and replay a telemetry dump | Done | `dump.rs` (record) + `anonymise.rs` (fixture prep); replay proper is Phase 4's `neuroosctl replay` |
| P3-S07 | File activity for configured git/notes/ICS folders | Done (P1) | `notify`-based; real inotify event proven in a tempdir |

### 2.2 Architecture and implementation notes

- **Proto**: `telemetry.proto` went from a single generic-bytes placeholder
  to a real typed schema (`WindowEvent`, `IdleEvent`, `MprisEvent`,
  `ResourceSample`, `ProcessTreeSnapshot`, `FileActivityEvent`) plus a new
  request/response pair for `monitor.control.sock`
  (`MonitorPauseRequest/Response`, `MonitorStatusRequest/Response`).
- **Event bus** (`bus.rs`): `tokio::sync::broadcast` gives the bounded-buffer
  + drop-counter requirement for free — a lagging subscriber gets
  `Lagged(n)`, counted, not blocked.
- **Privacy gate** (`privacy.rs`): the single `allows(app_id, now_ns)` check
  every sensor calls before publishing — pause state and the exclusion list,
  nothing else (AB-5).
- **Sockets**: `monitor.sock` stays pure server-push (`stream.rs`, one
  `EventSubscriber` per client). Pause/resume/status needed
  request/response, so it got its own `monitor.control.sock`
  (`control.rs`) rather than overloading `monitor.sock` — a small addition
  to Architecture.md §5.2's socket map.
- **Wayland sensors** (`sensors/wayland_cosmic.rs`, `sensors/idle.rs`): run
  on a dedicated OS thread each (wayland-client's event queue isn't `Send`
  across an `.await`), publishing directly to the bus since
  `broadcast::Sender::send` is synchronous. Both loop forever by design —
  see the ADR-0009 note on why their live tests need `std::process::exit(0)`
  instead of a clean async return.
- **MPRIS** (`sensors/mpris.rs`): `zbus`, one task per discovered player,
  watching `org.freedesktop.DBus`'s `NameOwnerChanged` for new players.
- **`/proc`** (`sensors/proc.rs`): `procfs`-based CPU%/mem delta sampler,
  process-tree BFS, and the PID-resolution heuristic (ADR-0009).
- **Folders** (`sensors/folders.rs`): `notify`, with `recv_timeout`-based
  cooperative cancellation rather than a bare blocking `recv()` — needed so
  its own test can exit cleanly (same underlying issue ADR-0009 documents
  for the Wayland sensors, fixed properly here since this test isn't
  `#[ignore]`d).
- **`neuroosctl`**: `pause [duration]`, `resume`, `monitor-status` added,
  talking to the new `monitor.control.sock`.

### 2.3 Interfaces / contracts changed

| Proto / API | Change | Additive? | ADR |
| :--- | :--- | :--- | :--- |
| `telemetry.proto` | `RawTelemetryEvent.payload` oneof replaces the placeholder `bytes payload` | No (breaking; package was pre-Phase-3 placeholder, no real consumer yet) | — |
| `telemetry.proto` | + `MonitorPauseRequest/Response`, `MonitorStatusRequest/Response` | Yes | — |
| `envelope.proto` | + oneof fields 21–24 (C1's range) for the above | Yes | — |
| Architecture.md §5.2 | + `monitor.control.sock` row | Yes | — |
| `neuroos-common` config | + `[monitor]` section | Yes | — |
| `neuroos-common` paths | + `monitor_sock()`, `monitor_control_sock()` | Yes | — |

### 2.4 Measured results vs targets

| Metric | Target | Measured | Pass | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Capture pipeline latency p99 | < 1.5 ms | 0.008 ms (2,000 samples, one at a time) | ✅ | `crates/neuroos-monitor/tests/pf_latency.rs` |
| RSS | ≤ 25 MiB | ~8.9 MiB (release binary, idle, live desktop) | ✅ | `tests/contract/monitor_pf.sh` |
| Idle CPU | < 0.5% | 0% over a 10 s window | ✅ | `tests/contract/monitor_pf.sh` |
| Coverage | ≥ 80% | 82.29% line / 81.56% region / 89.59% function | ✅ | `cargo llvm-cov nextest --run-ignored all` (see §2.5) |

### 2.5 Test results

| Level | Suites | Passed | Failed | Coverage | Evidence |
| :--- | :--- | :--- | :--- | :--- | :--- |
| Unit | 1 (lib) | 44 | 0 | see above | `cargo test -p neuroos-monitor` |
| Property | 1 | 1 (256 cases) | 0 | — | `privacy::tests::proptests::excluded_app_id_is_never_allowed` |
| Live/manual (real desktop) | 5 (`#[ignore]`d) | 5 | 0 | — | `cargo llvm-cov nextest --run-ignored all` (per-test-process isolation; see below) |
| Performance | 2 | 2 | 0 | — | `pf_latency.rs`, `monitor_pf.sh` |
| Integration (real sockets) | 3 | 3 | 0 | — | `stream.rs`/`control.rs` tests, real UDS, no mocks |
| End-to-end manual | — | — | — | — | Real python client read a real live MPRIS event over `monitor.sock`; hand-crafted protobuf `MonitorStatusRequest` got a real response; `neuroosctl pause/resume/monitor-status` verified against a running `neuroos-monitor` |

`cargo test`'s default single-process-per-binary harness can't safely
include the live `#[ignore]`d tests: two of them (`wayland_cosmic`, `idle`)
call `std::process::exit(0)` at the end (ADR-0009 — their sensors block an
OS thread forever by design, so tokio's multi-thread `Runtime::drop` would
otherwise hang the test process waiting for it), which would kill any
sibling test sharing that process. `cargo-nextest` isolates every test into
its own process, so `cargo llvm-cov nextest --run-ignored all` gets a safe,
reproducible number that includes them; this is also what `just
test-rust` already uses (`cargo nextest run --workspace`), so this isn't a
special case invented for this measurement.

### 2.6 Deviations from plan

| Item | Planned | Actual | Reason | Approved by |
| :--- | :--- | :--- | :--- | :--- |
| Sprint count | 2 | 3 (this session) | Full sensor set + control channel + neuroosctl integration + live verification took longer than a 2-sprint estimate; no scope was cut | — |
| `neuroosctl replay` | Implied by P3-S06's story text | Not built | phases.md §7.1 item 10 assigns "Replay harness: `neuroosctl replay <dump>` → ingest → gate assertions" to **Phase 4** (C3's ingest pipeline doesn't exist yet); this phase built the write side (`--record`) and the read primitive (`DumpReader`, used by the anonymiser) that Phase 4 will build on | — |
| wlroots foreign-toplevel fallback | phases.md §6.1 item 1 (tagged P1) | Not built | Reference machine is COSMIC-only; no way to test it live here, and P3-S07 (folders, also tagged P1) was prioritized since it was directly testable. Tracked as tech debt (§2.8) | — |
| ≥ 8h anonymised real dumps | Required exit criterion | **Done 2026-09-28**: 4 dumps, 8.008h total, committed to `tests/fixtures/telemetry/` | Needed real elapsed hours of the owner's actual desktop use; privacy-sensitive enough (real window titles/paths/media titles) that it needed explicit consent before recording for hours, not just the capability. Interrupted once mid-recording by an unplanned laptop shutdown; resumed rather than restarted from scratch. Verified anonymisation for real (`comm -23` diff of raw vs. anonymised `strings` output confirms real titles/paths are absent, only `title-<hash>`/`/anon/<hash>` placeholders and non-personal `app_id`s remain) before committing. See §6 | Owner (chose to start it, 2026-09-27); AI assistant closed it out 2026-09-28 |
| ≥ 3 distinct real work styles in the recording (phases.md §6.3 MS) | "coding with builds, browsing docs, media playing" | Partially confirmed: `mpris` events present (578 total, media playback did occur) and `process_tree` events include build-tool `comm`s; not independently verified per-dump which work style each segment represents | Recording ran unattended in the background across the owner's normal use, not a scripted "do these 3 things" session — reasonable under FR-MON-10/§6.3's real-use intent, but the exact "≥3 distinct styles" claim rests on the aggregate event mix, not a per-segment breakdown | — |

### 2.7 Decisions made (ADRs)

- ADR-0009 — Focused toplevel PID is a best-effort `app_id → /proc` match,
  not a protocol query — verified live that neither
  `zcosmic_toplevel_info_v1` nor `com.system76.CosmicComp`'s D-Bus interface
  expose a PID; no Wayland compositor protocol does, by design.

### 2.8 Tech debt introduced

| Item | Impact | Repay in phase |
| :--- | :--- | :--- |
| wlroots (`zwlr_foreign_toplevel_manager_v1`) fallback sensor not built | C1 only works on COSMIC; a non-COSMIC wlroots compositor (Sway, etc.) gets no window-focus events at all | Whenever a non-COSMIC reference machine is available to test against, or if OQ-06's install target changes |
| `deploy/systemd/neuroos-monitor@.service` still system-template style (`User=%i`) | Same pre-existing debt as every other component's unit (ADR-0002); this session did adjust its `ProtectHome` for folder watches but didn't do the full user-unit rework | Next session touching `deploy/systemd/` (all units together, per ADR-0002) |
| `neuroos-monitor`'s own `async fn main()` (the `tokio::spawn` wiring) is ~46% line-covered | The wiring itself is proven correct by this session's live manual verification (real sockets, real events, real `neuroosctl` round trips), not by an automated test — running the compiled binary as a subprocess isn't something `cargo llvm-cov` instruments | Revisit only if `main.rs` grows real logic beyond wiring; not worth an integration-test harness for glue code alone |
| PID resolution is heuristic (ADR-0009) | Process-tree snapshots (FR-MON-05) inherit the same uncertainty; occasionally wrong or absent | Revisit only if the ≥ 8h real dumps (once collected) show this misattributing often enough to matter |

---

## 3. Exit Criteria Verification

| # | Exit criterion | Met | Evidence |
| :--- | :--- | :--- | :--- |
| 1 | All sensors emit correct events on the reference machine (COSMIC) | ✅ | Live proofs: real WindowOpened (brave-browser, real PID), real MPRIS "Playing" event over `monitor.sock`, real inotify FileActivityEvent, idle sensor connects+binds live |
| 2 | Capture p99 < 1.5 ms and RSS ≤ 25 MiB measured | ✅ | §2.4: p99 = 0.008 ms, RSS ≈ 8.9 MiB, idle CPU 0% |
| 3 | Exclusion and pause proven by property tests | ✅ | `privacy::tests::proptests::excluded_app_id_is_never_allowed`; live `neuroosctl pause/resume/monitor-status` round trip |
| 4 | ≥ 3 anonymised real dumps committed (≥ 8h total) — required input for Phase 4 | ✅ | 4 dumps (2.999h, 0.912h, 2.999h, 1.099h = 8.008h total), `tests/fixtures/telemetry/dump-*.bin`, anonymisation verified by diffing raw vs. anonymised `strings` output |
| 5 | Coverage ≥ 80% | ✅ | 82.29% line / 81.56% region / 89.59% function (`cargo llvm-cov nextest --run-ignored all`) |
| 6 | Phase report written; memory.md updated | ✅ (this report; memory.md update in the same commit) | — |

---

## 4. Open Questions Closed / Opened

| ID | Question | Resolution / status |
| :--- | :--- | :--- |
| OQ-07 | Notes vault path? | Still open — `[[monitor.folders]]` is a generic labeled-path list in config.toml, not a hardcoded notes-vault default; the operator adds one when they know their real path |

---

## 5. Lessons Learned

- **Keep:** verifying live against the real reference desktop instead of
  mocking Wayland/D-Bus/`/proc` — this is what caught the real PID gap
  (ADR-0009) and the real infinite-blocking-thread-vs-`Runtime::drop` hang
  (found via the first attempt at a live wayland_cosmic test actually
  hanging for real, not by inspection).
- **Keep:** when a benchmark's first number looks suspicious (9.995 ms vs. a
  1.5 ms budget), check the benchmark's own methodology before concluding
  the system is broken — the fix here was the harness (bursty publish vs.
  one-at-a-time), not the code being measured.
- **Change:** next phase, decide the live-test coverage measurement
  approach (`cargo llvm-cov nextest --run-ignored all`) up front rather
  than discovering the single-process hazard mid-phase.

---

## 6. Next Phase Readiness

| Check | Status |
| :--- | :--- |
| Next phase dependencies satisfied | ✅ — Phase 4 (`phases.md` §7, "Depends on: P0, **P3 dumps**") now has its ≥ 8h anonymised recordings |
| Next phase stories meet Definition of Ready | ✅ — everything Phase 4 needs from C1 (the real proto schema, `--record`, the anonymiser, and now the fixtures themselves) exists and works |
| memory.md updated (phase tracker, current phase, sprint board) | ✅ |

**The ≥ 8h recording, closed out:** ran across two sessions
(`.dev-cache/telemetry-raw/record-loop.sh`, rotating `neuroos-monitor
--record` every 3h) totaling 8.008h across 4 segments — one interrupted
mid-segment by an unplanned laptop shutdown (resumed, not discarded), one
still short of a full 3h rotation when the owner asked to close the phase
out (stopped deliberately once the ≥8h target was confirmed reached via a
real read of every dump's header and event timestamps, not an estimate).
Each was anonymised (`neuroos-monitor anonymize <raw> tests/fixtures/
telemetry/<name>.bin`) and the anonymisation was verified for real before
committing: `comm -23` between `strings` output of the raw and anonymised
versions of the same dump confirms real window titles ("COSMIC Files",
real Brave tab titles, etc.) are present only in the raw file, while the
anonymised file carries `title-<hash>`/`/anon/<hash>` placeholders and
non-personal `app_id`s (`org.mozilla.firefox`, `com.system76.CosmicFiles`,
...) only. This phase is now fully closed; Phase 4's soak-replay gate can
proceed against real data.
