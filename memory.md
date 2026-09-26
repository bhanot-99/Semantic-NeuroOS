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
| Project stage | Phase 0 — all 10 stories done, exit criteria check + phase report pending |
| Current phase | **Phase 0 — Foundation, Contracts & Spikes** (stories done, gate pending) |
| Current sprint | Sprint 0 (started 2026-09-26) |
| Current story | — (all P0-S01…S10 done; next: verify exit criteria §3.4, write `reports/phase-00-foundation.md`, gate to Phase 1) |
| Overall progress | 0 / 11 phases complete (10 / 10 Sprint-0 stories done, all on branch `p0/s01-just-ci-green`, not yet merged to `main`) |
| Health | 🟢 On track |
| Next milestone | M0 Foundation |

---

## 2. Currently Working On

| Field | Value |
| :--- | :--- |
| Story | — (all P0-S01…S10 done) |
| File(s) being edited | — |
| Branch | `p0/s01-just-ci-green` (all of P0-S01…S10 built here per owner's direction; not yet merged to `main`) |
| Started | — |
| Goal of this session | — |
| Next concrete step | Verify Phase 0 exit criteria (phases.md §3.4) one by one, write `reports/phase-00-foundation.md`, decide whether to merge `p0/s01-just-ci-green` to `main` before or as part of the phase gate. Blueprints still need moving to `docs/blueprints/` (deferred since P0-S01, still not done). |

---

## 3. Completed Work Log

Newest first. One line per meaningful unit of work. Format: `YYYY-MM-DD · [Phase/Story] · what was done · evidence/link`.

| Date | Phase / Story | Completed | Evidence |
| :--- | :--- | :--- | :--- |
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

**Sprint 0 goal:** repository, toolchains, contracts and shared IPC libraries in place; spikes S-01 and S-02 started. **All 10 stories done as of 2026-09-26** — the "Sprint 0 / Sprint 0b" split in earlier revisions of this table was this file's own internal organization, not a phases.md split; phases.md §3.2 lists all 10 as one set and all 10 are now complete.

| Story | Title | Pts | Status |
| :--- | :--- | :--- | :--- |
| P0-S01 | `just ci` green on clean checkout (scaffold, toolchains, justfile) | 5 | Done |
| P0-S02 | Proto v1 contracts + codegen for Rust/C++/Python | 5 | Done |
| P0-S03 | `neuroos-ipc` (framing, UDS, SO_PEERCRED, deadlines, reconnect) | 8 | Done |
| P0-S04 | `neuroos-health` endpoint + histograms | 3 | Done |
| P0-S05 | systemd templates with hardening baseline | 3 | Done |
| P0-S06 | Spike S-01 unit mode → ADR-0002 | 5 | Done |
| P0-S07 | Spike S-02 memfd seqlock ring | 5 | Done |
| P0-S08 | Spikes S-03 (COSMIC) + S-04 (bitnet.cpp) | 5 | Done |
| P0-S09 | Model manifest + fetch script | 3 | Done |
| P0-S10 | cargo-deny network-crate ban | 2 | Done |

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
| 2026-09-26 | Risk | R-02: BitNet decode speed on Zen 3 unverified. | Architect | **First signal in** — spike S-04 (P0-S08, ADR-0006): 17.89 t/s decode, 17.07 t/s prefill, real 2.41B-param i2_s model, 8 threads, this reference machine, AVX2 confirmed active (`-march=native`). Full judgment against Phase 2's latency budget still pending. |

Tech debt register (add as it appears):

| Date | Item | Introduced in | Plan to repay |
| :--- | :--- | :--- | :--- |
| 2026-09-26 | `deploy/systemd/*.service` (Zone 2/3 components) are still system templates (`User=%i`) from P0-S05; ADR-0002 (P0-S06) recommends reworking them to user-scope units instead. Mechanical rework (drop `User=`/`Group=`/`neuroos@%i.target` plumbing, retarget `WantedBy=`), not new design. | P0-S05, superseded by P0-S06/ADR-0002 | next session touching `deploy/systemd/` |
| 2026-09-26 | `deny.toml` empty stub; `cargo deny check` not wired into `just ci` (license/bans policy undefined, only `just deny` exists standalone). | P0-S01 | P0-S10 |
| 2026-09-26 | System has `clang-format-18`/`clang-tidy` (no unversioned `clang-format` alias); justfile calls `clang-format-18` explicitly. `cargo-nextest` and `shellcheck` installed manually this session (were missing from env, see memory.md §10). | P0-S01 | none needed — document only |
| 2026-09-26 | All 10 real models (~2.5 GiB total) fetched and verified into `/home/bhanot/neuroos-models-test` (outside the repo, `NEUROOS_MODELS_DIR` override) — not `/opt/neuroos/models` (needs root, not done this session). Kokoro voice choice (`af_heart`) is this manifest's pick, not an OQ/ADR decision — revisit if the owner wants a different default voice. | P0-S09 | `scripts/install.sh` (not yet written) does the real `/opt/neuroos/models` install; revisit voice choice whenever voice UX is actually designed (Phase 6) |
| 2026-09-26 | `cpp/third_party/bitnet.cpp` submodule needs `git submodule update --init --recursive` after a fresh clone (not automatic, not yet documented in a README quick-start — README.md itself predates this and is still a P0-S01 stub). Not wired into `cpp/CMakeLists.txt` or `just build` yet — Phase 2 does that. Its `build/` (807M) and `models/` (1.2G, the real downloaded GGUF) are untracked, left in place per owner's choice this session. | P0-S08 | note in README when it's written for real; Phase 2 wires it into the real build |
| 2026-09-26 | Rust ThreadSanitizer runs (used to verify `neuroos-shm`, ADR-0005) need a local `nightly` toolchain + `rust-src` component, installed this session but not part of the pinned `rust-toolchain.toml` (D-07) or `just ci`. C++ TSan (`tsan` CMake preset) needed `setarch $(uname -m) -R` to work around an unrelated ASLR/mmap-placement TSan issue on this machine. | P0-S07 | document only; re-derive the exact commands from ADR-0005 if `neuroos-shm`'s concurrency logic changes |
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
