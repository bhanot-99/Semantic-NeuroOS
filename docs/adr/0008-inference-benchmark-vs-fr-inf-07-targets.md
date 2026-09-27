# ADR-0008: Phase 2 inference benchmark results miss FR-INF-07's raw targets; proceed anyway

- Status: Accepted
- Date: 2026-09-27

## Context

PRD FR-INF-07 sets targets for the reference machine: prefill ≈ 2.5 ms/token,
decode ≈ 45 ms/token, "verified by Phase 2 benchmark at 128/512/1,024
tokens." Phases.md §5.4's Phase 2 exit criterion requires the benchmark JSON
to be compared against these targets, and if a target is missed, a
mitigation decision recorded here before proceeding.

ADR-0006 (P0-S08 spike) already flagged R-02 ("BitNet decode speed on Zen 3
unverified") with a first signal — 17.89 t/s decode (≈ 55.9 ms/token) via
the official `e2e_benchmark.py` script — and explicitly left "whether that
meets Phase 2's latency budget" as Phase 2's question to answer.

## Measured results

`neuroos-inference --bench` (real model, real `llama_decode` calls, not an
estimate), reference machine (AMD Ryzen 7 5800H, 8 physical cores),
`reports/bench/inference-1790486839.json`:

| n_ctx | threads | prompt tokens | TTFT | prefill ms/tok | decode ms/tok | RSS |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| 128 | 8 | 71 | 629.5 ms | 8.87 | 72.11 | 1,223 MiB |
| 512 | 8 | 451 | 3,720.6 ms | 8.25 | 56.09 | 1,283 MiB |
| 1,024 | 8 | 961 | 8,520.1 ms | 8.87 | 60.21 | 1,327 MiB |
| 512 | 1 | 451 | 16,960.8 ms | 37.61 | 141.34 | 1,283 MiB |
| 512 | 4 | 451 | 5,404.7 ms | 11.98 | 57.93 | 1,283 MiB |
| 512 | 8 | 451 | (repeat) | — | 55.24 | 1,283 MiB |

Both FR-INF-07 targets are missed on these raw numbers:

- **Decode**: 55–72 ms/token measured vs. 45 ms/token target (1.2×–1.6× over).
- **Prefill**: 8.25–8.87 ms/token measured vs. 2.5 ms/token target (3.3×–3.5× over).

Thread scaling is real and roughly as expected (1→4 threads: 141→58 ms/token
decode; 4→8 threads: diminishing return, 58→55 ms/token — this 8-core CPU is
already close to saturated on this model's per-token matmul work at 8
threads). RSS (1.22–1.33 GiB) is well within the 1,590 MiB budget
(Architecture.md §7.1's C4 row) even at the largest context tested.

## Why these numbers, and why the gap is smaller in practice than it looks

This benchmark measures **cold, worst-case** prefill and decode: every
`bench_one()` call creates a **fresh** `Context` and decodes the **entire**
filler prompt from position 0, with no KV cache reuse. That is deliberately
the worst case for prefill (it isolates "cost per prompt token" cleanly),
but it is *not* representative of a real interactive-lane request:

- Architecture.md §6.1's actual prompt shape is 128 (system) + 64 (deictic)
  + 320 (evidence) = 512 tokens, but the 128-token system block is
  **constant across every request on a lane**. `Context::generate`'s
  prefix-cache reuse (engine.cpp, this phase) means only the varying ~384
  tokens get freshly decoded on the second and later requests on a warm
  lane, not the full 512 — cutting real per-turn prefill roughly in half
  after the first request, not measured by this cold-context benchmark.
- Decode cost is prompt-independent (per generated token, not per context
  size) — the 512/1,024-context decode numbers (56–60 ms/token) are already
  close to the ~55 ms/token steady state, confirming decode speed isn't
  meaningfully sensitive to context length here.

## Decision

**Proceed to Phase 3 without a model or hardware change.** Missing
FR-INF-07's raw numbers doesn't automatically mean the user-facing
experience is broken:

1. A typical spoken answer sentence (PRD's UX design plays a preamble
   immediately, then streams the real answer) is on the order of 10–20
   tokens; at ~55–72 ms/token that's 0.6–1.4 s of decode for a full
   sentence, arriving as an audio stream, not a single blocking wait — the
   preamble (Phase 6) covers the perceptual gap the same way it was always
   designed to (Architecture.md §9.1).
2. Thread count is already effectively maximized (8 physical cores, no
   further easy win there on this CPU).
3. The concrete mitigation levers Architecture.md §9.1 already named
   ("smaller evidence budget, shorter first sentence") remain available to
   Phase 5 (knowledge engine, which controls prompt assembly) if real
   end-to-end voice latency (measured in Phase 6/9) turns out to need them.
4. This is a personal, single-user, CPU-only reference machine (OQ-06); a
   GPU/NPU backend is an explicit v1.0 non-goal (phases.md §14) and not
   worth pulling forward for this gap alone.

## Consequences

- FR-INF-07's numeric targets are **not met** on this hardware; this ADR is
  the recorded disposition phases.md §5.4 requires, not a retroactive
  change to the targets themselves (they stay as the aspirational number
  for future hardware).
- Phase 5 (knowledge engine) should budget for ~55–72 ms/token decode and
  ~8–9 ms/token prefill (post-cache-reuse, roughly half the fresh-prompt
  number for the varying portion) when reasoning about end-to-end answer
  latency, not the PRD's original 45 ms/2.5 ms figures.
- Phase 6 (voice) and Phase 9 (integration soak) should re-measure
  perceived TTFA/latency end-to-end with the preamble in place before
  deciding whether further mitigation (shorter prompts, a smaller model)
  is actually needed — this ADR explicitly defers that call, it doesn't
  rule it out.
- `reports/bench/inference-1790486839.json` is the evidence of record for
  Phase 2's exit criterion; future benchmark runs should add new
  timestamped files here rather than overwrite it, so the trend is visible.

## Alternatives considered

- **Swap to a different/smaller BitNet variant** (e.g. a 1B-class model):
  rejected for now — no evidence yet that decode speed is the actual
  end-to-end bottleneck once preambles and streaming are in place; premature
  without Phase 6's real measurement.
- **Reduce max context below 512**: rejected — Architecture.md's 512-token
  prompt budget (128+64+320) is a considered design already, not slack to
  cut without redesigning Phase 5's evidence assembly.
- **Block Phase 2 exit pending a hardware upgrade or GPU backend**:
  rejected — out of scope for a personal CPU-only install (OQ-06), and
  phases.md's process is exactly "record the disposition, don't block."
