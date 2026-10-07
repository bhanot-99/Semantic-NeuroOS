# ADR-0014: Raise C3's RSS budget to 340/420 MiB

- Status: Accepted (owner approved on 2026-10-07, among: drop the transient load copy / switch to the int8 model / raise the budget)
- Date: 2026-10-07
- Relates to: BUGS.md M21 (open decision 7), PRD.md §6.2, Architecture.md §12.4

## Context

PRD §6.2 budgeted C3 at 205 MiB RSS with a 300 MiB `MemoryMax`. M21 measured
the real figures in a release build (`lto = "thin"`, three runs):

| Point in C3's lifecycle | RSS |
| :--- | :--- |
| After startup, query path warmed | 299–308 MiB |
| After ingesting 20 spool documents | 309–316 MiB |
| After a real backup | 323 MiB |
| After a real GC | 334 MiB |
| Peak during startup (`VmHWM`) | 392–401 MiB |

So C3 was over its hard limit before serving a single query, and healthd
correctly reported it DEGRADED on every start — which in practice meant the
health aggregator always showed a problem and so could not show a real one.

`/proc/<pid>/smaps` at a 305 MiB steady state decomposes as: `[heap]`
173.7 MiB (the ONNX session's weights plus LanceDB/datafusion/SQLite/arrow
buffers), 64.6 MiB of the binary's own resident text (the arrow + datafusion
+ lance graph is simply that large), `[anon]` 47.1 MiB (thread stacks and
mmap'd allocations), `libonnxruntime.so` 15.6 MiB.

The decisive fact: `model.onnx` is **126.9 MiB of f32 weights**. The 205 MiB
budget was written before C3 had an embedding model at all, and cannot hold
that model plus this dependency graph under any implementation of the
current design. It was never a target C3 missed; it was a target that did
not describe C3.

The ~95 MiB of peak over steady state is a transient second copy at load:
`Embedder::load` reads the file into a `Vec<u8>` and fastembed hands it to
ORT's `commit_from_memory`, which copies it into the session before our
vector drops.

## Decision

Raise C3 to a **340 MiB budget** and a **420 MiB hard limit** — the measured
worst case (334 MiB steady, 401 MiB peak) plus a little headroom — in
PRD §6.2, `neuroos-healthd`'s target registry and
`deploy/systemd/neuroos-storage.service`. The total in PRD §6.2, G-5,
Architecture.md §12.4 and phases.md's PF gate moves from 2,340 to 2,475 MiB.

## Verification

Re-measured independently on 2026-10-07 against the release binary built
from this branch (a fresh `neuroos-storage` with the query path warmed, real
`.dev-cache/models`): **301 MiB steady, 394 MiB peak (`VmHWM`)**. That sits
inside the new 340/420 figures and confirms M21's original 299–334 / 392–401
range, so the budget is calibrated to a measurement taken twice, in separate
sessions, rather than to one run.

## Alternatives rejected (for now)

- **Switch to the int8 quantized model** (~33 MiB, saving ~95 MiB steady and
  bringing C3 back inside roughly the original envelope). This is the
  technically best answer and should be revisited, but **not now**: it
  re-embeds the entire store and changes retrieval behaviour, and Phase 5's
  KPI-1 score is currently awaiting the owner's grading against the
  retrieval quality the f32 model produces. Changing the embedding model
  before that grading would invalidate the measurement the gate depends on.
  Tracked for Phase 7 hardening.
- **Eliminate the transient load-time copy only** (peak 395 → ~300 MiB,
  steady unchanged). Rejected as insufficient and risky: steady state would
  still be 299–334 MiB against a 300 MiB limit, so C3 would stay borderline
  DEGRADED, and buying only the peak costs leaving fastembed and
  re-implementing its tokenization and CLS pooling against ORT directly.
  Worth doing once the budget is honest and it can be judged on its own.

## Consequences

- healthd reports C3 OK in normal operation, so a DEGRADED C3 becomes
  information again.
- The laptop total rises by 135 MiB to 2,475 MiB, still well inside the
  8 GiB reference machine.
- The budget now reflects a measurement rather than an estimate, and the two
  ways to lower it are written down with what each costs.
- If Phase 6's C2 (405/520 MiB) plus C4 (1,590/1,900 MiB) make the total
  uncomfortable on smaller hardware, the int8 option above is the first
  lever to pull.
