# ADR-0012: `ActivityItem` carries taint

- Status: Proposed (needs owner approval — rules.md §10.2.4: contract change)
- Date: 2026-10-04
- Relates to: BUGS.md M9, Architecture.md §7.4, R0-3, FR-KNO-07

## Context

Architecture.md §7.4 states `taint(output) = union(taint(inputs))`, and
R0-3 makes taint one-way: "no code path may remove `EXTERNAL_UNTRUSTED`
from derived data". FR-KNO-07 requires C5's answer to carry the union of
every piece of evidence that went into it.

BUG-007 added a second evidence path: questions about the shape of the
user's time ("what did I do most", "the last episode") are routed to C3's
structured `QueryActivity` instead of hybrid retrieval, and the returned
`ActivityItem`s are converted into `ChunkMatch`es by
`knowledge-query::evidence`. `ActivityItem` has no `taint` field, so that
conversion sets `taint: None` — twice, at `evidence.rs:230` and `:353`.

`None` is not "no taint": `prost` maps an absent `Taint` message to
`TaintFlags::empty()` downstream, so an activity-derived answer reports
clean provenance no matter what its evidence was. Both of C3's activity
sources can carry taint:

- `activity_media` reads `chunks_fts`, whose rows have a real `taint`
  column (migration `0002_chunks_fts.sql`) — a media chunk derived from a
  C7-fetched page would be `EXTERNAL_UNTRUSTED` and that flag was being
  dropped on the floor.
- `activity_windows` reads `focus_history`, which has no taint column
  because it is C1's own first-party desktop observation.

So the current behaviour is not merely incomplete, it silently lowers
taint on one real path — the thing R0-3 forbids outright.

## Decision

Add one field to `ActivityItem` in `proto/neuroos/v1/storage.proto`,
additive only (Architecture.md §5.4 — no existing field number or meaning
changes):

```proto
message ActivityItem {
  string app_id = 1;
  string text = 2;
  uint64 first_ns = 3;
  uint64 last_ns = 4;
  uint64 dwell_ms = 5;
  // M9 / ADR-0012: provenance of `text`, so an activity-derived answer
  // reports the same taint a retrieval-derived one would. Empty for
  // `focus_history` rows (C1's own observation); the chunk's real taint
  // for `chunks_fts` rows.
  Taint taint = 6;
}
```

`sqlite::ActivityRow` gains a `taint: u32`. `activity_media` selects the
column it already stores; `activity_windows` reports
`TaintFlags::empty()`, which is a positive statement about first-party
desktop telemetry rather than a missing value. `evidence.rs` passes the
item's taint through instead of `None`.

Two related taint leaks are fixed in the same change, neither of which
needs a contract change:

- `engine::fuse_rrf` dropped taint when de-duplicating by chunk text: a
  dense hit and a keyword hit with identical text kept only the *dense*
  one's taint. If the keyword hit was the `EXTERNAL_UNTRUSTED` one, its
  flag vanished. Taint is now unioned on every merge and every duplicate
  drop, per §7.4.
- `AskResult.taint` never set `MODEL_GENERATED`, even though the answer
  string is literally C4's output. It is now set on the generated-answer
  path only — the "no evidence" and capability-denied answers are fixed
  strings composed by C5, so claiming the model wrote them would be false.

## Consequences

- `QueryActivityResponse` grows one optional field. Old readers ignore it;
  a new reader against an old C3 sees an absent `Taint`, which is the same
  value it gets today.
- An activity-derived answer about web-sourced media now correctly reports
  `EXTERNAL_UNTRUSTED`, so C6's tier evaluation (`escalates_tier`) sees it.
  This is the point, but it does mean some questions that silently got a
  clean answer before will now escalate — correct, and previously a real
  prompt-injection gap (NFR-SEC-07).
- `MODEL_GENERATED` on answers makes C5's output honest about provenance
  for the Phase 7 kernel, which is the consumer that will act on it.
  Nothing in v1.0 changes tier on `MODEL_GENERATED` (§8.3: only
  `EXTERNAL_UNTRUSTED` escalates), so there is no behaviour change today.
- `focus_history` still has no taint column. If a future source ever
  writes focus rows that are not first-party observation, that assumption
  has to be revisited; the `activity_windows` call site names it.

## Alternatives considered

- **Leave `taint: None` and document it.** Rejected: it is an active R0-3
  violation on the `chunks_fts` path, not a documentation gap.
- **Add a taint column to `focus_history` too.** Rejected for now: it
  would be a migration carrying a column that is provably always 0, and
  the empty value is already correct. Revisit if a non-C1 writer appears.
- **Derive the activity item's taint in C5 by re-querying each chunk.**
  Rejected: an extra round trip per evidence line to recover data C3
  already had, and it would still be guessing for `focus_history`.
