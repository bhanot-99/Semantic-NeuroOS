# PRD — Semantic-NeuroOS

**Document type:** Product Requirements Document
**Product:** Semantic-NeuroOS (working name "NeuroOS")
**Product version targeted:** v1.0 (implements Blueprint V3.2, reconciled with Blueprint V2.7)
**Document version:** 1.0
**Status:** Baseline, approved for Phase 0
**Date:** 2026-09-26
**Source inputs:** `semantic_neuroos_implementation_blueprint.md` (V3.2, primary), `neuroos_v2_modular_blueprint-v6.md` (V2.7, secondary)
**Companion documents:** [Architecture.md](Architecture.md) · [rules.md](rules.md) · [phases.md](phases.md) · [design.md](design.md) · [memory.md](memory.md)

---

## 1. Product Summary

Semantic-NeuroOS is an **on-device, privacy-first desktop intelligence layer for Linux**. It watches what the user is doing on their desktop (with their consent), builds a private semantic memory of that activity, and answers spoken questions about it using a small local language model. It can also run a small set of actions ("skills") on the user's behalf, under a strict safety firewall.

The defining property is the **Zero-Egress Guarantee**. Every component that touches personal data runs inside a network-isolated sandbox (`PrivateNetwork=true`). Exactly one component, the External Fetcher, may reach the internet. It runs as a separate system user, only on explicit user approval, and every byte it brings back is marked as untrusted ("tainted").

### 1.1 One-sentence pitch

> "A voice assistant that knows what you are working on right now, remembers what you worked on last week, and never sends a single byte of your life to the cloud."

### 1.2 The problem

| Problem | Today's reality | Consequence |
| :--- | :--- | :--- |
| Context switching | Developers and knowledge workers lose track of what they were doing, which error they saw, which doc they read. | Time lost re-finding information. |
| Cloud assistants need your data | Mainstream assistants stream screen content, audio and files to remote servers. | Unacceptable for privacy-conscious users, regulated work, and airgapped environments. |
| Local assistants lack context | Local LLM chat apps are blind to the desktop. The user must copy and paste context manually. | High friction, low value. |
| Agentic tools are unsafe | Assistants that can act (run tools, read calendars, fetch URLs) are open to prompt injection from web content. | Real risk of data exfiltration or unwanted actions. |

### 1.3 The solution

1. **Sense** — Component 1 records desktop context: focused window, idle state, media, system load, process trees, and chosen folders (git repos, notes, calendar files).
2. **Remember** — Component 3 filters the noise (compiler subprocesses, its own windows, passive media), embeds what matters, and stores it locally in LanceDB + SQLite.
3. **Understand** — Component 5 resolves "this" and "it" by snapping to what was on screen at the moment the user spoke (±1.5 s), retrieves evidence, and builds a compact 512-token prompt. A background worker learns work rhythms and relationships as a knowledge graph.
4. **Answer** — Component 4 runs BitNet b1.58 2B4T on the CPU. Component 2 speaks the answer back, masking latency with an instant spoken preamble.
5. **Act safely** — Component 6 gates every skill through capability tiers (SAFE / REVIEW / DANGEROUS). Untrusted web content automatically escalates SAFE skills to REVIEW, which requires an HMAC-bound user confirmation.
6. **Research on request** — Component 7 fetches a URL only after user approval, blocks SSRF, and tags the result as `EXTERNAL_UNTRUSTED`.
7. **Stay healthy** — `neuroos-healthd` monitors every component out-of-process and enforces soak-test thresholds.

---

## 2. Goals and Non-Goals

### 2.1 Product goals (v1.0)

| ID | Goal | Measure of success |
| :--- | :--- | :--- |
| G-1 | Useful context-aware answers by voice | ≥ 80% of scripted evaluation questions answered correctly (see §8, KPI-1) |
| G-2 | Absolute privacy | 0 bytes of egress from Components 1–6 and healthd, proven by test in Phase 9 |
| G-3 | Feels responsive | Spoken preamble starts ≤ 50 ms after the transcript is ready; first answer audio ≤ 2.0 s p50 |
| G-4 | Safe by construction | 100% of tainted-context skill calls escalate; 0 skills execute without an `ATTEMPT` + `RESULT` audit pair |
| G-5 | Runs on a normal laptop | Total steady-state RSS ≤ 2,475 MiB; idle CPU ≤ 3% on reference hardware (§6.2: C3's share was corrected upward in ADR-0014) |
| G-6 | Stable for long sessions | 24 h soak: RSS growth < 5%, p99 latency drift < 10% |

### 2.2 Non-goals (explicitly out of scope for v1.0)

- Cloud sync, remote accounts, telemetry to the developer, or any "phone home" behaviour.
- A full graphical application. v1.0 is voice-first plus a CLI (`neuroosctl`). The only GUI surfaces are the confirmation dialog and the static `graph_view.html` file.
- GPU acceleration. v1.0 is CPU-only (AVX2).
- Windows, macOS, mobile, and X11 sessions.
- Multi-user or multi-seat installations. One install serves one desktop user.
- Autonomous agents that chain actions without confirmation outside the SAFE tier.
- A general-purpose web browsing agent. The fetcher retrieves single, user-approved URLs only.
- Screen capture, OCR, keystroke logging, or microphone recording outside active voice sessions.

---

## 3. Target Users

### 3.1 Primary persona — "The Privacy-First Developer"

| Attribute | Detail |
| :--- | :--- |
| Who | Software engineer on Linux (Pop!_OS / COSMIC, Fedora, Arch with a wlroots compositor). |
| Daily context | Terminals, `cargo build`, editors, browsers with docs, git, notes in markdown. |
| Needs | "What was that error?", "Summarize this bug", "What did I do on the parser yesterday?", hands-free while debugging. |
| Pain points | Cloud assistants are banned by their employer or conflict with their values. Local chat LLMs are blind to their desktop. |
| Success looks like | They ask about "this" and the assistant knows what "this" is, and they can prove nothing left the machine. |

### 3.2 Secondary persona — "The Security-Sensitive Professional"

| Attribute | Detail |
| :--- | :--- |
| Who | Security researcher, journalist, legal or medical professional, or anyone working in a regulated or airgapped environment. |
| Needs | Verifiable zero egress, an audit trail of every action, explicit consent for any network access. |
| Success looks like | They can audit `actions.jsonl`, inspect systemd sandbox stanzas, and trust the tier model. |

### 3.3 Tertiary persona — "The Hands-Free Knowledge Worker"

| Attribute | Detail |
| :--- | :--- |
| Who | A writer, researcher or student who prefers voice, or who has an accessibility need (RSI, low vision). |
| Needs | Reliable wake word, natural voice, the ability to interrupt the assistant (barge-in), answers about their notes and calendar. |

### 3.4 Internal persona — "The Operator / Maintainer"

The project owner, who builds, installs and debugs the system. They need `neuroosctl status`, health reports, benchmark reports, reproducible builds, and clear logs.

### 3.5 Anti-personas (we do not design for)

- IT administrators wanting fleet-wide management.
- Users who want a cloud-quality LLM (GPT-class reasoning). NeuroOS trades raw power for privacy.
- Users on hardware without AVX2 or with less than 8 GB RAM.

---

## 4. Platform and Constraints

| Constraint | Value |
| :--- | :--- |
| OS | Linux, x86_64, kernel ≥ 6.7 (Landlock ABI v4) |
| Reference hardware | AMD Ryzen 7 5800H (Zen 3, 8C/16T, AVX2, no AVX-512), 16 GB RAM, Pop!_OS 24.04 LTS, COSMIC on Wayland |
| Minimum hardware | 4 cores with AVX2, 8 GB RAM, 6 GB free disk (models ~2.5 GB + data) |
| Display server | Wayland only. COSMIC (`zcosmic_toplevel_info_v1`) is primary. wlroots compositors (`zwlr_foreign_toplevel_manager_v1`) are secondary. |
| Audio | PipeWire ≥ 1.0 |
| Init | systemd ≥ 255 |
| Network | Components 1–6 and healthd: none. Component 7: egress only on approval. |
| Memory budget | ≤ 2,475 MiB RSS total (§6.2) |
| Models | Shipped as local files under `/opt/neuroos/models/`. Never downloaded at runtime by any component. |

---

## 5. Core Features

Priority key: **P0** = required for v1.0 · **P1** = should have in v1.0 · **P2** = could have, post-v1.0 candidate.

| ID | Feature | Priority | Owning component(s) |
| :--- | :--- | :--- | :--- |
| F-01 | Wake-word voice interaction with barge-in | P0 | C2, C5 |
| F-02 | Desktop context sensing (telemetry) | P0 | C1 |
| F-03 | Semantic memory: filtered ingest, embedding, storage, retrieval | P0 | C3 |
| F-04 | Deictic resolution ("this", "it", "that") | P0 | C5, C3 |
| F-05 | Local LLM answer generation with streamed speech | P0 | C4, C2 |
| F-06 | Two-tier evidence pipeline (fast truncation + async distillation) | P0 | C5, C4 |
| F-07 | SafetyGate skill execution with tiers, taint escalation and HMAC confirmation | P0 | C6 |
| F-08 | User-approved web fetch with anti-SSRF and taint tagging | P0 | C7, C6, C3 |
| F-09 | Tamper-evident audit trail (`actions.jsonl`) | P0 | C6 |
| F-10 | Out-of-process health monitoring and soak harness | P0 | healthd |
| F-11 | Privacy controls: pause sensing, app exclusion list, forget a time range | P0 | C1, C3, CLI |
| F-12 | Data lifecycle: retention TTLs, daily GC, 6-hourly backups, re-index on model change | P0 | C3 |
| F-13 | Operator CLI `neuroosctl` (status, ask by text, audit tail, graph, pause, forget) | P0 | CLI |
| F-14 | Background knowledge graph learning (APPNP, Hawkes rhythm model, 72 h edge TTL) | P1 | C5 (cold worker) |
| F-15 | On-demand knowledge graph visualisation (`graph_view.html`) | P1 | C5 |
| F-16 | One consistent, human-quality voice (Kokoro-82M, 24 kHz) for every spoken word, with natural conversational turn-taking | P0 | C2, C5 |
| F-17 | SQLite v1 → v2 migration tool | P1 (conditional, see OQ-03) | C3 |
| F-18 | Folder sensors for git repos, notes vault and ICS calendar | P1 | C1 |
| F-19 | wlroots compositor support | P1 | C1 |
| F-20 | Heads-up display (HUD) overlay | P2 | future |
| F-21 | Latency-driven HNSW index promotion (USearch) | P1 | C3 |

### 5.1 Feature detail and functional requirements

Each functional requirement (FR) has an ID used by [phases.md](phases.md) user stories and test plans.

#### F-01 Voice interaction (C2 `neuroos-voice`)

| ID | Requirement |
| :--- | :--- |
| FR-VOI-01 | Capture 16 kHz mono PCM from PipeWire continuously while the service runs. Never persist raw audio to disk. |
| FR-VOI-02 | Run Silero VAD (threshold 0.75) on every frame. Run openWakeWord only while VAD reports speech. |
| FR-VOI-03 | On wake word ("hey jarvis" default, configurable), capture the utterance until VAD end-of-speech (silence ≥ 600 ms, configurable). |
| FR-VOI-04 | Transcribe with whisper.cpp (`ggml-tiny.en-q5_1.bin`) and emit `VoiceCommand{text, t_speech_start_ns, t_speech_end_ns}` to C5. |
| FR-VOI-05 | Play an acoustic preamble requested by C5 with ≤ 50 ms time-to-first-audio (TTFA). Preambles are **pre-rendered Kokoro clips** (same voice as answers), cached in memory at startup and regenerated whenever the voice setting changes. |
| FR-VOI-06 | Stream answer text from C4's shared-memory ring and speak it with Kokoro-82M at 24 kHz (≈ 240 ms stream latency), synthesising sentence by sentence so pauses and intonation sound natural. |
| FR-VOI-09 | **Single voice rule:** every sound NeuroOS speaks (preambles, answers, errors, confirmations) uses the same Kokoro voice and speed. No second TTS engine exists. |
| FR-VOI-10 | If Kokoro fails, C2 plays pre-rendered Kokoro clips for status lines ("My voice is restarting, one moment") and restarts the engine; it never switches to a different voice. |
| FR-VOI-11 | Conversation mode: after an answer, C2 keeps listening for 8 s (configurable) for a follow-up **without** the wake word, so a back-and-forth feels like talking to a person. |
| FR-VOI-12 | Natural prosody: C5 writes answers as short spoken sentences (contractions, no lists, no markdown); C2 inserts natural pauses at sentence and clause boundaries. |
| FR-VOI-07 | Barge-in: when VAD detects user speech during playback, flush the PipeWire output buffer in < 1 ms and cancel the in-flight generation. |
| FR-VOI-08 | Idle CPU use with no speech < 1% of one core. |

#### F-02 Desktop sensing (C1 `neuroos-monitor`)

| ID | Requirement |
| :--- | :--- |
| FR-MON-01 | Track the focused toplevel window (`app_id`, title, focus start/end) via `zcosmic_toplevel_info_v1`, or `zwlr_foreign_toplevel_manager_v1` on wlroots. |
| FR-MON-02 | Track user idle/active state via `ext_idle_notify_v1` (default timeout 60 s). |
| FR-MON-03 | Observe D-Bus MPRIS players (playback state + metadata). |
| FR-MON-04 | Sample system CPU and memory every 5 s (configurable). |
| FR-MON-05 | Snapshot the process tree (`/proc`) for the focused window's PID so C3 can collapse subprocesses. |
| FR-MON-06 | Emit every observation as `RawTelemetryEvent` with UTC nanosecond timestamps. C1 never filters or judges data. |
| FR-MON-07 | Event capture latency < 1.5 ms from compositor event to socket write. |
| FR-MON-08 | Honour the pause state and the app exclusion list before emitting (the only "filter" in C1, required for privacy). |
| FR-MON-09 | (P1) Watch configured folders (git repos, notes vault, ICS files) via inotify and emit file activity events. |
| FR-MON-10 | Provide a `--record <file>` mode that writes the event stream to a replay dump for test fixtures. |

#### F-03 Semantic memory (C3 `neuroos-storage`)

| ID | Requirement |
| :--- | :--- |
| FR-STO-01 | Run the synchronous 4-stage ingest filter on every event: (1) PPID process-tree collapse, (2) self-observation exclusion, (3) promotion gate (N ≥ 3 occurrences or > 5 s active dwell), (4) MPRIS media demotion. |
| FR-STO-02 | Map promoted events to one of 13 domain adapters and attach `TaintFlags`. |
| FR-STO-03 | Own the single FastEmbed ONNX model (`bge-small-en-v1.5`, 384-D). No other component loads an embedding model. |
| FR-STO-04 | Store vectors and chunks in LanceDB; store relational metadata (focus history, counters, entities, edges) in SQLite (WAL mode). |
| FR-STO-05 | Answer `QueryHybridVectorText(text, top_k)` in ~11–13 ms total (embedding 9.5–12 ms + exact SIMD flat search < 1 ms up to 20,000 items). |
| FR-STO-06 | Answer `QueryFocusHistory(t, ±window)` for deictic snapping. |
| FR-STO-07 | Promote a collection to a USearch HNSW index only when its p99 search latency exceeds 5.0 ms. No item-count thresholds. |
| FR-STO-08 | Ingest fetcher spool payloads, always tagged `EXTERNAL_UNTRUSTED`. |
| FR-STO-09 | Retention: raw telemetry 14 days, aggregates 90 days, notes/git/commits permanent. Daily GC vacuum. |
| FR-STO-10 | Backups: atomic snapshot of SQLite (WAL checkpoint) and LanceDB every 6 hours to `~/.local/share/neuroos/backups/`, keep the last 8. |
| FR-STO-11 | Record `embedding_model_id` in index metadata. On change, re-index in the background without blocking queries. |
| FR-STO-12 | Support "forget": delete every record within a time range or from an app, including vectors and backups older than the next backup cycle. |

#### F-04 / F-06 Understanding (C5 `neuroos-knowledge`)

| ID | Requirement |
| :--- | :--- |
| FR-KNO-01 | Deictic snapping: resolve deictic words by querying C3 focus history at `t_speech_start ± 1.5 s`. Choose the window with the longest overlap. |
| FR-KNO-02 | On receipt of a `VoiceCommand`, request an acoustic preamble from C2 before any retrieval work begins. |
| FR-KNO-03 | Retrieve evidence from C3 (top_k = 5 default). |
| FR-KNO-04 | Fast path: truncate evidence to 320 tokens in < 5 ms, always. |
| FR-KNO-05 | Slow path: when raw evidence > 1,000 tokens, also spawn an asynchronous distillation job on C4 (≈ 6.0 s budget) that warms a cache for follow-up questions. It never blocks the current answer. |
| FR-KNO-06 | Assemble a prompt with a hard 512-token cap: 128 system + 64 deictic + 320 evidence. |
| FR-KNO-07 | Wrap any tainted evidence in `<untrusted_external_doc taint="true">…</untrusted_external_doc>` and propagate the union of all evidence taint flags to C6. |
| FR-KNO-08 | Ask C6 to evaluate the capability of every generation and skill dispatch before execution. |
| FR-KNO-09 | Hot query path total < 5 ms of C5-owned compute (excluding C3/C4 calls). |
| FR-KNO-10 | (P1) Cold worker: APPNP sparse power iteration (10 iterations), Hawkes excitation (decay 0.05), prune unreinforced edges after 72 h. |
| FR-KNO-11 | (P1) Write a self-contained `graph_view.html` (inline D3, inline fonts, no network) on demand. |

#### F-05 Inference (C4 `neuroos-inference`)

| ID | Requirement |
| :--- | :--- |
| FR-INF-01 | Load BitNet b1.58 2B4T GGUF via read-only `mmap` from `/opt/neuroos/models/`. |
| FR-INF-02 | Default context cap 512 tokens, configurable up to 4,096. |
| FR-INF-03 | Stream generated text through a shared-memory seqlock ring buffer (memfd) to C2, zero-copy. |
| FR-INF-04 | Support GBNF grammar-constrained output (used for skill-call JSON). |
| FR-INF-05 | Support cancellation of an in-flight generation within 1 decode step. |
| FR-INF-06 | Two priority lanes: interactive (preempts) and background distillation (yields). |
| FR-INF-07 | Performance targets on reference hardware: prefill ≈ 2.5 ms/token, decode ≈ 45 ms/token; verified by Phase 2 benchmark at 128/512/1,024 tokens. |

#### F-07 / F-09 SafetyGate kernel (C6 `neuroos-kernel`)

| ID | Requirement |
| :--- | :--- |
| FR-KER-01 | Object-capability skill registry: each skill declares exactly one capability and a base tier. |
| FR-KER-02 | Tiers: **SAFE** (auto-execute), **REVIEW** (requires confirmation), **DANGEROUS** (requires confirmation with typed phrase; disabled by default). |
| FR-KER-03 | Taint escalation: if the context carries `EXTERNAL_UNTRUSTED`, SAFE becomes REVIEW. REVIEW and DANGEROUS stay as they are. Tiers never go down. |
| FR-KER-04 | Confirmation: show a desktop dialog; bind the decision with HMAC-SHA256 over `(action_id, nonce, capability, args_hash, decision, expiry)`. Reject replayed, expired or forged tokens. |
| FR-KER-05 | Execute each skill in a sandboxed child process with a per-skill Landlock ruleset and seccomp filter. |
| FR-KER-06 | Write paired `ATTEMPT` / `RESULT` records to `actions.jsonl` for every dispatch, including rejections. Rotate at 10 MB, keep 5 generations. Records are hash-chained. |
| FR-KER-07 | Capability check latency < 0.5 ms (excluding user confirmation time). |
| FR-KER-08 | v1.0 skill catalogue: `system.read_metadata` (SAFE), `calendar.read` (SAFE), `notes.search` (SAFE), `memory.forget` (REVIEW), `notes.append` (REVIEW), `web.fetch` (REVIEW). `shell.exec` (DANGEROUS) exists but is disabled by default. |

#### F-08 External fetch (C7 `neuroos-fetcher`)

| ID | Requirement |
| :--- | :--- |
| FR-FET-01 | Run as the dedicated system user `neuroos-fetcher` (group `neuroos`), created by `systemd-sysusers`. |
| FR-FET-02 | Accept fetch requests only from C6 over `/run/neuroos-fetcher/fetch.sock` (mode 0660, verified via `SO_PEERCRED`). |
| FR-FET-03 | Anti-SSRF: resolve DNS once, reject loopback, private, link-local, CGNAT, multicast and reserved ranges (IPv4 and IPv6), pin the connection to the validated IP, and re-validate every redirect (max 3). |
| FR-FET-04 | Allow only `https` (and `http` behind a config flag), `GET` only, content types HTML/JSON/plain text/markdown, max 5 MiB body, 15 s total timeout. |
| FR-FET-05 | Write payloads to `/var/spool/neuroos-fetcher/<doc_id>.json` (dir mode 2770, file mode 0660) with `taint: EXTERNAL_UNTRUSTED`. |
| FR-FET-06 | C3 detects new payloads via inotify on the spool directory. The fetcher never writes into the user's home or runtime directory. |

#### F-10 Health (`neuroos-healthd`)

| ID | Requirement |
| :--- | :--- |
| FR-HLT-01 | Standalone binary, independent of every other component. |
| FR-HLT-02 | Scrape each component's health socket every 30 s (`status`, `rss_bytes`, `p50/p99` latencies, error counters, uptime). |
| FR-HLT-03 | Read cgroup memory (`memory.current`, `memory.peak`) for each unit. |
| FR-HLT-04 | Soak mode: track RSS growth and p99 drift against a baseline; flag breaches of 5% RSS growth or 10% p99 drift. |
| FR-HLT-05 | Expose an aggregate report to `neuroosctl status`. |

#### F-11 Privacy controls

| ID | Requirement |
| :--- | :--- |
| FR-PRV-01 | `neuroosctl pause [duration]` stops all sensing (C1) and wake-word listening (C2) until resumed. |
| FR-PRV-02 | App exclusion list (e.g. password managers, private browser windows) configured in `config.toml`. Excluded apps are never emitted by C1. Defaults include common password managers. |
| FR-PRV-03 | `neuroosctl forget --since 1h` / `--app <app_id>` purges data from C3 (routed through C6 as `memory.forget`, REVIEW tier). |
| FR-PRV-04 | The microphone is only transcribed after the wake word. Raw PCM lives only in a bounded in-memory ring. |

#### F-13 Operator CLI (`neuroosctl`)

| ID | Requirement |
| :--- | :--- |
| FR-CLI-01 | `status` (healthd aggregate), `ask "<text>"` (text query path, identical to voice path after STT), `audit tail`, `graph open`, `pause/resume`, `forget`, `bench`, `replay <dump>`. |
| FR-CLI-02 | Human-readable output by default, `--json` for machines. Follows [design.md](design.md) §8. |

---

## 6. Non-Functional Requirements

### 6.1 Performance (reference hardware)

| ID | Metric | Target | Measured at |
| :--- | :--- | :--- | :--- |
| NFR-PERF-01 | Preamble TTFA (transcript ready → first preamble audio sample) | ≤ 50 ms p95 | Phase 6 |
| NFR-PERF-02 | End-of-speech → first answer audio | ≤ 2.0 s p50, ≤ 3.0 s p95 | Phase 9 |
| NFR-PERF-03 | C3 hybrid query (embed + search) | ≤ 13 ms p50, ≤ 20 ms p99 | Phase 4 |
| NFR-PERF-04 | C5 hot-path own compute | < 5 ms p99 | Phase 5 |
| NFR-PERF-05 | C6 capability check | < 0.5 ms p99 | Phase 7 |
| NFR-PERF-06 | C1 event capture | < 1.5 ms p99 | Phase 3 |
| NFR-PERF-07 | C4 decode | ≤ 45 ms/token p50 at 512 context | Phase 2 |
| NFR-PERF-08 | Barge-in flush | < 1 ms | Phase 6 |

> **Honesty note:** "< 50 ms perceived latency" in the blueprint refers to the spoken preamble only. A full 512-token prefill at 2.5 ms/token costs ≈ 1.3 s before the first answer token. NFR-PERF-02 states the real end-to-end target. Phase 2 replaces these projections with measured numbers.

### 6.2 Resources

| Component | RSS budget | Hard limit (`MemoryMax`) |
| :--- | :--- | :--- |
| C1 monitor | 25 MiB | 40 MiB |
| C2 voice | 405 MiB | 520 MiB |
| C3 storage | 340 MiB | 420 MiB |
| C4 inference | 1,590 MiB | 1,900 MiB |
| C5 knowledge (hot + cold) | 75 MiB | 120 MiB |
| C6 kernel | 15 MiB (+25 MiB transient dialog) | 64 MiB |
| C7 fetcher | 10 MiB | 32 MiB |
| healthd | 15 MiB | 32 MiB |
| **Total** | **2,475 MiB** | — |

> **C3's budget was raised from 205/300 MiB to 340/420 MiB on 2026-10-07
> (ADR-0014, BUGS.md M21).** The original figure was set before C3 had an
> embedding model: `bge-small-en-v1.5` is 126.9 MiB of f32 weights on its
> own, and the arrow/datafusion/lance stack adds ~110 MiB of resident text
> and buffers, so 205 MiB was not reachable by any implementation of this
> design. Measured release-build behaviour: 299–334 MiB across the real
> lifecycle, 392–401 MiB peak during startup. The new figures are those
> measurements plus headroom, so healthd now reports C3 honestly instead of
> DEGRADED on every start. ADR-0014 records the two ways to bring the number
> back down and why neither was taken now.

Idle CPU (no speech, no queries): ≤ 3% of total CPU across all components.

### 6.3 Security and privacy

| ID | Requirement |
| :--- | :--- |
| NFR-SEC-01 | Components 1–6 and healthd run with `PrivateNetwork=true` from the first build (Phase 0 templates). |
| NFR-SEC-02 | Every IPC server verifies peer UID/GID with `SO_PEERCRED` and rejects unknown peers. |
| NFR-SEC-03 | No component except C7 links an HTTP client or DNS resolver (enforced by `cargo-deny` bans and CI grep). |
| NFR-SEC-04 | Landlock restricts every component's filesystem access to its declared paths. |
| NFR-SEC-05 | Models are verified by SHA-256 against `models/manifest.toml` at startup. A mismatch is fatal. |
| NFR-SEC-06 | Secrets (HMAC key) live in a 0600 file created at install, loaded into zeroized memory, never logged. |
| NFR-SEC-07 | Prompt injection resistance: no text from tainted evidence can lower a tier or execute a non-SAFE skill without confirmation. Verified by an adversarial test corpus (Phase 7 and 9). |
| NFR-PRIV-01 | No raw audio, screenshots or keystrokes are ever stored. |
| NFR-PRIV-02 | All user data lives under `~/.local/share/neuroos/` (plus the spool) and is fully removed by `neuroosctl purge --all`. |

### 6.4 Reliability and operability

| ID | Requirement |
| :--- | :--- |
| NFR-REL-01 | Every component restarts on failure (`Restart=on-failure`, backoff), and peers reconnect automatically with jittered exponential backoff. |
| NFR-REL-02 | Loss of any single component degrades features but never crashes others (e.g. C4 down → C2 plays the pre-rendered "I can't think right now" clip in the normal voice). |
| NFR-REL-03 | Storage survives power loss without corruption (SQLite WAL + atomic snapshots). |
| NFR-REL-04 | 24 h soak: RSS growth < 5%, p99 drift < 10%, zero crashes. |
| NFR-OPS-01 | Structured logs to journald with component, trace_id and span fields. No personal content in logs above `debug`. |
| NFR-OPS-02 | One-command build (`just build`), test (`just test`) and full CI (`just ci`). |

### 6.5 Quality

| ID | Requirement |
| :--- | :--- |
| NFR-QA-01 | Line coverage ≥ 80% on Rust library crates and ≥ 70% on C++ and Python modules. Security-critical modules (taint, tier evaluation, SSRF, HMAC) require 100% branch coverage. |
| NFR-QA-02 | Zero warnings under `clippy -D warnings`, `-Wall -Wextra -Werror`, `ruff` and `mypy --strict`. |

---

## 7. Key User Journeys

### UJ-1 "Summarize this bug" (happy path)

1. The user is looking at a terminal showing a failed `cargo build`.
2. The user says "Hey Jarvis, summarize this bug."
3. Within 50 ms of the transcript being ready, the user hears "Checking your files…".
4. NeuroOS snaps "this" to the Alacritty window titled `cargo build - err 104`, retrieves the related build events, and answers aloud in about 2 seconds.
5. The user interrupts with "stop"; audio stops instantly.

### UJ-2 Research with taint (safety path)

1. The user says "Research https://example.com/api-docs."
2. `web.fetch` is REVIEW tier, so a confirmation dialog appears. The user approves.
3. The fetcher downloads the page; C3 ingests it tagged `EXTERNAL_UNTRUSTED`.
4. Later the user asks "Check my calendar events matching api-docs." The evidence is tainted, so `calendar.read` escalates from SAFE to REVIEW and the dialog appears again, explaining why.
5. Whatever the user decides, `actions.jsonl` records `ATTEMPT` and `RESULT`.

### UJ-3 Privacy control

1. The user opens a banking site and says "Hey Jarvis, pause for an hour."
2. Sensing and wake-word listening stop; `neuroosctl status` shows `PAUSED until 15:42`.
3. The next day the user runs `neuroosctl forget --since "2026-10-01 14:00" --until "2026-10-01 15:00"` and confirms. The data is purged.

### UJ-4 Operator health check

1. The operator runs `neuroosctl status` and sees every component, its RSS against budget, p99 latencies and last error.

---

## 8. Success Metrics (KPIs)

| ID | KPI | Target v1.0 | How measured |
| :--- | :--- | :--- | :--- |
| KPI-1 | Answer correctness on the scripted evaluation set (≥ 50 questions over recorded telemetry) | ≥ 80% judged correct | Phase 5 / Phase 9 eval harness, human-graded |
| KPI-2 | Deictic snap accuracy | ≥ 95% correct window | Phase 5 fixture suite |
| KPI-3 | Wake-word false accepts | ≤ 1 per 8 h of ambient audio | Phase 6 |
| KPI-4 | Wake-word false rejects | ≤ 5% | Phase 6 |
| KPI-5 | STT word error rate on the command set | ≤ 12% | Phase 6 |
| KPI-6 | Egress from isolated components | 0 bytes | Phase 9 |
| KPI-7 | Tainted-context escalation rate | 100% | Phase 7 / 9 |
| KPI-8 | Ingest noise reduction (soak-replay gate) | promoted entities ≤ 30, compiler subprocesses = 0, zero-access MPRIS nodes = 0 | Phase 4 |

---

## 9. Release Milestones

| Milestone | Contents | Phases |
| :--- | :--- | :--- |
| M0 Foundation | Repo, contracts, IPC, sandbox templates, spikes resolved | 0 |
| M1 Core Engines | healthd + inference benchmarked | 1, 2 |
| M2 Memory | Monitor + storage with passing soak-replay gate | 3, 4 |
| M3 Understanding | Knowledge hot path + cold worker; `neuroosctl ask` works end-to-end by text | 5 |
| M4 Voice | Full voice loop with barge-in | 6 |
| M5 Safety & Reach | Kernel + fetcher with taint escalation | 7, 8 |
| M6 v1.0 RC | Integrated, 24 h soak passed, packaged | 9, 10 |

---

## 10. Dependencies and Licensing

| Asset | Use | Licence | Note |
| :--- | :--- | :--- | :--- |
| BitNet b1.58 2B4T + `bitnet.cpp` | LLM | MIT | Pin commit and model hash |
| whisper.cpp + `ggml-tiny.en` | STT | MIT | |
| Silero VAD | VAD | MIT | |
| openWakeWord | Wake word | Apache-2.0 (code) / **CC BY-NC-SA 4.0 (pre-trained models)** | **Risk R-06:** non-commercial model licence. Fine for personal use; a custom-trained model is required for any commercial distribution. |
| Kokoro-82M | Neural TTS | Apache-2.0 | |
| espeak-ng | Phonemizer for Kokoro | GPL-3.0 | **Risk R-07:** GPL linkage; isolate or accept GPL for the voice binary |
| bge-small-en-v1.5 | Embeddings | MIT | |
| LanceDB, USearch, ONNX Runtime | Storage / runtime | Apache-2.0 | |

---

## 11. Risks

| ID | Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- | :--- |
| R-01 | `PrivateNetwork=true` in **systemd user units** needs user namespaces; Ubuntu 24.04 restricts unprivileged userns, and `PrivateUsers` breaks `SO_PEERCRED` UID checks. | High | High | Phase 0 spike S-01. Preferred fallback: system template units `neuroos-<c>@<user>.service` with `User=%i`. |
| R-02 | BitNet decode slower than 45 ms/token on Zen 3 | Medium | High | Phase 2 benchmark gate; tune threads; reduce context; shorter spoken answers. |
| R-03 | COSMIC toplevel protocol version changes | Medium | Medium | Abstract the sensor behind a trait; wlroots fallback; spike S-03. |
| R-04 | Kokoro + Whisper exceed the 405 MiB voice budget | Medium | Medium | int8 Kokoro model; measure in Phase 6. |
| R-11 | Kokoro-82M expressiveness is below large cloud voices | Medium | Medium | Phase 6 voice spike compares Kokoro voices and any CPU-real-time alternative; single-voice rule stays. |
| R-05 | Deictic snapping wrong because of window titles with little signal | Medium | Medium | Combine focus overlap with retrieval score; evaluation fixtures. |
| R-06 | openWakeWord model licence (non-commercial) | Certain | Low (personal) / High (commercial) | Document; plan a custom wake-word model before commercial release. |
| R-07 | espeak-ng GPL-3.0 | Certain | Medium | Keep phonemization in the voice binary; review licence stance before distribution. |
| R-08 | Prompt injection via fetched docs | High | High | Taint escalation, XML wrapping, grammar-constrained skill calls, adversarial tests. |
| R-09 | Cold worker's "Neural Hawkes" implies PyTorch, which breaks the 75 MiB budget | High | Medium | v1.0 uses a parametric exponential-kernel Hawkes process in NumPy (OQ-04). |
| R-10 | Blueprint internal inconsistencies | Certain | Low | Resolved in [Architecture.md](Architecture.md) §13. |

---

## 12. Open Questions

| ID | Question | Default until answered | Needed by |
| :--- | :--- | :--- | :--- |
| OQ-01 | Final wake phrase: "hey jarvis" (openWakeWord stock) or custom? | "hey jarvis" | Phase 6 |
| OQ-02 | Which 13 domain adapters exactly? (Proposal in [Architecture.md](Architecture.md) §7.3) | Proposal accepted | Phase 4 |
| OQ-03 | Does a NeuroOS v1 SQLite database exist to migrate? | Build migrator only if a sample DB is provided; otherwise F-17 is dropped | Phase 4 |
| OQ-04 | Is a neural (PyTorch) Hawkes model required, or is parametric Hawkes acceptable for v1.0? | Parametric | Phase 5 |
| OQ-05 | Calendar source: local ICS file(s) only, or Evolution Data Server over D-Bus? | ICS files | Phase 7 |
| OQ-06 | Distribution: personal install only, or public release (affects R-06/R-07)? | Personal | Phase 10 |
| OQ-07 | Notes vault path for the notes domain (e.g. an Obsidian vault)? | `~/Notes` configurable | Phase 3 |

---

## 13. Glossary

| Term | Meaning |
| :--- | :--- |
| Zero-Egress | No network traffic leaves the device from any data-handling component. |
| Deictic snapping | Resolving "this/that/it" to the on-screen object at the time of speech. |
| Taint | A flag on data that came from outside the device; it makes actions stricter. |
| Tier | SAFE / REVIEW / DANGEROUS classification of a skill. |
| TTFA | Time to first audio. |
| Soak test | Long-running stability test (24 h). |
| Seqlock | Lock-free single-writer/multi-reader synchronisation primitive. |
| Spool | Drop directory where the fetcher leaves downloaded documents. |
| Promotion gate | Rule deciding whether an event becomes a persistent node (N ≥ 3 or > 5 s dwell). |
