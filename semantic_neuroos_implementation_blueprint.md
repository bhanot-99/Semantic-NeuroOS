# Semantic-NeuroOS: Full System Implementation & Engineering Architecture Blueprint
## Visual Architectural Design, Component Class Diagrams, and Interactive Workflows

**Document Title**: Complete Implementation Specification for Semantic-NeuroOS  
**Version**: 3.2 (Production Engineering Release: Modular Architecture & Semantic Matrix Integration)  
**Date**: September 2026  
**Security Model**: Airgapped Zero-Egress Kernel (`PrivateNetwork=true`) + Landlock LSM + Capabilities  

---

## Executive Architectural Map

```
+-----------------------------------------------------------------------------------------------------------------------------------+
|                                                  SEMANTIC-NEUROOS SYSTEM BUS                                                      |
|                             (Unix Domain Sockets & POSIX SHM Seqlock — Isolated Loopback Fabric)                                  |
+-----------------------------------------------------------------------------------------------------------------------------------+
        |               |               |               |               |               |               |               |
        v               v               v               v               v               v               v               v
+---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+
|  COMPONENT 1  | |  COMPONENT 2  | |  COMPONENT 3  | |  COMPONENT 4  | |  COMPONENT 5  | |  COMPONENT 6  | |  COMPONENT 7  | | DIAGNOSTICS   |
|   Desktop     | |   Voice &     | |   Semantic    | |   Neural      | |   Data        | |   SafetyGate  | |   External    | |   Health      |
|   Monitor     | |   Audio       | |   Storage     | |   Inference   | |   Refinement  | |   Execution   | |   Fetcher     | |   Aggregator  |
|   Telemetry   | |   Pipeline    | |   Engine &    | |   Engine      | |   Knowledge   | |   Kernel &    | |   Spool       | |   Daemon      |
|               | |               | |   Ingest      | |               | |   Engine      | |   Registry    | |               | |               |
| [neuroos-     | | [neuroos-     | | [neuroos-     | | [neuroos-     | | [neuroos-     | | [neuroos-     | | [neuroos-     | | [neuroos-     |
|  monitor]     | |  voice]       | |  storage]     | |  inference]   | |  knowledge]   | |  kernel]      | |  fetcher]     | |  healthd]     |
+---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+ +---------------+
  UID: $USER        UID: $USER        UID: $USER        UID: $USER        UID: $USER        UID: $USER        UID: fetcher (sys) UID: $USER
  Net: Isolated     Net: Isolated     Net: Isolated     Net: Isolated     Net: Isolated     Net: Isolated     Net: EGRESS ALLOW Net: Isolated
  RAM: ~25 MiB      RAM: ~485 MiB     RAM: ~205 MiB     RAM: ~1,590 MiB   RAM: ~75 MiB      RAM: ~15 MiB      RAM: ~10 MiB      RAM: ~15 MiB
  Path: N/A         Path: Audio/PCM   Path: ~/.local    Path: mmap GGUF   Path: ~/.local    Path: actions.j   Path: /var/spool/ Path: Unix Socket
                                            /share/storage  + KV Cache          /graph_view.html  sonl              fetcher/          Scraper
        |               |               |               |               |               |               |               |
        +---------------+---------------+------- Systemd PrivateNetwork=true Sandbox ---+---------------+---------------+
                                                (Components 1-6 & Healthd physically blocked)                    v
                                                                                                +---------------+
                                                                                                | Outer Web /   |
                                                                                                | External Net  |
                                                                                                +---------------+
```

> **System Footprint Summary**: Total active memory footprint is **~2,420 MiB RSS** (~2.42 GB). On resource-constrained systems, omitting Kokoro-82M neural vocoding drops footprint to **~2,120 MiB RSS** (~2.12 GB), with Piper ONNX handling TTS directly.

---

## 1. High-Level System Sequence & Workflow Diagrams

### 1.1 Query Execution & Deictic Temporal Snapping Flow

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant C1 as Comp 1: Monitor
    participant C2 as Comp 2: Voice
    participant C3 as Comp 3: Storage
    participant C4 as Comp 4: Inference
    participant C5 as Comp 5: Knowledge
    participant C6 as Comp 6: Kernel

    User->>C2: Vocal Trigger: "Summarize this bug" (t = 10.5s)
    C2->>C2: Silero VAD (Gated) + Whisper.cpp Transcription
    C2->>C5: Emit VoiceCommand("Summarize this bug", timestamp=10.5s)
    
    rect rgb(240, 255, 240)
        note over C5,C2: Immediate Acoustic Preamble (<50ms TTFA)
        C5->>C2: TriggerAcousticPreamble("Checking your files...") [<50ms TTFA]
        C2-->>User: Audio Stream: "Checking your files..."
    end

    rect rgb(230, 245, 255)
        note over C5,C3: Deictic Temporal Snapping Phase (±1.5s Window)
        C5->>C3: Query FocusHistory(timestamp=10.5s ± 1.5s)
        C3-->>C5: Return Active Window: {app_id: "alacritty", title: "cargo build - err 104"}
    end

    rect rgb(255, 245, 230)
        note over C5,C3: Evidence Retrieval & Async Background Distillation
        C5->>C3: QueryHybridVectorText("Summarize this bug", top_k=5)
        C3->>C3: Generate Embedding via FastEmbed ONNX (9.5-12ms) + SIMD Search (<1.0ms) [Total ~11-13ms]
        C3-->>C5: Return Evidence Chunks
        alt Tokens > 1,000 (Async Distillation Path)
            C5->>C5: Fast-Path Truncate Evidence to 320 tokens (<5ms)
            C5-)C4: Spawn Async Background Distillation Job (Budget ~6.0s for cache warming)
        else Tokens <= 1,000 (Fast Path Direct)
            C5->>C5: Fast-Path Truncate Evidence to 320 tokens (<5ms)
        end
    end

    C5->>C5: Assemble Prompt (512 Token Cap: 128 System + 64 Deictic + 320 Evidence)
    C5->>C6: EvaluateCapability(Prompt, Capability::ReadMetadata)
    C6-->>C5: Approved (SAFE Tier)

    rect rgb(240, 255, 240)
        note over C5,C4: LLM Generation & Audio Streaming
        C5->>C4: ExecuteLLMInference(Prompt) [mmap BitNet b1.58]
        C4-->>C2: Stream Tokens via SHM Seqlock Buffer
        C2-->>User: Kokoro-82M 24kHz Neural Audio Stream (~240ms latency)
    end
```

---

### 1.2 Web Fetcher & Active Taint Escalation Flow

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant C5 as Comp 5: Knowledge
    participant C6 as Comp 6: Kernel
    participant C7 as Comp 7: Fetcher
    participant C3 as Comp 3: Storage

    User->>C5: Request: "Research URL: https://example.com/api-docs"
    C5->>C6: DispatchSkill(Capability::NetworkEgress, URL)
    C6->>C6: Check Capability -> REVIEW Tier
    C6-->>User: Trigger Desktop Confirmation Dialog
    User->>C6: User Approves Network Egress
    C6->>C7: IPC Fetch Request over Socket
    
    rect rgb(255, 230, 230)
        note over C7: Isolated Egress Environment (Dedicated UID)
        C7->>C7: Anti-SSRF Sandbox Validation (Block 127.0.0.1, 10.0.0.0/8, 192.168.0.0/16)
        C7->>C7: Fetch External HTML/JSON
        C7->>C7: Write Payload to /var/spool/neuroos-fetcher/doc_42.json (mode 0770)
        C7->>C3: IPC Signal: NotifySpoolPayload("doc_42.json")
    end

    C3->>C3: Ingest Payload, Attach TaintFlags::EXTERNAL_UNTRUSTED & Index in LanceDB
    
    rect rgb(255, 240, 200)
        note over C5,C6: Active Taint Escalation Trigger (SAFE -> REVIEW)
        User->>C5: Request: "Check my calendar events matching api-docs"
        C5->>C3: Retrieve Evidence (Payload contains TaintFlags::EXTERNAL_UNTRUSTED)
        C5->>C5: Wrap Payload: <untrusted_external_doc taint="true">
        C5->>C6: RequestSkillDispatch(Capability::ReadCalendar, Prompt)
        C6->>C6: Detect TaintFlags == EXTERNAL_UNTRUSTED
        C6->>C6: ESCALATE: SAFE -> REVIEW Tier (Auto-Execution Revoked)
        C6-->>User: Trigger HMAC-SHA256 Desktop Confirmation Dialog
        alt User Accepts
            User->>C6: Signed HMAC Confirmation
            C6->>C6: Execute Skill & Log actions.jsonl
        else User Rejects
            User->>C6: Deny
            C6->>C6: Abort Skill & Log ATTEMPT/REJECTED
        end
    end
```

---

## 2. Component Architecture & Class Diagrams

```
+-------------------------------------------------------------------------------------------------------------------+
|                                       COMPONENT CLASS INTERCONNECTION MATRIX                                      |
|                                                                                                                   |
|  +-------------------------+      Events       +-------------------------+      Vectors      +-----------------+  |
|  |   Component 1: Monitor  | ----------------> |   Component 3: Storage  | <---------------> | Comp 4: Infer.  |  |
|  | (TelemetrySensors,      |  (RawTelemetry)   | (IngestPipeline,        |   (mmap GGUF)     | (BitNetEngine,  |  |
|  |  WindowFocusHistory)    |                   |  FastEmbed, LanceDB)    |                   |  TokenBuffer)   |  |
|  +-------------------------+                   +-------------------------+                   +-----------------+  |
|               |                                             ^                                         ^           |
|               | Window Focus History                        | Query Text -> Evidence                  | Inferences|
|               v                                             v                                         |           |
|  +-----------------------------------------------------------------------+                            |           |
|  |                         Component 5: Knowledge                        | ---------------------------+           |
|  |  (DeicticSnapper, TwoTierEvidencePipeline, LightRAGContextAssembler)  |                                        |
|  +-----------------------------------------------------------------------+                                        |
|               ^                                             |                                                     |
|               | Voice Commands                              | Prepared Prompt + Taint Flags                       |
|               |                                             v                                                     |
|  +-------------------------+                   +-------------------------+                   +-----------------+  |
|  |   Component 2: Voice    |                   |   Component 6: Kernel   | ----------------> | Comp 7: Fetcher |  |
|  | (SileroVAD, WhisperSTT, |                   | (SafetyGate, Capability,|  (Spool Command)  | (AntiSSRF,      |  |
|  |  PiperTTS, Kokoro24k)   |                   |  AuditLog, SkillReg)    |                   |  TaintAttacher) |  |
|  +-------------------------+                   +-------------------------+                   +-----------------+  |
|                                                                                                                   |
|  +-------------------------------------------------------------------------------------------------------------+  |
|  |                                  Diagnostics: Independent neuroos-healthd                                   |  |
|  |                           (Scrapes /health endpoints across all components)                                 |  |
|  +-------------------------------------------------------------------------------------------------------------+  |
+-------------------------------------------------------------------------------------------------------------------+
```

---

### 2.1 Component 1: Desktop Telemetry & System Monitoring (`neuroos-monitor`)

```mermaid
classDiagram
    class TelemetrySensor {
        +u64 poll_interval_ms
        +bool active
        +sample_cpu_usage() CpuMetrics
        +sample_memory_rss() MemoryMetrics
    }

    class WaylandWindowSensor {
        +String compositor_protocol
        +u64 focus_dwell_threshold_ms
        +connect_zcosmic_toplevel() bool
        +connect_wlr_toplevel() bool
        +get_active_window_handle() WindowHandle
    }

    class IdleNotificationSensor {
        +u64 idle_timeout_ms
        +bool is_user_idle
        +on_idle_state_change(bool idle) void
    }

    class MPRISObserver {
        +String dbus_bus_name
        +get_playback_status() PlaybackState
        +get_metadata() MediaMetadata
    }

    class RawTelemetryEvent {
        +u64 timestamp_ns
        +u32 sensor_id
        +String domain_hub
        +Vec<u8> payload_json
    }

    TelemetrySensor --> RawTelemetryEvent : emits
    WaylandWindowSensor --> RawTelemetryEvent : emits
    IdleNotificationSensor --> RawTelemetryEvent : emits
    MPRISObserver --> RawTelemetryEvent : emits
```

> **Component 1 Key Notes**:
> - **UID/GID**: Runs as local `$USER` (no root required).
> - **Isolation**: Enforced via systemd `PrivateNetwork=true` & Landlock read-only access to `/proc` and Wayland sockets.
> - **Decoupling**: Pure event generator. Does NOT judge or filter data—emits raw events over Unix Socket (`/run/user/$UID/neuroos/monitor.sock`).

---

### 2.2 Component 2: Voice, Audio & Speech Pipeline (`neuroos-voice`)

```mermaid
classDiagram
    class PipeWireAudioCapture {
        +u32 sample_rate_hz = 16000
        +u32 channels = 1
        +pw_stream* stream_handle
        +start_capture() void
        +hardware_barge_in_flush() void
    }

    class SileroVAD {
        +float probability_threshold = 0.75
        +process_frame(Vec<f32> pcm) bool
    }

    class OpenWakeWordEngine {
        +String model_name = "jarvis"
        +detect_activation(Vec<f32> pcm) bool
    }

    class WhisperSTT {
        +String model_path = "ggml-tiny.en-q5_1.bin"
        +transcribe_pcm(Vec<f32> pcm) String
        +emit_voice_command_to_component5(String cmd) void
    }

    class PiperTTSEngine {
        +u32 ttfa_target_ms = 50
        +synthesize_preamble(String text) Vec<u8>
    }

    class KokoroNeuralVocoder {
        +u32 sample_rate_hz = 24000
        +u32 latency_ms = 240
        +synthesize_audio_stream(String text) ChunkStream
    }

    PipeWireAudioCapture --> SileroVAD : raw pcm
    SileroVAD --> OpenWakeWordEngine : speech pcm
    OpenWakeWordEngine --> WhisperSTT : activated pcm
    WhisperSTT ..> PiperTTSEngine : parallel tts path
    WhisperSTT ..> KokoroNeuralVocoder : parallel tts path
```

> **Component 2 Key Notes**:
> - **VAD CPU Gating**: Silero VAD runs continuously on incoming PipeWire PCM frames to gate CPU usage; openWakeWord executes strictly when active speech is detected (< 1% idle CPU).
> - **Audio Barge-In**: Native PipeWire `pw_stream_flush` drops hardware audio buffers in **< 1 ms** upon speech detection.
> - **Memory Allocation**: ~485 MiB RSS total (Silero VAD ~15MB + openWakeWord ~10MB + Whisper ~80MB + Piper ~80MB + Kokoro ~300MB).
> - **Latency Masking**: Piper ONNX synthesizes instant preambles ("Checking...") in **< 50 ms TTFA**, masking Kokoro's 240 ms vocoder stream.

---

### 2.3 Component 3: Semantic Matrix Autonomous Storage Engine (`neuroos-storage`)

```mermaid
classDiagram
    class IngestFilterPipeline {
        +ppid_process_tree_collapse(u32 ppid) u32
        +filter_self_observation(String app_id) bool
        +evaluate_promotion_gate(u32 count, u64 dwell_ms) bool
        +demote_mpris_media(MediaMetadata meta) bool
    }

    class FastEmbedONNX {
        +String model_id = "bge-small-en-v1.5"
        +u32 dimension = 384
        +generate_embedding(String text) Vec<f32>
    }

    class LanceDBStorageAdapter {
        +String db_path = "~/.local/share/neuroos/storage/lancedb"
        +insert_vectors(Vec<VectorRecord> records) void
        +exact_simd_flat_search(Vec<f32> query, u32 top_k) Vec<QueryResult>
    }

    class USearchHNSWIndex {
        +u32 max_neighbors = 16
        +bool latency_promotion_active
        +hnsw_graph_search(Vec<f32> query, u32 top_k) Vec<QueryResult>
        +check_p99_latency_trigger(f32 p99_ms) void
    }

    class DomainAdapter13 {
        +String domain_name
        +TaintFlags taint_state
        +pack_payload(RawTelemetryEvent event) DomainPayload
    }

    IngestFilterPipeline --> DomainAdapter13 : filtered event
    DomainAdapter13 --> FastEmbedONNX : payload text
    FastEmbedONNX --> LanceDBStorageAdapter : 384D vector
    LanceDBStorageAdapter --> USearchHNSWIndex : latency > 5.0ms trigger
```

> **Component 3 Key Notes**:
> - **Single FastEmbed Owner**: Component 3 owns the FastEmbed ONNX model (~120 MiB RSS). C5 passes query text to C3; C3 generates the embedding locally (9.5-12ms) and runs SIMD search (<1.0ms), returning evidence chunks in ~11-13ms total.
> - **Ingest Execution**: Synchronous 4-stage filter runs in Rust inside C3 before storage.
> - **Latency-Driven Indexing**: Flat SIMD dot-product search handles up to 20,000 items. Promotes to USearch HNSW graph **strictly when p99 latency exceeds 5.0 ms**.

---

### 2.4 Component 4: Neural Inference Engine (`neuroos-inference`)

```mermaid
classDiagram
    class POSIXMmapWeightLoader {
        +String GGUF_file_path
        +void* mmap_base_address
        +u64 total_weight_bytes = 1513988096
        +mmap_shared_weights() void*
    }

    class BitNetTernaryEngine {
        +String architecture = "2B4T b1.58"
        +u32 context_window_cap = 512
        +execute_simd_gemm(Vec<i8> input, void* weights) Vec<f32>
        +apply_gbnf_grammar(String grammar_rules) void
    }

    class PreAssemblyDistiller {
        +u64 time_budget_ms = 6000
        +async_distill_chunks(Vec<String> chunks) String
    }

    class TokenRingBuffer {
        +void* shm_seqlock_ptr
        +write_token(u32 token_id) void
        +read_tokens_zero_copy() Vec<u32>
    }

    POSIXMmapWeightLoader --> BitNetTernaryEngine : mapped memory
    BitNetTernaryEngine --> PreAssemblyDistiller : async background distillation
    BitNetTernaryEngine --> TokenRingBuffer : zero-copy token stream
```

> **Component 4 Key Notes**:
> - **Zero-Copy Memory**: Single physical RAM allocation (~1.41 GiB) mapped across processes via `mmap`.
> - **Context Cap Partition**: Default 512 tokens (128 System + 64 Deictic + 320 Evidence).
> - **Async Cache Warming Distillation**: Large evidence (>1,000 tokens) distilled in the background (~6.0s budget @ 2.5ms/token) to warm cache without blocking fast-path query execution.

---

### 2.5 Component 5: Data Refinement & Knowledge Engine (`neuroos-knowledge`)

```mermaid
classDiagram
    class HotQueryPathRust {
        +u64 deictic_window_ms = 1500
        +deictic_snap(u64 timestamp_ns) WindowContext
        +assemble_lightrag_context(Vec<Chunk> chunks) Prompt
        +wrap_xml_taint(Prompt prompt, TaintFlags flags) Prompt
        +write_graph_view_html_on_demand() void
    }

    class ColdBackgroundWorkerPython {
        +u32 appnp_power_iterations = 10
        +f32 hawkes_decay_rate = 0.05
        +run_appnp_sparse_power_iteration() void
        +calculate_hawkes_excitation() PointIntensity
        +prune_unreinforced_edges_72h_ttl() void
    }

    class TwoTierEvidencePipeline {
        +fast_path_truncate(Vec<Chunk> chunks) Vec<Chunk>
        +spawn_async_distillation_job(Vec<Chunk> chunks) void
    }

    HotQueryPathRust --> TwoTierEvidencePipeline : evidence size check
    HotQueryPathRust --> ColdBackgroundWorkerPython : async graph updates
```

> **Component 5 Key Notes**:
> - **Hot/Cold Process Split**: Real-time queries run in native Rust (`neuroos-knowledge-query`, < 5 ms). Background GNN/Hawkes iterations run in Python (`neuroos-knowledge-background`).
> - **Deictic Snapping**: Resolves ambiguous words ("this", "it") using a **±1.5-second temporal window** against Component 3's focus history.
> - **Graph Inspection**: Writes a standalone single-file D3 visualization to `~/.local/share/neuroos/graph_view.html` on demand.

---

### 2.6 Component 6: SafetyGate & Execution Kernel (`neuroos-kernel`)

```mermaid
classDiagram
    class ObjectCapabilityRegistry {
        +HashMap<SkillID, Capability> skill_table
        +register_skill(SkillID id, Capability cap) void
        +lookup_capability(SkillID id) Capability
    }

    class SafetyGateFirewall {
        +evaluate_tier(Capability cap, TaintFlags taint) SafetyTier
        +escalate_tainted_context(SafetyTier tier) SafetyTier
    }

    class LandlockLSMEnforcer {
        +apply_landlock_ruleset(Vec<Path> rw_paths) void
        +apply_seccomp_syscall_filter() void
    }

    class AuditLogManager {
        +String log_path = "~/.local/share/neuroos/actions.jsonl"
        +u64 max_bytes = 10485760
        +log_attempt_and_result(ActionAttempt a, ActionResult r) void
        +rotate_logs() void
    }

    ObjectCapabilityRegistry --> SafetyGateFirewall : capability check
    SafetyGateFirewall --> LandlockLSMEnforcer : sandbox execution
    SafetyGateFirewall --> AuditLogManager : record JSONL
```

> **Component 6 Key Notes**:
> - **Active Taint Escalation**: External untrusted web data **promotes SAFE skills (e.g. `Capability::ReadCalendar`) to REVIEW tier**, revoking auto-execution and requiring an HMAC confirmation popup.
> - **Transient Dialog Budget**: GTK/Zenity HMAC confirmation dialogs allocate **+25 MiB RSS dynamically on demand** while displayed, freed immediately upon closure.
> - **Audit Trail**: Every execution writes paired `ATTEMPT`/`RESULT` records to `actions.jsonl` (rotated at 10 MB, 5 generations retained).

---

### 2.7 Component 7: External Network Fetcher & Spool Daemon (`neuroos-fetcher`)

```mermaid
classDiagram
    class UnprivilegedFetcherDaemon {
        +String system_user = "neuroos-fetcher"
        +String system_group = "neuroos"
        +String spool_directory = "/var/spool/neuroos-fetcher/"
        +fetch_url(String url) NetworkPayload
    }

    class AntiSSRFSandbox {
        +Vec<String> blocked_ip_ranges
        +validate_ip_address(IPAddr ip) bool
        +block_private_subnets() void
    }

    class TaintAttacher {
        +attach_untrusted_flag(NetworkPayload payload) TaintedPayload
        +write_to_spool_and_notify_c3(TaintedPayload payload) void
    }

    UnprivilegedFetcherDaemon --> AntiSSRFSandbox : validate destination
    AntiSSRFSandbox --> TaintAttacher : approved fetch
```

> **Component 7 Key Notes**:
> - **User & File Isolation**: Runs under dedicated `neuroos-fetcher` UID/GID (allocated dynamically via `systemd-sysusers`). Writes strictly to `/var/spool/neuroos-fetcher/` (`mode 0770`).
> - **Network Egress**: The ONLY component with off-device network access.
> - **Health Scraping**: `/run/neuroos-fetcher/health.sock` is `mode 0660` owned by `neuroos-fetcher:neuroos`. `$USER` is in the `neuroos` group to allow cross-UID health scraping.

---

### 2.8 System Diagnostics: Independent Health Aggregator (`neuroos-healthd`)

```mermaid
classDiagram
    class HealthdAggregator {
        +u64 poll_interval_s = 30
        +scrape_component_health_over_unix_sockets() HealthReport
        +check_soak_harness_thresholds(HealthReport r) bool
        +check_cgroup_memory_and_latency() void
    }
```

> **Diagnostics Key Notes**:
> - **Independent Binary**: Independent daemon (`neuroos-healthd`, ~15 MiB RSS) built at Step 1.
> - **Out-of-Process Resilience**: Kernel faults or security panics in Component 6 never blind health monitoring.

---

## 3. Operational Data Engineering & Maintenance Lifecycle

```mermaid
flowchart LR
    subgraph Ingestion_And_GC["Ingestion & Retention Engine (Component 3)"]
        direction TB
        RAW_IN["Raw Telemetry Stream"] --> FILTER["4-Stage Ingest Filter"]
        FILTER --> RETENTION{"Data Age Check"}
        RETENTION -->|"< 14 Days"| RAW_DB[("Raw Telemetry Tables")]
        RETENTION -->|"< 90 Days"| AGG_DB[("Aggregated Event Tables")]
        RETENTION -->|"Permanent"| PERM_DB[("Notes / Git / Commits")]
        RETENTION -->|"> TTL Expiry"| PURGE["Daily GC Vacuum Purge"]
    end

    subgraph Reindexing_And_Backup["Model Versioning & Backups"]
        direction TB
        MODEL_UP["Embedding Model Upgrade"] --> CHECK_ID{"Model ID Changed?"}
        CHECK_ID -->|"Yes"| REINDEX["Non-Blocking Background Re-Index"]
        CHECK_ID -->|"No"| STABLE["Maintain USearch / LanceDB Index"]
        
        WAL["SQLite WAL Journal"] -->|Every 6 Hours| CHECKPOINT["Atomic Snapshot Checkpoint"]
        CHECKPOINT --> BACKUP[("~/.local/share/neuroos/backups/")]
    end
```

---

## 4. Master Technical Performance & Resource Budget Table

| System Component | Binary Service | Primary Runtime / Engine | RSS Memory Budget | Latency Target | Operational Security & Boundaries |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Component 1** | `neuroos-monitor` | Rust 2021 (`tokio`) | **~25 MiB** | < 1.5 ms | User UID; `PrivateNetwork=true`; Read-only Wayland/D-Bus |
| **Component 2** | `neuroos-voice` | C++20 / ONNX Runtime | **~485 MiB** | < 50 ms TTFA | Silero VAD + Whisper + Piper + Kokoro-82M; Sub-1ms Audio Barge-In |
| **Component 3** | `neuroos-storage` | Rust + LanceDB / USearch | **~205 MiB** | ~11–13 ms Total | FastEmbed ONNX (120MiB); 4-stage filter; Latency-driven HNSW ($p99 > 5.0\text{ms}$) |
| **Component 4** | `neuroos-inference` | C++20 (`bitnet.cpp`) | **~1,590 MiB** | 45.0 ms/token | POSIX `mmap` BitNet b1.58 (~1,410MiB) + KV Cache/SIMD Buffers (~180MiB) |
| **Component 5** | `neuroos-knowledge` | Rust (Query) + Python (GNN) | **~75 MiB** | < 5.0 ms (Hot) | Deictic snapping ($\pm 1.5\text{s}$); Fast-path truncate + Async background distillation |
| **Component 6** | `neuroos-kernel` | Rust 2021 | **~15 MiB** | < 0.5 ms | SafetyGate Firewall; Active Taint Escalation; Landlock LSM; `actions.jsonl` |
| **Component 7** | `neuroos-fetcher` | Rust 2021 | **~10 MiB** | Network Dep. | System `neuroos-fetcher` UID; Egress Allowed; Anti-SSRF; Taint Attacher |
| **Diagnostics** | `neuroos-healthd` | Rust 2021 (Standalone) | **~15 MiB** | 30s Poll | Standalone binary built Step 1; Unix socket scraper; Soak harness alerting |
| **Transient UI** | GTK / Zenity Dialog | C / GTK3 (On-Demand) | **+25 MiB (Transient)**| On Display | Allocated dynamically during HMAC confirmation popups; freed on close |
| **TOTAL FOOTPRINT** | **System Total** | **Polyglot Stack** | **~2,420 MiB** | **< 50 ms perceived** | **100% Airgapped Execution (~2,120 MiB if Kokoro omitted)** |

---

## 5. 6-Step Component Build Roadmap

```
Step 1: Healthd & Inference Engine   ---> Step 2: Storage & Migration Script ---> Step 3: Knowledge Query & Context
(neuroos-healthd + bitnet.cpp mmap)       (LanceDB + Soak-Replay Assertion Gate)  (Hot Rust Snapping + Two-Tier Pipeline)
                                                                                            |
                                                                                            v
Step 6: Integration & 24h Soak Harness <--- Step 5: SafetyGate & Fetcher Spool <--- Step 4: Telemetry & Voice Pipeline
(Soak Harness + End-to-End Tests)          (Landlock Kernel + Fetcher UID)          (Wayland Sensors + PipeWire Audio)
```

1. **Step 1 (Core Diagnostics & Inference Benchmark)**: Build `neuroos-healthd` (standalone ~15 MiB) and `neuroos-inference`. Benchmark BitNet b1.58 TTFT on target CPU across 128, 512, and 1024 token contexts. Enforce `PrivateNetwork=true` in unit stanzas from day one.
2. **Step 2 (Storage Engine & Soak-Replay Gate)**: Build `neuroos-storage` with LanceDB, FastEmbed ONNX, and the synchronous 4-stage ingest filter. Run the SQLite migration script. **Pass Condition Gate**: Replay captured telemetry dumps and assert `total_promoted_entities <= 30`, `compiler_subprocesses == 0`, and `zero_access_mpris_nodes == 0`.
3. **Step 3 (Knowledge Query Path & Two-Tier Pipeline)**: Build `neuroos-knowledge-query` in Rust. Validate Deictic Snapping using C3's focus history, verify fast-path evidence truncation (<5ms) and async background distillation spawning, and generate on-demand `graph_view.html`.
4. **Step 4 (Desktop Telemetry & Voice Pipeline)**: Build `neuroos-monitor` (Wayland/MPRIS sensors) and `neuroos-voice` (PipeWire VAD, Whisper, Piper TTFA, Kokoro, and sub-1ms audio barge-in).
5. **Step 5 (SafetyGate Kernel & Network Fetcher)**: Build `neuroos-kernel` (Landlock LSM, Object-Capability registry, active taint escalation, `actions.jsonl`) and `neuroos-fetcher` (dedicated system UID via `systemd-sysusers`, anti-SSRF).
6. **Step 6 (System Integration & 24h Soak Verification)**: Verify `PrivateNetwork=true` stanzas across all internal units, execute end-to-end integration tests, and run 24-hour `neuroos-healthd` soak harness validation (asserting RSS growth < 5% and $p99$ latency drift < 10%).

---
*Document generated for the Semantic-NeuroOS V3.2 Production Engineering Release.*
