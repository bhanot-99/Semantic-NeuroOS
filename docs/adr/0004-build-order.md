# ADR-0004: component build order — healthd, then monitor, then storage

- Status: Accepted
- Date: 2026-09-26 (decisions D-02, D-03, memory.md §6)

## Context

11 phases (0–10) deliver 8 components plus shared infrastructure
(phases.md §2). The order needs to front-load the pieces every other
component depends on, and needs real data available before the pieces that
consume it (storage, knowledge) can be meaningfully tested.

## Decision

1. **healthd is a standalone binary, built first** (D-02) — out-of-process
   resilience (Architecture.md, V3.2): if a component wedges or crashes,
   the health aggregator that notices must not be the thing that's down.
2. **C1 (monitor) before C3 (storage)** (D-03) — Phase 4's soak-replay gate
   needs real captured telemetry dumps (`tests/fixtures/telemetry/`), which
   only exist once C1 has run and produced them.
3. Full order (phases.md §2): Phase 0 foundation → Phase 1 healthd →
   Phase 2 inference (C4) → Phase 3 monitor (C1) → Phase 4 storage (C3) →
   Phase 5 knowledge (C5) → Phase 6 voice (C2) → Phase 7 kernel (C6) →
   Phase 8 fetcher (C7) → Phase 9 integration & soak → Phase 10 release.
   C4 (inference) lands before C1/C3 because it has the highest technical
   risk (BitNet AVX2 performance, spike S-04) and the least dependency on
   other components' output.

## Consequences

- Every phase after Phase 1 can register itself with healthd from day one
  (P0-S04's `neuroos-health` — "a health endpoint by adding one line").
- Phase 4's exit criteria can require a soak-replay gate against real C1
  output instead of synthetic fixtures.
- Components with the fewest cross-component dependencies (healthd,
  inference) are proven first, reducing the chance that a later phase
  discovers a foundational assumption was wrong (as happened with the
  systemd unit-mode question, ADR-0002 — better to find that class of
  problem in Phase 0/1 than Phase 6).

## Alternatives considered

- **Build in component-number order (C1 → C7)**: rejected — would build
  storage (C3) before monitor (C1) produces anything for it to store, and
  would build voice (C2) before inference (C4) exists to answer it.
- **Build the riskiest component (C4 inference) last, after easier wins**:
  rejected — discovering a BitNet AVX2 performance problem late would be
  far more costly to unwind than discovering it in Phase 2.
