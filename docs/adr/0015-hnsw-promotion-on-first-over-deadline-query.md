# ADR-0015: Promote to HNSW on the first over-deadline query, and probe for it once ingest settles

- Status: Accepted (owner directed the fix on 2026-10-07; BUG-006 reopened the same day)
- Date: 2026-10-07
- Relates to: BUGS.md BUG-006, PRD.md FR-STO-07, rules.md §5.7, ADR-0013/BUG-003 (detached index build)

## Context

FR-STO-07 says: *"Promote a collection to a USearch HNSW index only when its
p99 search latency exceeds 5.0 ms. No item-count thresholds."*

`LanceStore::record_query_latency` implemented that with a sample floor:

```rust
const MIN_SAMPLES_BEFORE_PROMOTION_CHECK: u64 = 20;
```

The floor is not in the requirement. It was added so a p99 taken off a
near-empty histogram could not promote a family on the strength of one cold
query — a reasonable guard in isolation.

It also meant C3 could not act on its own measurement until it had served 20
queries. Measured against this machine's own ~6,100-event recording, flat
scan over the real `attention` family costs **~150 ms**, which is 30x the
5 ms promotion threshold and, more importantly, past C5's own 100 ms query
deadline (rules.md §5.7). So the first 20 queries against any fresh store
were *guaranteed* to miss that deadline and come back to the user as
degraded "I couldn't retrieve evidence" answers.

This is what KPI-1 had been failing on: **21 of 59 real questions failed,
contiguously from the very first question**, then the remaining 36 succeeded.

### Why this was previously mis-diagnosed

BUG-006 was first closed against a different cause — unbounded concurrent
background distillation starving the CPU — and capped with
`MAX_CONCURRENT_DISTILLATIONS = 1`. That cap is sound on its own terms, but
it was not this bug. Two measurements separated them:

1. **Reproduced with distillation absent entirely.** A direct
   `StorageEngine::query_hybrid` loop (no socket, no C4, no distillation:
   `tests/bug006_query_latency_after_ingest_burst.rs`) shows the same curve —
   24 of 45 queries over the deadline, recovering at query 24.
2. **It is query-count-driven, not time-driven.** Letting the store settle
   for 45 s before querying changed nothing: same curve, same recovery point.
   That rules out both a warm-up cost and a compaction backlog draining,
   which were the other two candidates.

And the direct test: dropping the floor to 2 moved the recovery point from
query 24 to query 6, tracking the gate exactly.

## Decision 1: act on one decisive sample

Keep the sample floor for borderline cases, and let a single decisive sample
bypass it:

```rust
const PROMOTE_IMMEDIATELY_ABOVE: Duration = Duration::from_millis(100);
```

A query slower than this promotes its family on the first sample. The p99
check still makes the actual decision, so FR-STO-07 is unchanged: promotion
is driven by measured p99 exceeding 5 ms, never by item count. What changes
is only how long C3 waits before believing a measurement it has already
taken.

100 ms is C3's own copy of C5's query deadline. It is duplicated as a literal
rather than imported because C3 must not depend on C5a (AB-1).

### Why this threshold, and why not something else

- **A sample past 100 ms is not noise relative to a 5 ms threshold.** The
  floor exists to stop a marginal p99 being set by one outlier; 20x the
  threshold is not marginal. Borderline samples (over 5 ms, inside the
  deadline) still wait for the full floor, which is what the
  `one_borderline_sample_still_waits_for_the_floor` test pins.
- **The error costs are asymmetric.** Promoting a family early costs a
  detached index build that only ever makes queries faster (BUG-003 already
  moved that build off the triggering query). Promoting late costs the user
  ~20 unanswered questions.
- **An item-count or ingest-volume trigger was rejected**: FR-STO-07
  explicitly forbids item-count thresholds, and building the index at
  `open()` would be exactly that in disguise.

### Effect of decision 1

Measured on this machine, same recording, real 100 ms deadline:

| | Before | After |
| :--- | :--- | :--- |
| Queries over deadline (45-query isolation test) | 24 | 5 |
| KPI-1 tuned set (59 questions) | 31/59 | 46/59 |
| KPI-1 held-out set (15 questions) | 0/15 | 9/15 |
| `storage.sock` deadline errors, tuned set | 21 | 5 |

Five misses remained after this change alone: query 1 must be slow for C3 to
measure anything, and queries 2–5 overlap the ~1 s detached `create_index`.
The owner asked for those to be removed too, which is the second decision
below.

## Decision 2: a background latency probe, fired once ingest settles

C3 now queries *itself* once a family has gone `PROBE_QUIET_PERIOD` (500 ms)
without an insert, while that family is still un-indexed
(`LanceStore::maybe_spawn_latency_probe`, called from `StorageEngine::ingest`).
The probe is a real `query()` call, so it feeds the same histogram and the
same promotion decision a user query would — deliberately no separate path
whose timings could drift from the real one. It is detached (rules.md AB-11),
and stops for good once the family is promoted.

Effect, measured on the same recording:

| Gap between data landing and the first question | Queries over deadline (of 45) |
| :--- | :--- |
| None — asked the instant ingest stops | 5 → **4–5** (unchanged) |
| 3 s | 5 → **0** |

So the probe removes the remaining cost whenever anything at all happens
between data arriving and a question being asked, which is the real-world
case: C3 ingests telemetry continuously, and the gaps between events are
seconds, so a 500 ms quiet window is reached constantly. It cannot help a
harness that asks in the same instant ingest stops — which is exactly what
`kpi1_eval` does, so **KPI-1's own numbers are unchanged by this** (46/59
before, 45/59 after, run-to-run noise). The isolation test measures both
paths; `NEUROOS_BUG006_SETTLE_SECS` switches between them.

### What the first attempt got wrong, and what it revealed

The probe was first written to fire every `COMPACT_EVERY_INSERTS` (256)
inserts, like compaction. That made things **strictly worse — all 45 queries
missed the deadline instead of 5.**

The reason is worth recording, because it is a property of the design and not
of the probe: a probe 256 inserts into a 6,100-event burst measures a corpus
that is still small, promotes on it, and builds an index over that prefix.
**An HNSW index does not cover rows added after it is built, and `promoted` is
a one-shot in-memory flag** — so the family was permanently marked "indexed"
with ~95% of its rows outside the index, and every query fell back to a flat
scan with nothing left to trigger a rebuild.

Waiting for ingest to settle avoids it: the index is built over the whole
corpus. But the underlying gap remains and is now recorded in BUGS.md as its
own item — any promotion is a snapshot, and continuous telemetry ingest will
keep adding rows the index does not cover. It is also the most plausible
explanation yet for the question BUG-002 left open (why HNSW did not beat flat
scan in the 20k synthetic benchmark, when here it beats it threefold:
~150 ms → ~50 ms).

## Consequences

Guarded by `tests/bug006_query_latency_after_ingest_burst.rs`, which asserts
a miss budget and that steady-state latency is inside the deadline. It is
`#[ignore]`d and
machine-local (it needs the real embedder and the raw recording) and pinned
to the `heavy-live` nextest group — as an absolute-latency test it is
sensitive to machine load, which cost one wrong attribution during this work
(the same lesson as C14).

Separately noted, not addressed here: the store still shows ~680 data files
for `attention` after the burst, unchanged after 45 s of settling. That may
be nothing more than uncleaned old LanceDB versions, but it is worth
measuring on its own.
