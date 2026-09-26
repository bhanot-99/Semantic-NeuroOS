# NeuroOS Version 2.0: Modular Architecture & System Blueprint
## Refined 7-Component Architecture Specification & Engineering Blueprint

**Document Title**: System Specification & Modular Component Blueprint for NeuroOS Version 2.0  
**Version**: 2.7 (Production Component Architecture & Hardened Security Blueprint)  
**Date**: September 2026  
**Security Classification**: Airgapped / Zero-Egress Compliant (`PrivateNetwork=true`)  

---

## Executive Summary

**NeuroOS Version 2.0** is an on-device, privacy-first desktop neural operating system operating under a strict **Zero-Egress Guarantee**. Moving away from legacy monolithic SQLite structures and centralized graph bottlenecks, NeuroOS V2.0 organizes desktop intelligence into **7 Decoupled, Standalone Components**.

Data management is driven by **The Semantic Matrix Data Storage Fabric**, which structures desktop context across 13 domain adapters ("Shapeless Blocks"). To handle large evidence sources without violating latency SLAs, retrieval incorporates an **Optional Pre-Assembly Distillation Pipeline** powered by a zero-copy, `mmap`-shared C++ **BitNet b1.58 1.58-bit LLM engine** (`neuroos-inference`).

---

## 1. Resolved Design Issues & Architectural Principles

| Identified Flaw / Issue | Root Cause | Resolved V2.7 Engineering Fix |
| :--- | :--- | :--- |
| **1. Unmatched System Users vs. `$HOME` Paths** | Per-component UIDs (`neuroos-storage`, `neuroos-kernel`) cannot write to user `$HOME` directories. | **Unified User-UID + Landlock Sandboxing**. All internal components (Components 1–6) run under the user's UID using **Landlock LSM** and `systemd` path restriction stanzas (`ProtectHome=tmpfs`, `ReadWritePaths=~/.local/share/neuroos/`). Only Component 7 (Fetcher) runs under a dedicated system user (`neuroos-fetcher`) writing strictly to `/var/spool/neuroos-fetcher/`. |
| **2. Unimplemented Compute-at-Rest & Evidence Truncation** | Fast RAG truncated evidence to 320 tokens without option for semantic distillation. | **Two-Tier Evidence Pipeline**. Adds an explicit Pre-Assembly Distillation path in Component 5: short contexts (<1,000 tokens) use instant <5ms truncation, while large evidence blocks trigger parallel C++ BitNet chunk summarization (100ms budget) before context packing. |
| **3. Flawed Taint Enforcement (No-Op)** | Tainted context restated existing confirmation requirements for REVIEW/DANGEROUS skills, leaving SAFE skills uninhibited. | **Taint Tier Escalation**. Untrusted web data (`TaintFlags::EXTERNAL_UNTRUSTED`) **promotes SAFE skills to REVIEW tier**, revoking auto-execution and requiring explicit user confirmation before any skill executes from a tainted context. |
| **4. Unreachable `/graph/inspect` Server** | `PrivateNetwork=true` loopback namespace blocked browser access to Component 5's HTTP server. | **On-Demand Disk Generation**. Replaced the HTTP server with an on-demand static file generator that writes a self-contained SVG/D3 HTML file (`~/.local/share/neuroos/graph_view.html`) directly to disk for local browser viewing. |
| **5. Roadmap vs. Unit Template Timing Conflict** | Roadmap deferred `PrivateNetwork=true` to Step 5 while text mandated it at Step 1. | **Step 1 Unit Template Mandate**. `PrivateNetwork=true` is included in all systemd service unit templates from **Step 1** day-one builds. |
| **6. Contradictory HNSW Latency/Count Triggers** | Text mixed a static $N < 1,000$ count threshold with $p99 > 5	ext{ms}$ latency rules. | **Pure Latency-Driven Promotion**. Removed item count thresholds. All collections use SIMD-accelerated exact flat search (up to 20,000 items) and promote to HNSW graph indices **only when $p99$ retrieval latency exceeds 5 ms**. |
| **7. Component Count Alignment** | Text referenced 8 components or dropped `neuroos-healthd`. | **Strict 7-Component Layout**. Standardized on 7 standalone components, with `neuroos-healthd` integrated cleanly as a core service inside Component 6 (`neuroos-kernel`). |

---

## 2. Decoupled 7-Component System Architecture

```
+---------------------------------------------------------------------------------------------------+
|                                     NEUROOS V2.7 SYSTEM BUS                                       |
|               (Unix Domain Sockets & POSIX SHM Seqlock — Isolated Loopback Fabric)                |
+---------------------------------------------------------------------------------------------------+
       |            |             |             |             |             |             |
       v            v             v             v             v             v             v
+------------+ +------------+ +------------+ +------------+ +------------+ +------------+ +------------+
| COMP. 1    | | COMP. 2    | | COMP. 3    | | COMP. 4    | | COMP. 5    | | COMP. 6    | | COMP. 7    |
| Desktop    | | Voice &    | | Semantic   | | Neural     | | Data       | | SafetyGate | | External   |
| Monitor    | | Audio      | | Storage    | | Inference  | | Refinement | | Execution  | | Fetcher    |
| Telemetry  | | Pipeline   | | Engine &   | | Engine     | | Knowledge  | | Kernel &   | | Spool      |
|            | |            | | Ingest     | |            | | Engine     | | Skill Reg. | |            |
| [neuroos-  | | [neuroos-  | | [neuroos-  | | [neuroos-  | | [neuroos-  | | [neuroos-  | | [neuroos-  |
|  monitor]  | |  voice]    | |  storage]  | |  inference]| |  knowledge]| |  kernel]   | |  fetcher]  |
+------------+ +------------+ +------------+ +------------+ +------------+ +------------+ +------------+
       |            |             |             |             |             |             |
       +------------+-------------+- Systemd PrivateNetwork=true Sandbox ------+             |
                                  (Enforced strictly on Components 1 through 6)               v
                                                                                   +------------------+
                                                                                   | /var/spool/      |
                                                                                   | neuroos-fetcher/ |
                                                                                   +------------------+
```

---

## 3. Comprehensive Component Specifications

### Component 1: Desktop Telemetry & System Monitoring (`neuroos-monitor`)
* **Execution Identity**: User UID (isolated via `systemd` sandboxing and Landlock LSM).
* **Core Purpose**: Emits a raw, unfiltered stream of `RawTelemetryEvent` items over Unix domain sockets (`/run/user/$UID/neuroos/monitor.sock`).
* **Sensing Hooks**: Wayland window handles, `ext_idle_notify_v1` focus dwell time, D-Bus MPRIS media metadata, and system resource sampling.

### Component 2: Voice, Audio & Speech Pipeline (`neuroos-voice`)
* **Execution Identity**: User UID (PipeWire audio group access).
* **Core Purpose**: Real-time voice interaction, wake-word detection (`openWakeWord`), Silero VAD, sub-100ms `Whisper.cpp` STT, sub-50ms Piper ONNX acoustic preambles (*"Checking your files..."*), and Kokoro-82M 24kHz neural vocoding (~240ms stream latency). Sub-1ms hardware audio barge-in flush via PipeWire `pw_stream_flush`.

### Component 3: Semantic Matrix Storage Engine & Ingest (`neuroos-storage`)
* **Execution Identity**: User UID (`ReadWritePaths=~/.local/share/neuroos/storage/`).
* **Synchronous Rust Ingest Filter**: Filters incoming events before storage:
  1. *PPID Process-Tree Collapsing*: Traces `/proc` ancestry to collapse compiler subprocesses (`cc1`, `cc1plus`) into top-level targets (`cargo build`).
  2. *Self-Observation Exclusion*: Ignores `zenity`, `neuroos-hud`, and assistant UI windows.
  3. *Promotion Gate*: Requires $N \ge 3$ occurrences or $>5\text{s}$ active dwell time to promote events to persistent graph nodes.
  4. *Media Demotion*: D-Bus MPRIS media events without active interaction remain transient unindexed state.
* **Storage Engine**: LanceDB disk tables + USearch vector indexing with FastEmbed ONNX embeddings (`bge-small-en-v1.5`, 384D). Applies pure latency-driven HNSW promotion ($p99 > 5\text{ms}$).

### Component 4: Neural Inference Engine (`neuroos-inference`)
* **Execution Identity**: User UID (`ReadOnlyPaths=/opt/neuroos/models/`).
* **Core Purpose**: C++20 `bitnet.cpp` background daemon running BitNet b1.58 2B4T ternary models via POSIX `mmap` weight sharing (~1.41 GiB RAM). Caps context trees at 512 tokens by default (tunable up to 4096 tokens).

### Component 5: Data Refinement & Knowledge Engine (`neuroos-knowledge`)
* **Execution Identity**: User UID.
* **Hot/Cold Architecture**:
  * **Hot Query Path (`neuroos-knowledge-query`)**: Native Rust service executing Deictic Snapping ($\pm 1.5\text{s}$ window), LightRAG context assembly, and Taint XML wrapping (`<untrusted_external_doc taint="true">`). Offers a **Two-Tier Evidence Pipeline**: short items use instant <5ms truncation, while large evidence blocks trigger an optional 100ms distillation call to Component 4. Writes `graph_view.html` directly to disk on demand for local visualization.
  * **Cold Worker (`neuroos-knowledge-background`)**: Python process running APPNP GNN sparse iterations, Neural Hawkes Process work-rhythm modeling, and 72-hour hypothesis TTL edge pruning.

### Component 6: SafetyGate Execution Kernel & Health Aggregator (`neuroos-kernel`)
* **Execution Identity**: User UID (`ReadWritePaths=~/.local/share/neuroos/`).
* **Core Purpose**: Enforces the Object-Capability Skill Registry, SafetyGate tiers (**SAFE**, **REVIEW**, **DANGEROUS**), and `neuroos-healthd` metric collection over `SO_PEERCRED` verified Unix sockets.
* **Taint Escalation**: Inspects prompt context for `TaintFlags::EXTERNAL_UNTRUSTED`. **Promotes SAFE skills to REVIEW tier**, requiring HMAC-SHA256 user confirmation before execution.
* **Audit Trail**: Maintains `~/.local/share/neuroos/actions.jsonl` paired `ATTEMPT`/`RESULT` logs (rotated at 10 MB, 5 generations).

### Component 7: External Network Fetcher & Spool Daemon (`neuroos-fetcher`)
* **Execution Identity**: Dedicated System UID (`neuroos-fetcher`, group `neuroos`).
* **Isolation Boundary**: The *only* component allowed network egress. Downloads web content into `/var/spool/neuroos-fetcher/` (mode `0770`), applying anti-SSRF IP filtering and attaching `TaintFlags::EXTERNAL_UNTRUSTED` to all payloads.

---

## 4. System Memory Budget & Operational Engineering

| Component / Subsystem | Binary / Runtime | Physical RSS Budget | Processing Latency SLA |
| :--- | :--- | :--- | :--- |
| **Component 1 (`neuroos-monitor`)** | Rust (`tokio`) | ~25 MiB | < 1.5 ms event capture |
| **Component 2 (`neuroos-voice`)** | C++20 / ONNX Runtime | ~485 MiB | < 50 ms TTFA (Piper) / ~240 ms (Kokoro) |
| **Component 3 (`neuroos-storage`)** | Rust + FastEmbed ONNX | ~165 MiB | < 5.0 ms retrieval ($p99$) |
| **Component 4 (`neuroos-inference`)** | C++20 (`bitnet.cpp`) | ~1,410 MiB | ~2.5 ms/token prefill; ~45 ms/token decode |
| **Component 4 Buffers & KV Cache** | C++20 Aligned Alloc | ~180 MiB | Dynamic (512-token cap) |
| **Component 5 (`neuroos-knowledge`)** | Rust (Hot) + Python (Cold) | ~75 MiB | < 5.0 ms context assembly |
| **Component 6 (`neuroos-kernel`)** | Rust Core + `healthd` | ~30 MiB | < 1.0 ms capability check |
| **Component 7 (`neuroos-fetcher`)** | Rust Unprivileged Daemon | ~15 MiB | Async background spool |
| **TOTAL ACTIVE FOOTPRINT** | **Polyglot Tri-Engine** | **~2,385 MiB RSS** | **100% On-Device Execution** |

### Operational Data Engineering
* **Retention & GC**: Daily cleanup job enforces 14-day telemetry TTL, 90-day aggregate TTL, and permanent retention for user notes and git commits.
* **Model Versioning**: Tracks `embedding_model_id` in index metadata; non-blocking background re-indexing is triggered upon model updates.
* **Backups**: Atomic 6-hour SQLite WAL checkpoints and LanceDB index snapshots saved to `~/.local/share/neuroos/backups/`.
* **UTC Normalization**: Sensor hooks convert all incoming desktop timestamps to ISO-8601 UTC Nanoseconds (`u64` Epoch).

---

## 5. 6-Step Implementation Roadmap

```text
Step 1: Core Base & Healthd  ---> Step 2: Storage & Migration ---> Step 3: Knowledge Engine
(PrivateNetwork=true day 1)       (LanceDB + Ingest Filter)        (Hot Query + Distillation)
                                                                            |
                                                                            v
Step 6: Integration & Soak   <--- Step 5: Security & Fetcher   <--- Step 4: Telemetry & Voice
(24h Soak Harness + Tests)        (Landlock Kernel + Spool)        (Wayland + Audio Pipeline)
```

1. **Step 1 (Core Base, Inference & Healthd)**: Build `neuroos-inference` and `neuroos-healthd`. Include `PrivateNetwork=true` in all systemd unit templates from day one. Run benchmark suites across 128/512/1024 token contexts to replace prefill latency projections with empirical measurements.
2. **Step 2 (Storage Engine & Migration)**: Build Component 3 (`neuroos-storage`) with the synchronous Rust ingest filter, FastEmbed ONNX embedder, and LanceDB/USearch storage. Execute the SQLite v1 $\rightarrow$ v2 schema transformer script.
3. **Step 3 (Knowledge Engine & Context Assembly)**: Build Component 5 (`neuroos-knowledge`), implementing the hot/cold split, LightRAG context assembly (with optional 100ms Component 4 distillation call for oversized evidence), and on-demand `graph_view.html` generation. Validate recall on 320-token evidence allocations using a mock focus stream.
4. **Step 4 (Telemetry Sensing & Voice Pipeline)**: Build Component 1 (`neuroos-monitor`) and Component 2 (`neuroos-voice`), connecting raw Wayland/MPRIS event streams and PipeWire sub-1ms audio barge-in flushing.
5. **Step 5 (Security Kernel & External Fetcher)**: Build Component 6 (`neuroos-kernel`) with the Object-Capability Skill Registry, Landlock LSM sandbox, Taint escalation logic, and `actions.jsonl` audit logging. Deploy Component 7 (`neuroos-fetcher`) as a dedicated system user writing to `/var/spool/neuroos-fetcher/`.
6. **Step 6 (Full System Integration & 24h Soak Test)**: Execute end-to-end integration tests, verify zero network egress under systemd sandboxing, and run the 24-hour `neuroos-healthd` soak harness (verifying RSS growth $<5\%$ and $p99$ latency drift $<10\%$).

---
*Document generated for the NeuroOS Version 2.0 Project. Maintained locally in `docs/neuroos_v2_modular_blueprint-v6.md`.*
