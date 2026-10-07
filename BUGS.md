# BUGS.md — open issues

Everything from the full-codebase review of 2026-10-04 (end of Phase 5, before
Phase 6) is closed: **H1–H15, M1–M23, L1–L17, D1–D5 and C1–C14**, plus owner
decisions 1–5 and 7–9. This file now lists only what is still outstanding.

The per-item write-ups — root cause, what was wrong with the finding itself
where the review was mistaken, and the verification for each — are in the
commits that closed them, which is where to look for "why is it like this":

| Items | Closed in |
| :--- | :--- |
| H1–H15 | `e4ea394` |
| M1–M10 | `1daba7c` |
| M11–M22, C11 | `df5e487` (and `4fa98c7`, `de73c99`) |
| L1–L17 | `b0d541e` |
| D1–D5, C1–C10, C12 | `8afab9e` |
| M23, C13, C14, and decisions 4/5/7/8/9 | `0181405` |

About 33 comments in the code still cite items by id (`// BUGS.md D5`,
`// L3/ADR-0013`, `// C13:` and so on). Those ids are not stale — map the id to
its commit with the table above and `git show` it. The ranges are contiguous, so
any `H`, `M`, `L`, `D` or `C` number lands in exactly one row. Decisions taken
along the way are recorded as ADRs: **0011** (storage maintenance RPC), **0012**
(`ActivityItem.taint`), **0013** (`DetachRing` + the C4 overflow field range),
**0014** (C3's memory budget).

Last verified on `0181405`: `just fmt-check` and `just lint` exit 0;
`cargo nextest run --workspace` 421 passed / 0 failed / 36 skipped;
`cargo deny check` all four checks ok; Python 45 passed; the four cpp test
binaries and every `tests/contract/*.sh` pass, including the real-model
`inference_smoke` (`monitor_pf` skips without a graphical session);
`neuroos-storage`'s live `--ignored` suite 151/151 and C5a's 79/79.

---

## 🔴 Needs the owner

### 1. Purge the pre-M8 fixture blobs from public git history

`tests/fixtures/telemetry/*.bin` were first committed in `c70a70b`, anonymised
with M8's old **unsalted FNV-1a** hash: guess a window title, hash it, match the
placeholder. The four fixtures in the tree have since been re-anonymised with
the fixed keyed tool (HMAC-SHA256 under a per-run random key), so a snapshot —
a tarball, a shallow clone — is sound.

**That is not the real exposure.** The original blobs are still reachable through
git history, and **this repository is public**, so anyone can read them and
attack them directly. Re-anonymising the tree did not reduce that.

The raw originals are gone (`.dev-cache/telemetry-raw/` holds only a later 4 h
dump from 2026-09-29), so re-deriving clean fixtures from source is not an
option either.

Removing the exposure means `git filter-repo` over the `c70a70b` versions of
those paths, followed by a force-push. That rewrites a merged commit on a public
`main` and breaks every existing clone, fork and PR ref, which is why it was not
done on anyone's behalf. **Decision needed: do it, or accept and record the
residual risk.**

### 2. ~~KPI-1 grading (closes Phase 5)~~ — **graded and passed 2026-10-07**

Not a bug — the Phase 5 gate. **The owner graded the answer quality on 2026-10-07
and accepted it**, so KPI-1 is met: 48/59 = 81% against phases.md §8.4's "≥ 80%"
target. Phase 5's gate is closed. The deadline failure below (item 3) is a
separate C3 latency defect, not a KPI-1 quality result, and is still open.

The measurement detail is kept here because it was previously recorded wrongly:

Re-run 2026-10-07 against the same single raw dump both question sets were
written from (`dump-20260929-4h.bin`, sha256 `7ad670e5…`, unmodified since
2026-09-29; 6103 events ingested). Transcripts are in `.dev-cache/kpi1/`
(gitignored — they quote real browsing history). **Two corrections to what this
file previously said:**

1. The transcripts were *not* in `.dev-cache/telemetry-raw/` — only the question
   files are. The harness writes to `/tmp` by default
   (`kpi1_eval.rs:56`), and `/tmp` had been cleared, so the 2026-10-04 numbers
   had nothing behind them until this re-run reproduced them.
2. **The recorded 48/59 and 10/15 hold only with the 100 ms C3 query deadline
   lifted.** At the real deadline the scores are 31/59 and 0/15 — see item 3.

| Run | Deadline | Tuned (59) | Held-out (15) |
| :--- | :--- | :--- | :--- |
| 2026-10-04 (recorded) | lifted (undocumented) | 48/59 = 81% | 10/15 = 67% |
| 2026-10-07 re-run | **real 100 ms** | **31/59 = 53%** | **0/15 = 0%** |
| 2026-10-07 re-run | lifted (30 s, diagnostic) | 48/59 = 81% | 11/15 = 73% |

Answer quality is genuinely ~81%: on the 38 tuned questions that got evidence at
the real deadline, 31 are correct (82%). Retrieval-only (no deadline, no
generation) scores 56/59, so 56/59 is the ceiling generation can reach and the
corpus still matches the questions. **What to grade is the quality, from
`.dev-cache/kpi1/tuned-59-nodeadline.md` and `heldout-15-nodeadline.md`; the
deadline failure is a separate engineering defect, not a quality verdict.**

---

## 🟠 Open engineering work

### 3. BUG-006 is NOT fixed: the first ~21 C3 queries blow the 100 ms deadline

Reopened 2026-10-07. BUG-006 was closed on the theory that unbounded concurrent
distillation starved the CPU, and `MAX_CONCURRENT_DISTILLATIONS = 1`
(`distill.rs:33`) was the fix. The symptom is unchanged in magnitude: BUG-006
was recorded as "~20/51 questions", and the 2026-10-07 re-run fails **21/59**
with `storage.sock request failed: read deadline exceeded`.

What is new and makes the old root cause doubtful:

- The failures are **contiguous from question 1** through 21 (plus Q23), then
  stop for the remaining 36 questions. A distillation backlog would build up
  over the first few questions, not be worst at the very first one; D-19's note
  explicitly records that "the very first call alone reliably succeeds".
- Lifting the deadline to 30 s makes **all** of them pass (0 errors, 48/59 and
  11/15). So it is purely latency, never a hang, an error or a wrong result.

Front-loaded latency that decays suggests a **warm-up cost that amortises** —
most plausibly LanceDB fragmentation from the 6103-event ingest burst, with the
background compaction added for BUG-007 (every 256 inserts) only catching up
around query ~21. That is a hypothesis, not a diagnosis; it has not been
measured. The competing explanation is still CPU contention from background
distillation decodes.

**Why it matters beyond KPI-1:** this is a real user-facing defect, not a test
artifact. After any burst of ingest, roughly the first 20 questions a user asks
return a degraded "I couldn't retrieve evidence" answer. Needs its own
systematic-debugging session: instrument real C3 query latency per query index,
then fix the cause rather than the deadline.

### 4. M10: the distillation cache is written but never read

`DistillationCache::get` is public, entries carry their taint and a
`stored_at_ns`, and nothing blocks a consumer. What is missing is the *contract*,
not the code: design.md §9.2 specifies only "listen 8 s for a follow-up without
the wake word" and says nothing about how a later turn keys into the cache (per
question text? per focus window? per session?) or how stale an entry may be.

Those answers come out of C2's turn-taking state machine — **Phase 6 story
P6-S08**. Wiring a lookup before then would invent the contract a phase early
(R0-8). This is Phase 6 work, not a defect.

### 5. Phase 7: bring C3's RSS back down (ADR-0014)

C3's budget was raised to 340 MiB / 420 MiB because 205/300 predated C3 having
an embedding model and was unreachable by any implementation of this design
(`bge-small-en-v1.5` is 126.9 MiB of f32 weights alone). Measured 301 MiB steady
/ 394 MiB peak.

The int8 quantized model (~33 MiB, saving ~95 MiB steady) is the better answer
and would bring C3 back inside roughly the original envelope. It was **not** taken
now because it re-embeds the whole store and changes retrieval behaviour, and
KPI-1 is currently awaiting grading against the retrieval quality the f32 model
produces. Revisit once that grading is done. ADR-0014 also records a second,
smaller option: dropping the transient load-time copy (peak only, ~95 MiB).

---

## ✅ Closed by owner decision

### BUG-002: the 20k-item query latency target — **accepted 2026-10-07**

Phase 4's PF target is p50 ≤ 13 ms / p99 ≤ 20 ms at 20,000 items. Two real
overhead sources were found and fixed (per-call table re-opening; 192-file
fragmentation), bringing flat scan to ~35–42 ms. The literal target is still
missed, and that is inherent flat-scan cost.

**The owner accepted this permanently on 2026-10-07** rather than spend a
session on it. Rationale: the target was set before C3 had an embedding model,
and ~35–42 ms sits comfortably inside C5's own 100 ms query deadline, so nothing
downstream misses its budget because of it. Why HNSW does not beat flat scan at
this corpus size stays unexplained and is no longer tracked as open work — it
would only matter again if the corpus grew far past 20k items or the 100 ms
deadline tightened.

The benchmark (`query_latency_at_20k_synthetic_items`) asserts only a 5 s sanity
bound, so it passes; it does not gate on the target. **Reopen if** a real corpus
exceeds ~20k items and query latency approaches the 100 ms C5 deadline.

---

## 🟡 Standing watch items

These are closed with a deliberate compromise and a condition that should
reopen them.

| What | Reopen when |
| :--- | :--- |
| `deny.toml` ignores **RUSTSEC-2024-0436** (`paste` unmaintained; a proc-macro three levels inside vendored lancedb, no vulnerability, nothing to patch locally) | lancedb drops `paste`, or the advisory becomes a vulnerability |
| **C10**: `cpp/neuroos-inference/CMakeLists.txt` writes `bitnet-lut-kernels.h` into the bitnet.cpp submodule worktree. Cannot be relocated — ggml lists it by a fixed relative path in `add_library` and CMake hard-errors on a missing source. `.gitmodules` sets `ignore = untracked` so it cannot show as a dirty tree | upstream makes the header optional, or the pin moves |
| **C3**: C5a's `inference_client` is the one request site not on `neuroos_ipc::request_once` — it owns its stream across a whole streaming exchange, so it is not a request/response call | it stops being a streaming client |
| **OQ-01** (PRD §13): final wake phrase, defaulting to openWakeWord's stock "hey jarvis" (already in `models/manifest.toml`) | Phase 6 — it is a Phase 6 exit criterion |

---

## ⚫ Lessons that cost real time twice

Kept because both have already caused a wrong diagnosis.

- **A failing test here often means the tree, not the code.** `cargo clean
  --profile dev` left `roundtrip.sh`, `sandboxed_echo.sh` and
  `landlock_services.sh` failing against missing binaries until `just build-rust`
  ran. BLOCKER-001/C12 was the same lesson from the other direction: a disk-full
  `cargo build` failed silently and the tests then ran against stale binaries.
  `just prune` and the thin `[profile.dev]` address the cause; the habit is to
  rebuild before believing a contract-script failure.
- **Check parallelism before believing a live-test failure** (C14). Real-model
  tests starve each other: four BitNet instances against a 30 s deadline all
  fail, and each passes in about a second alone. `.config/nextest.toml` pins the
  `heavy-live` and `real-inference` groups to one thread. Verify a new group
  binds with `cargo nextest show-config test-groups` — a filter using `test()`
  where the names are binaries matches nothing, silently.
