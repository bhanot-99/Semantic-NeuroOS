# ADR-0001: Record architecture decisions as ADRs

- Status: Accepted
- Date: 2026-09-26

## Context

Architecture.md §13 resolves conflicts between the two source blueprints
(V2.7, V3.2) and states V3.2 is primary. Beyond that initial reconciliation,
the project needs a durable record of decisions that change architecture,
contracts, security posture, a dependency, or a budget (rules.md item 6),
separate from `memory.md`'s day-to-day decisions log (which is a live,
frequently-edited tracker, not a durable record).

## Decision

Every decision meeting rules.md item 6's bar gets an ADR at
`docs/adr/NNNN-title.md` with four sections: Context, Decision, Consequences,
Alternatives (rules.md line 32). `memory.md` §6 keeps a one-line pointer to
the ADR; the ADR is the durable record. Rules themselves (rules.md) change
only through an ADR plus an update to rules.md (rules.md line 6).

## Consequences

- Every subsequent architecturally-significant decision in this project
  (ADR-0002, ADR-0003, ADR-0004, and all later ones) follows this format.
- `memory.md`'s decisions log entries reference an ADR number once one
  exists for that decision, instead of restating the rationale.

## Alternatives considered

- Keep decisions only in `memory.md`: rejected — `memory.md` is explicitly a
  live tracker ("update every session"), not an audit trail; important
  decisions would be lost to edits or truncation over the project's life.
