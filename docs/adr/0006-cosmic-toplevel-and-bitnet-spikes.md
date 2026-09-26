# ADR-0006: spikes S-03 (COSMIC toplevel) and S-04 (bitnet.cpp AVX2)

- Status: Accepted
- Date: 2026-09-26

## Context

P0-S08: "COSMIC toplevel data and bitnet.cpp AVX2 builds are proven on the
reference machine" (FR-MON-01, FR-INF-01). Two independent, time-boxed
spikes, run on the reference machine (memory.md §10: AMD Ryzen 7 5800H,
AVX2, Pop!_OS 24.04, COSMIC/Wayland).

## S-03: `zcosmic_toplevel_info_v1`

Bound `zcosmic_toplevel_info_v1` against the real, live COSMIC session on
this machine (`cosmic-protocols` 0.2, `wayland-client` 0.31 — both already
on Architecture.md §12.2's allowlist). Ported the crate's own
`toplevel-list.rs` example into `crates/neuroos-monitor/src/sensors/
wayland_cosmic.rs` as `list_toplevels()`.

Real output against this session (5 windows open: a browser, two COSMIC
Files windows, COSMIC Terminal, Obsidian):

```
Toplevel app_id=brave-browser        title="Claude Code - Brave"    states=[Maximized]
Toplevel app_id=com.system76.CosmicFiles  title="... COSMIC Files"  states=[]
Toplevel app_id=com.system76.CosmicTerm   title="... COSMIC Terminal" states=[Activated]
Toplevel app_id=md.obsidian.Obsidian      title="... Obsidian"      states=[Maximized]
Toplevel app_id=com.system76.CosmicFiles  title="Documents — COSMIC Files" states=[]
```

`app_id`, `title`, and activation state (`Activated` correctly on the
foreground terminal window) all read back correctly. **Confirmed
reachable, confirmed the data C1 needs.**

**Decision:** proceed with `zcosmic_toplevel_info_v1` as designed
(Architecture.md, FR-MON-01). `list_toplevels()` is a one-shot snapshot;
Phase 3 replaces it with a persistent event-subscription (the protocol
already supports push events — `Toplevel`/`Closed`/`State`/`Title`/`AppId`
— `list_toplevels()` just does three round-trips and stops) feeding
`monitor.sock`.

## S-04: bitnet.cpp AVX2 build

Added `microsoft/BitNet` as a git submodule at `cpp/third_party/bitnet.cpp`
(pinned commit, its own nested submodule `3rdparty/llama.cpp` pinned too,
per Architecture.md §12.3). Downloaded the real model PRD/Architecture
already named (`microsoft/BitNet-b1.58-2B-4T-gguf`, `ggml-model-i2_s.gguf`,
i2_s quantization — Architecture.md §7.1, PRD FR-INF-01) via
`huggingface-cli download`. Built with `setup_env.py -md
models/BitNet-b1.58-2B-4T -q i2_s`, which drives CMake/clang with the AVX2
kernel selection for this specific model shape.

**Build:** succeeded (`build/bin/llama-cli`, `build/bin/llama-bench`;
version `9918 (390c30775)`, Clang 18.1.3). CMake sets `GGML_NATIVE=ON`
(not the individual `GGML_AVX2` toggle — that's for cross-compiling; native
builds go through `-march=native` instead). Confirmed both ways:
- The actual compile flags for `ggml-cpu` (`.../ggml-cpu.dir/flags.make`)
  include `-march=native`.
- `clang++ -march=native -dM -E -x c++ /dev/null | grep __AVX2__` →
  `#define __AVX2__ 1` on this CPU (AMD Ryzen 7 5800H, Zen 3).

**Throughput** (`utils/e2e_benchmark.py -m models/BitNet-b1.58-2B-4T/
ggml-model-i2_s.gguf -n 128 -p 128 -t 8`, the official benchmark script,
8 threads matching this CPU's 8 physical cores):

| model | size | params | threads | test | t/s |
| :--- | :--- | :--- | :--- | :--- | :--- |
| bitnet-b1.58 2B Q1_0 | 1.10 GiB | 2.41 B | 8 | pp128 (prefill) | 17.07 ± 0.71 |
| bitnet-b1.58 2B Q1_0 | 1.10 GiB | 2.41 B | 8 | tg128 (decode) | 17.89 ± 0.28 |

This is R-02's "first signal" (memory.md §8: "BitNet decode speed on Zen 3
unverified"): **~17.9 tokens/s decode, ~17.1 tokens/s prefill, 8 threads,
real 2.41B-parameter i2_s-quantized model, on the actual reference
machine.** Whether that meets Phase 2's latency budget (Architecture.md
§9) is Phase 2's question to answer with the real inference engine wired
to the memfd ring (P0-S07) — this spike only had to produce a number, not
judge it.

## Decision

Both candidates are confirmed usable on the reference machine as designed:
`zcosmic_toplevel_info_v1` works and gives C1 exactly the data it needs;
`bitnet.cpp` builds with AVX2 active via `-march=native` and produces a
concrete, real decode-speed figure. Neither spike surfaced a reason to
change Architecture.md's plan for C1 or C4. Proceed to Phase 2 (inference)
and Phase 3 (monitor) as designed.

## Consequences

- `crates/neuroos-monitor/src/sensors/wayland_cosmic.rs` is real, tested
  (2 unit tests + 1 live-session integration test, `#[ignore]`d for CI)
  code, not a stub — Phase 3 builds on it rather than starting from
  scratch.
- `cpp/third_party/bitnet.cpp` (and its nested `3rdparty/llama.cpp`) is a
  real submodule now; `.gitmodules` tracks it. The downloaded model
  (~1.1 GiB) lives under `cpp/third_party/bitnet.cpp/models/`, gitignored
  (models are never committed — PRD §"Models" / `scripts/fetch-models.sh`,
  P0-S09, is the real per-machine fetch mechanism; this spike fetched it
  by hand into the submodule's own working tree for a one-off
  measurement).

## Alternatives considered

- **Skip the real model download, just prove the build compiles:**
  rejected once the owner confirmed the full spike (network/disk cost
  accepted) — a build-only proof doesn't answer R-02 (decode speed
  unverified), which is the actual point of S-04.
