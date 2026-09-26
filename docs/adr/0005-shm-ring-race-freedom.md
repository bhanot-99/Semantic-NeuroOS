# ADR-0005: memfd seqlock ring — race-freedom proof (spike S-02)

- Status: Accepted
- Date: 2026-09-26

## Context

Architecture.md §5.5 specifies the memfd seqlock ring (C4 → C2 token
streaming): single writer, single reader, `u32` seqlock (odd = writing,
even = stable), reader retries on odd/changed seqlock, `generation_id` bump
on cancel. P0-S07 (spike S-02) implements it in Rust
(`crates/neuroos-shm`) and C++ (`cpp/libneuroos/include/libneuroos/
shm_ring.hpp`) and proves it race-free.

## What was found (two real bugs, both from actually testing this)

1. **The seqlock alone doesn't prove slot identity.** A reader that stalls
   can pass the "seqlock unchanged before/after" check against a slot the
   writer has since recycled for a *different, later* message — the seqlock
   value can coincidentally match across two different stable writes to the
   same slot. Caught by `tests/ring_stress.rs`'s concurrent stress test:
   `token_id went backwards or repeated: 26589 after 26844`. **Fix:** the
   slot layout gets an explicit `seq: u64` field (not in the original
   architecture doc's byte diagram) that the reader checks against the
   sequence number it expected; a mismatch means "lapped mid-read," handled
   the same way as lapping detected up front.
2. **Plain (non-atomic) reads/writes to the payload fields are undefined
   behavior under the language memory model**, even though the seqlock
   protocol makes them safe *in practice* on real hardware. The formal
   model has no concept of "this non-atomic access is actually fine because
   of an external protocol" — it only sees two threads racing on the same
   memory. Caught by ThreadSanitizer, which the functional stress test
   alone (10M messages, zero corruption) did not and could not catch, since
   TSan checks the *model*, not just observed outcomes. **Fix:** every slot
   field, including the payload bytes, is accessed through a `Relaxed`
   atomic (`Relaxed` because the seqlock's own `Acquire`/`Release`
   operations already establish the necessary ordering, not because
   ordering doesn't matter here).

Both fixes are mirrored exactly in the C++ implementation, which follows
the same rule (atomics throughout, matching field layout including `seq`).

## Evidence

- **Functional correctness, real hardware:** 10,000,000 messages,
  single-writer/single-reader threads, zero corruption, ~1s
  (`cargo test -p neuroos-shm --release -- --ignored --nocapture
  concurrent_writer_reader_no_torn_reads_10m`). Well above the PF target
  (≥1M slots/s, phases.md §3.3).
- **ThreadSanitizer, Rust:** `RUSTFLAGS="-Z sanitizer=thread" cargo
  +nightly test -p neuroos-shm --release --tests -Z build-std --target
  x86_64-unknown-linux-gnu` — 6 race warnings before the fix, zero after,
  2,000,000 messages. Nightly-only (TSan support isn't in stable Rust);
  not part of `just ci` (D-07 pins stable). Run manually when touching
  `neuroos-shm`.
- **ThreadSanitizer, C++:** `cpp/build-tsan/shm-stress-cpp` (the `tsan`
  CMake preset). Needed `setarch $(uname -m) -R` (disable ASLR) to work
  around an unrelated TSan/mmap-placement issue
  (`FATAL: ThreadSanitizer: unexpected memory mapping`) on this machine.
  Zero race warnings after the fix. Not part of `just ci` (slow, and the
  ASLR workaround is environment-specific); run manually.
- **Cross-language interop:** `tests/contract/shm_interop.sh`, part of
  `just test-shm` (and `just ci`): a writer in one language creates the
  ring, writes 1000 messages, and announces a `/proc/<pid>/fd/<n>` path (no
  SCM_RIGHTS needed for this same-host proof — see below); a reader in the
  other language opens that path and verifies every message, byte for
  byte, both directions.

## Decision

Ship the ring as implemented: `seq`-stamped slots, all fields behind
`Relaxed` atomics, in both languages. `just test-shm` runs the default-scale
functional and interop tests in `just ci`; the 10M-message and TSan runs
are documented spike evidence, run manually, not routine CI (nightly
toolchain / nontrivial runtime).

Actual `SCM_RIGHTS` fd handoff over `inference.sock` (the real production
mechanism, Architecture.md §5.1) is **not** implemented by this spike —
`tests/contract/shm_interop.sh` uses a `/proc/<pid>/fd/<n>` re-open instead,
which is sufficient to prove the ring format and protocol are
cross-language compatible without needing the IPC wiring that's Phase 2's
job (C4 and C2 don't exist yet).

## Consequences

- Any future change to the slot layout must be made identically in
  `crates/neuroos-shm/src/ring.rs` and `cpp/libneuroos/include/libneuroos/
  shm_ring.hpp`, or the interop test will catch the mismatch.
- Phase 2 (C4 inference) and Phase 6 (C2 voice) build the real
  `SCM_RIGHTS` handoff on top of this proven ring; they inherit the
  `seq`/atomics design as-is rather than re-deriving it.
- `neuroos-shm`'s public API (`Ring::create`/`open`, `RingWriter::write`,
  `RingReader::try_read`) now rejects `slot_size` values that aren't a
  multiple of 8 (needed for `AtomicU64` alignment on every slot), which
  the original architecture doc's byte diagram didn't call out.

## Alternatives considered

- **Trust the functional stress test alone, skip TSan:** rejected — it
  passed even with the un-fixed, formally-undefined-behavior version (bug
  2), because the race in question doesn't reliably produce an observably
  wrong *value* on x86; TSan is what actually catches it.
- **Wire real `SCM_RIGHTS` for the interop test:** deferred, not rejected —
  reasonable future work once C4/C2 exist and need it anyway; not needed to
  prove what this spike set out to prove.
