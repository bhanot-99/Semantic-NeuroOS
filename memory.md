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
| Last updated | 2026-09-26 |
| Project stage | Phase 0 in progress |
| Current phase | **Phase 0 — Foundation, Contracts & Spikes** (in progress) |
| Current sprint | Sprint 0 (started 2026-09-26) |
| Current story | — (next: P0-S02) |
| Overall progress | 0 / 11 phases complete (1 / 10 Sprint-0 stories done) |
| Health | 🟢 On track |
| Next milestone | M0 Foundation |

---

## 2. Currently Working On

| Field | Value |
| :--- | :--- |
| Story | P0-S02 (Proto v1 contracts + codegen for Rust/C++/Python) |
| File(s) being edited | — |
| Branch | `p0/s01-just-ci-green` (merge to `main`, then branch `p0/s02-...` for next story) |
| Started | — |
| Goal of this session | — |
| Next concrete step | Merge/PR `p0/s01-just-ci-green` to `main`, then start P0-S02: define proto/neuroos/v1/*.proto messages, wire prost (Rust), protoc (C++), python codegen, cross-language round-trip test. Blueprints still need moving to `docs/blueprints/` (was deferred, not part of S01 scope). |

---

## 3. Completed Work Log

Newest first. One line per meaningful unit of work. Format: `YYYY-MM-DD · [Phase/Story] · what was done · evidence/link`.

| Date | Phase / Story | Completed | Evidence |
| :--- | :--- | :--- | :--- |
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

**Sprint 0 goal:** repository, toolchains, contracts and shared IPC libraries in place; spikes S-01 and S-02 started.

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P0-S01 | `just ci` green on clean checkout (scaffold, toolchains, justfile) | 5 | Done |
| P0-S02 | Proto v1 contracts + codegen for Rust/C++/Python | 5 | Done |
| P0-S03 | `neuroos-ipc` (framing, UDS, SO_PEERCRED, deadlines, reconnect) | 8 | Done |
| P0-S04 | `neuroos-health` endpoint + histograms | 3 | Done |
| P0-S05 | systemd templates with hardening baseline | 3 | Done |
| P0-S06 | Spike S-01 unit mode → ADR-0002 | 5 | Ready |
| P0-S07 | Spike S-02 memfd seqlock ring | 5 | Backlog (Sprint 0b) |
| P0-S08 | Spikes S-03 (COSMIC) + S-04 (bitnet.cpp) | 5 | Backlog (Sprint 0b) |
| P0-S09 | Model manifest + fetch script | 3 | Backlog (Sprint 0b) |
| P0-S10 | cargo-deny network-crate ban | 2 | Backlog (Sprint 0b) |

Columns: Backlog → Ready → In Progress → In Review → Testing → Done.

---

## 5. Phase Tracker

| Phase | Name | Status | Started | Finished | Report |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 0 | Foundation, Contracts & Spikes | ⬜ Not started | — | — | `reports/phase-00-foundation.md` |
| 1 | Health Aggregator (healthd) | ⬜ Not started | — | — | `reports/phase-01-healthd.md` |
| 2 | Neural Inference Engine (C4) | ⬜ Not started | — | — | `reports/phase-02-inference.md` |
| 3 | Desktop Telemetry Monitor (C1) | ⬜ Not started | — | — | `reports/phase-03-monitor.md` |
| 4 | Semantic Storage Engine (C3) | ⬜ Not started | — | — | `reports/phase-04-storage.md` |
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
| 2026-09-26 | D-01 | V3.2 blueprint is primary; conflicts resolved per Architecture.md §13. | V3.2 is the newer, more detailed specification. | ADR-0001 (to write in P0) |
| 2026-09-26 | D-02 | healthd is a standalone binary, built first. | Out-of-process resilience (V3.2). | — |
| 2026-09-26 | D-03 | Build C1 (monitor) before C3 (storage). | The Phase 4 soak-replay gate needs real captured telemetry dumps. | ADR-0004 (to write in P0) |
| 2026-09-26 | D-04 | IPC = filesystem UDS + u32-LE length-prefixed protobuf; memfd + SCM_RIGHTS for the token ring. | Works under PrivateNetwork/PrivateDevices; one contract for 3 languages. | ADR-0003 (to write in P0) |
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
| 2026-09-26 | Risk | R-01: `PrivateNetwork=true` in systemd user units vs Ubuntu 24.04 userns restriction and `SO_PEERCRED` under `PrivateUsers`. Resolve with spike S-01 in Phase 0. | Architect | Watching |
| 2026-09-26 | Risk | R-02: BitNet decode speed on Zen 3 unverified. First signal from spike S-04, full answer in Phase 2. | Architect | Watching |

Tech debt register (add as it appears):

| Date | Item | Introduced in | Plan to repay |
| :--- | :--- | :--- | :--- |
| 2026-09-26 | `deny.toml` empty stub; `cargo deny check` not wired into `just ci` (license/bans policy undefined, only `just deny` exists standalone). | P0-S01 | P0-S10 |
| 2026-09-26 | System has `clang-format-18`/`clang-tidy` (no unversioned `clang-format` alias); justfile calls `clang-format-18` explicitly. `cargo-nextest` and `shellcheck` installed manually this session (were missing from env, see memory.md §10). | P0-S01 | none needed — document only |
| 2026-09-26 | `neuroos-ipc` branch coverage not measured: `cargo llvm-cov --branch` needs `-Z coverage-options=branch`, nightly-only. Line/region coverage (87.5%/88%) already exceeds the 80% bar. | P0-S03 | install/pin a nightly toolchain for coverage only, or accept line coverage as the working proxy — owner to decide |
| 2026-09-26 | Real cross-UID `SO_PEERCRED` rejection (IT: `sudo -u nobody` or second local user) not exercised — sandbox has no second UID/root. `peercred::is_allowed` decision function is unit-tested directly instead. | P0-S03 | exercise for real during Phase 0 hardening pass, if a suitable CI runner is available |
| 2026-09-26 | No C++ tests/executable entry point yet for `neuroos-voice` (only a static lib; Architecture.md §14 lists no `main.cpp` for it). `neuroos-inference` has a stub `main.cpp` returning 0. | P0-S01 | Phase 6 (voice), Phase 2 (inference) |

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
| Audio | PipeWire present |
| Not yet installed / checked | `cargo-llvm-cov`, ONNX Runtime, `libpipewire-0.3-dev`, `espeak-ng` |

---

## 11. Session Log

Newest first. One entry per work session.

| Date | Session summary | Stories touched | Next step |
| :--- | :--- | :--- | :--- |
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
