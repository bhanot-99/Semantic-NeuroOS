# ADR-0013: `DetachRing` RPC, and a C4 overflow field range

- Status: Accepted (owner approved on 2026-10-07, among: add the RPC / evict the least-recently-used ring / accept the cap)
- Date: 2026-10-07
- Relates to: BUGS.md L3 (open decision 8), Architecture.md §5.3, §5.5, ADR-0005

## Context

L3 bounded `RingRegistry` at `kMaxRings` = 16 so that a client on
`inference.sock` could not exhaust C4's file descriptors and its PRD §6.2
memory budget simply by attaching rings under fresh names. Each ring is a
live memfd (256 KiB by default) plus an fd.

Nothing ever freed a ring, so the cap was a limit on how many distinct ring
names C4 could serve **in its entire lifetime**, not on how many were in use
at once. A long-running C4 that legitimately saw more than 16 names would
refuse every subsequent `AttachRing` until it restarted. Phase 6 is when
this stops being theoretical: C2 becomes a real second client with its own
ring, alongside C5a's `knowledge-text`.

A second, quieter problem sat behind the same code. `LaneScheduler`'s worker
loop and `write_terminal` both called `get_or_create` for the ring named in
a job. Their comments said this was "a lookup, not a creation" — the ring is
always attached before a job referencing it is accepted — but the call could
still create. Any path that reached it with a missing ring would silently
make a brand-new memfd that no client is mapped to and stream a whole
generation into it, and the waiting client would block until its own
deadline with no error.

C4's envelope field range (60–69, Architecture.md §5.3) is fully allocated:
Generate, Cancel, Distill, AttachRing and GetInfo take all ten numbers.

## Decision

1. **Add `DetachRingRequest`/`DetachRingResponse`** to
   `proto/neuroos/v1/inference.proto`. `RingRegistry::detach(name)` drops
   the ring, closing its memfd once C4's last reference goes. The RPC is
   **idempotent**: a name that is not attached reports `detached = false`
   and no error, so a client recovering from a restart can detach
   unconditionally.

2. **Allocate 130–139 as "C4 overflow"** in Architecture.md §5.3, and put
   the pair at 130/131. Field numbers below 130 are all spoken for, and
   renumbering an existing component's range would break the wire contract
   for no gain.

3. **Make the write paths non-creating.** `RingRegistry::get(name)` returns
   a writer for an existing ring only. The lane workers and `write_terminal`
   use it; `AttachRing` stays the only caller that may create. A miss now
   takes the already-existing "no ring to write into" branch, which logs and
   drops the job.

Detaching a ring with a generation still queued or running is therefore
**allowed and safe**: that job's next `get` misses and it is dropped with a
log. The client that detached the ring is the one that stopped reading it,
so there is nobody left to disappoint, and refusing the detach instead would
mean a client could not release a ring until its own in-flight work drained.

## Alternatives rejected

- **Evict the least-recently-written ring with no in-flight job when at the
  cap.** No new field number, no ADR. Rejected: eviction guesses. A client
  that attached a ring and is about to use it looks exactly like one that
  has abandoned it, so C4 would be deciding, from timing alone, whose
  stream to break. Explicit release puts that decision with the only party
  that knows.
- **Accept the cap and revisit in Phase 6.** Rejected because Phase 6 is
  precisely when C2 arrives, and the cheaper fix now is a 2-field additive
  proto change plus one handler.
- **Reference-count rings and free at zero.** Rejected: C4 cannot observe a
  client that died without detaching, so the count would leak exactly in
  the case the cap exists to bound.

## Consequences

- `kMaxRings` becomes a concurrency limit. C4 can serve any number of ring
  names over its life as long as no more than 16 are attached at once.
- The latent "stream into a ring nobody reads" hang is gone by construction:
  only `AttachRing` can create a ring.
- Additive proto change: field numbers 130/131 were never used, and no
  existing message changes, so a C5a or C2 that never sends `DetachRing`
  behaves exactly as before.
- C5a does not need to detach. It reuses one ring name (`knowledge-text`)
  for its whole process lifetime, which is one slot out of sixteen.

## Verification

- `cpp-inference-unit`: `test_a_detached_ring_frees_its_slot` (the freed
  slot is reusable; detach is idempotent) and `test_get_never_creates_a_ring`
  (a miss never creates, before or after a detach).
- `tests/contract/inference_smoke.sh`, against the real BitNet model: a real
  `DetachRing` over a real `inference.sock` frees the ring, a second detach
  of the same name reports `detached = false` with no error, and a
  `Generate` naming the detached ring is refused up front rather than
  accepted into a void.
