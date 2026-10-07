# Semantic-NeuroOS

**An on-device, privacy-first desktop intelligence layer for Linux.**

> A voice assistant that knows what you are working on right now, remembers what you worked on last week, and never sends a single byte of your life to the cloud.

## What this is

Semantic-NeuroOS watches what you do on your desktop (with consent), builds a private semantic memory of that activity, and answers spoken questions about it using a small local language model running entirely on-device. It can also run a limited set of actions ("skills") on your behalf, gated by a strict safety firewall.

Defining property: **Zero-Egress Guarantee.** Every component that touches personal data runs inside a network-isolated sandbox (`PrivateNetwork=true`). Exactly one component, the External Fetcher, may ever reach the internet — as a separate system user, only on explicit approval, with every byte it returns marked untrusted ("tainted").

## Capabilities

- **Sense** — records desktop context: focused window, idle state, media, system load, process trees, and chosen folders (git repos, notes, calendar files).
- **Remember** — filters noise (compiler subprocesses, its own windows, passive media), embeds what matters, stores it locally in LanceDB + SQLite.
- **Understand** — resolves "this"/"it" by snapping to what was on screen at the moment you spoke (±1.5s), retrieves evidence, builds a compact prompt. A background worker learns work rhythms and relationships as a knowledge graph.
- **Answer** — runs BitNet b1.58 2B4T on CPU; speaks the answer back with an instant spoken preamble to mask latency.
- **Act safely** — every skill gated through capability tiers (SAFE / REVIEW / DANGEROUS); untrusted web content auto-escalates SAFE skills to REVIEW, requiring HMAC-bound user confirmation.
- **Research on request** — fetches a URL only after user approval, blocks SSRF, tags results `EXTERNAL_UNTRUSTED`.
- **Stay healthy** — `neuroos-healthd` monitors every component out-of-process, enforces soak-test thresholds.

## Product goals (v1.0)

| Goal | Target |
| :--- | :--- |
| Useful context-aware answers by voice | ≥ 80% of scripted evaluation questions answered correctly |
| Absolute privacy | 0 bytes of egress from Components 1–6 and healthd |
| Responsive | Spoken preamble ≤ 50 ms after transcript ready; first answer audio ≤ 2.0s p50 |
| Safe by construction | 100% of tainted-context skill calls escalate; every skill call has an `ATTEMPT` + `RESULT` audit pair |
| Runs on a normal laptop | Steady-state RSS ≤ 2,340 MiB; idle CPU ≤ 3% on reference hardware |
| Stable for long sessions | 24h soak: RSS growth < 5%, p99 latency drift < 10% |

## Non-goals (v1.0)

Cloud sync, remote accounts, telemetry to the developer, or any "phone home" behaviour.

## Status

Five of the eleven phases are built: Phase 0 (foundation, contracts, spikes), Phase 1
(health/IPC), Phase 2 (inference) and Phase 3 (monitor) are done and merged; Phase 4
(storage) passed with approved exceptions; Phase 5 (knowledge engine) is story-complete
with its KPI-1 gate awaiting the owner's grading. Phase 6 (voice) is next.

See [`phases.md`](phases.md) for the full 11-phase roadmap, [`memory.md`](memory.md) for
live project state (the authoritative source for what is done), and [`BUGS.md`](BUGS.md)
for the open issue registry.

## Documents

- [`PRD.md`](PRD.md) — Product Requirements Document
- [`Architecture.md`](Architecture.md) — system architecture, component contracts
- [`design.md`](design.md) — detailed design
- [`rules.md`](rules.md) — engineering rules and constraints
- [`phases.md`](phases.md) — phase-by-phase delivery plan
- [`memory.md`](memory.md) — single source of truth for project state
