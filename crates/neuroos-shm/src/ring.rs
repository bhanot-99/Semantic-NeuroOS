//! Single-writer/single-reader memfd seqlock ring (Architecture.md §5.5).
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::memfd::{SharedMap, create_memfd, fd_size, memfd_as_fd};

pub const MAGIC: u32 = 0x4E52_5452; // "NRTR"
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 64;
pub const DEFAULT_CAPACITY_SLOTS: u32 = 1024;
pub const DEFAULT_SLOT_SIZE: u32 = 256;

// slot byte layout, extending Architecture.md §5.5 with an explicit `seq`
// field (see `RingReader::try_read` doc comment for why: without it, a
// reader lapped mid-read can pass the seqlock stability check against a
// slot the writer has since recycled for a *different, later* message,
// silently returning the wrong data instead of detecting the lap). Found by
// `tests/ring_stress.rs` during the P0-S07 spike; also explicit padding for
// u64 alignment, not shown in the architecture doc's byte diagram:
//   [0..4)   seqlock: u32 (odd = writing, even = stable)
//   [4..8)   padding
//   [8..16)  seq: u64 (absolute ring position this slot instance was written at)
//   [16..24) generation_id: u64
//   [24..28) token_id: u32
//   [28..30) flags: u16
//   [30..32) utf8_len: u16
//   [32..slot_size) utf8_bytes
const SLOT_SEQLOCK_OFF: usize = 0;
const SLOT_SEQ_OFF: usize = 8;
const SLOT_GENERATION_OFF: usize = 16;
const SLOT_TOKEN_ID_OFF: usize = 24;
const SLOT_FLAGS_OFF: usize = 28;
const SLOT_UTF8_LEN_OFF: usize = 30;
const SLOT_PAYLOAD_OFF: usize = 32;

/// Terminal slot: the generation ended normally.
pub const FLAG_EOS: u16 = 1 << 0;
/// Terminal slot: the generation was cancelled (H8 sets `FLAG_EOS |
/// FLAG_CANCEL` together, so a reader that waits only on `FLAG_EOS` still
/// terminates).
///
/// D2 listed this as dead code because no Rust code reads it. It is not
/// removable: it is one half of the shm slot's wire contract with C4, whose
/// `neuroos::shm::kFlagCancel` (cpp/libneuroos/include/libneuroos/shm_ring.hpp)
/// writes this exact bit. Dropping the Rust side would leave the layout
/// documented in only one language. Both sides now pin the literal wire
/// values -- `static_assert` there, `wire_flag_values_are_pinned` here -- so
/// a renumbering on either side fails its own build.
pub const FLAG_CANCEL: u16 = 1 << 1;

#[derive(Debug, thiserror::Error)]
pub enum RingError {
    #[error("payload of {0} bytes exceeds max slot payload of {1} bytes")]
    PayloadTooLarge(usize, usize),
    #[error("bad ring header: {0}")]
    BadHeader(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPiece {
    pub token_id: u32,
    pub flags: u16,
    pub payload: Vec<u8>,
}

/// A mapped ring: owns the shared mapping (and, for the creator, the memfd).
pub struct Ring {
    map: SharedMap,
    capacity_slots: u32,
    slot_size: u32,
    fd: OwnedFd,
}

impl Ring {
    /// Creates a fresh memfd-backed ring. The returned `Ring` owns the fd;
    /// pass `ring.fd()` (via `SCM_RIGHTS`, or plain fd inheritance across a
    /// `fork`) to the peer process, which maps it with `Ring::open`.
    pub fn create(name: &str, capacity_slots: u32, slot_size: u32) -> Result<Self, RingError> {
        let total = validate_layout(VERSION, capacity_slots, slot_size)?;
        let fd = create_memfd(name, total)?;
        let map = SharedMap::new(memfd_as_fd(&fd), total)?;
        let ring = Self {
            map,
            capacity_slots,
            slot_size,
            fd,
        };
        ring.header().init(capacity_slots, slot_size);
        Ok(ring)
    }

    /// Maps a ring whose memfd was received from another process. The size
    /// and slot layout are read back from the file itself (via `fstat` and
    /// the header), so no out-of-band metadata is needed.
    pub fn open(fd: OwnedFd) -> Result<Self, RingError> {
        let size = fd_size(memfd_as_fd(&fd))?;
        if size < HEADER_SIZE {
            return Err(RingError::BadHeader("fd too small to contain a header"));
        }
        let map = SharedMap::new(memfd_as_fd(&fd), size)?;
        let header = unsafe { &*(map.as_ptr() as *const RawHeader) };
        if header.magic != MAGIC {
            return Err(RingError::BadHeader("magic mismatch"));
        }
        let capacity_slots = header.capacity_slots;
        let slot_size = header.slot_size;
        // M1: the header is shared memory a peer process writes, so every
        // field it declares is untrusted input and gets the same checks
        // `create` applies to its arguments -- including `version`, which
        // nothing used to read: a peer built against a different slot
        // layout would otherwise be parsed with this layout's offsets.
        let expected = validate_layout(header.version, capacity_slots, slot_size)?;
        if expected != size {
            return Err(RingError::BadHeader(
                "declared capacity does not match fd size",
            ));
        }
        Ok(Self {
            map,
            capacity_slots,
            slot_size,
            fd,
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        memfd_as_fd(&self.fd)
    }

    fn header(&self) -> &RawHeader {
        unsafe { &*(self.map.as_ptr() as *const RawHeader) }
    }

    fn slot_ptr(&self, idx: u32) -> *mut u8 {
        unsafe {
            self.map
                .as_ptr()
                .add(HEADER_SIZE + idx as usize * self.slot_size as usize)
        }
    }

    fn max_payload(&self) -> usize {
        self.slot_size as usize - SLOT_PAYLOAD_OFF
    }

    pub fn writer(&self) -> RingWriter<'_> {
        RingWriter { ring: self }
    }

    pub fn reader(&self) -> RingReader<'_> {
        RingReader {
            ring: self,
            next_seq: 0,
            only_generation: None,
        }
    }

    /// H7: a reader that returns only `generation`'s slots -- a client
    /// reading its own job's tokens by the `generation_id` its
    /// `GenerateResponse` carried, regardless of which job the ring's
    /// header has moved on to since.
    pub fn reader_for_generation(&self, generation: u64) -> RingReader<'_> {
        RingReader {
            ring: self,
            next_seq: 0,
            only_generation: Some(generation),
        }
    }
}

/// Checks a ring's layout parameters -- from `create`'s arguments or from a
/// peer-written header (M1) -- and returns the total mapping size.
fn validate_layout(version: u16, capacity_slots: u32, slot_size: u32) -> Result<usize, RingError> {
    if version != VERSION {
        return Err(RingError::BadHeader("unsupported ring version"));
    }
    if capacity_slots == 0 {
        // `seq % capacity_slots` in both `RingWriter::write_as` and
        // `RingReader::try_read` would divide by zero.
        return Err(RingError::BadHeader("capacity_slots must be at least 1"));
    }
    if (slot_size as usize) <= SLOT_PAYLOAD_OFF {
        return Err(RingError::BadHeader(
            "slot_size too small to hold slot metadata",
        ));
    }
    if !slot_size.is_multiple_of(8) {
        // every slot must be 8-byte aligned for the AtomicU64 field accesses in
        // `ring.rs`'s field helpers (HEADER_SIZE is a multiple of 8; mmap pages
        // always are, so this is the only alignment requirement left to enforce).
        return Err(RingError::BadHeader("slot_size must be a multiple of 8"));
    }
    (capacity_slots as usize)
        .checked_mul(slot_size as usize)
        .and_then(|slots| slots.checked_add(HEADER_SIZE))
        .ok_or(RingError::BadHeader(
            "ring layout overflows the address space",
        ))
}

#[repr(C, align(64))]
struct RawHeader {
    magic: u32,
    version: u16,
    flags: u16,
    capacity_slots: u32,
    slot_size: u32,
    generation_id: AtomicU64,
    write_seq: AtomicU64,
}

impl RawHeader {
    fn init(&self, capacity_slots: u32, slot_size: u32) {
        // SAFETY: only the creator calls this, once, before sharing the fd.
        unsafe {
            std::ptr::write(std::ptr::addr_of!(self.magic) as *mut u32, MAGIC);
            std::ptr::write(std::ptr::addr_of!(self.version) as *mut u16, VERSION);
            std::ptr::write(std::ptr::addr_of!(self.flags) as *mut u16, 0);
            std::ptr::write(
                std::ptr::addr_of!(self.capacity_slots) as *mut u32,
                capacity_slots,
            );
            std::ptr::write(std::ptr::addr_of!(self.slot_size) as *mut u32, slot_size);
        }
        self.generation_id.store(0, Ordering::Relaxed);
        self.write_seq.store(0, Ordering::Relaxed);
    }
}

fn seqlock_atomic(slot: *mut u8) -> &'static AtomicU32 {
    unsafe { AtomicU32::from_ptr(slot.add(SLOT_SEQLOCK_OFF).cast()) }
}

// The seqlock protocol (odd = writing, even = stable, retry on mismatch)
// makes concurrent access to a slot's payload safe on real hardware, but
// that isn't enough on its own: plain, non-atomic reads/writes to memory
// another thread may be concurrently mutating are undefined behavior under
// the Rust (and C++) memory model regardless of what a higher-level
// protocol guarantees about the *values* involved — the model has no
// concept of "seqlock", only of racing accesses. Every field access below
// goes through a `Relaxed` atomic instead of `read_unaligned`/
// `write_unaligned`: `Relaxed` because ordering is already established by
// the seqlock's own `Acquire`/`Release` operations, not because ordering
// doesn't matter. Found by running the stress test under ThreadSanitizer
// (`cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu`
// with `RUSTFLAGS="-Z sanitizer=thread"`), which flagged the original
// unaligned-pointer version as a data race even though 10M-plus messages
// ran correctly outside TSan — a real gap this spike (P0-S07) was written
// to catch. `Ring::create`/`Ring::open` require `slot_size % 8 == 0` so
// every slot is 8-byte aligned (mmap pages always are; `HEADER_SIZE` is a
// multiple of 8), which `AtomicU16`/`AtomicU32`/`AtomicU64::from_ptr`
// require.

fn u16_atomic(ptr: *mut u8) -> &'static AtomicU16 {
    unsafe { AtomicU16::from_ptr(ptr.cast()) }
}

fn u32_atomic(ptr: *mut u8) -> &'static AtomicU32 {
    unsafe { AtomicU32::from_ptr(ptr.cast()) }
}

fn u64_atomic(ptr: *mut u8) -> &'static AtomicU64 {
    unsafe { AtomicU64::from_ptr(ptr.cast()) }
}

/// # Safety
/// `dst` must point to at least `src.len()` valid, writable bytes.
unsafe fn atomic_copy_to_slot(dst: *mut u8, src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        unsafe { AtomicU8::from_ptr(dst.add(i)).store(b, Ordering::Relaxed) };
    }
}

/// # Safety
/// `src` must point to at least `len` valid, readable bytes.
unsafe fn atomic_copy_from_slot(src: *const u8, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = unsafe { AtomicU8::from_ptr(src.add(i) as *mut u8).load(Ordering::Relaxed) };
    }
    out
}

/// Single-writer handle. Constructing more than one per `Ring` is allowed by
/// the type system but violates the single-writer contract (Architecture.md
/// §5.5); callers are responsible for having exactly one.
pub struct RingWriter<'a> {
    ring: &'a Ring,
}

impl RingWriter<'_> {
    pub fn generation_id(&self) -> u64 {
        self.ring.header().generation_id.load(Ordering::Acquire)
    }

    /// Bumps the generation (used on cancel, Architecture.md §5.5); readers
    /// discard slots from older generations.
    pub fn cancel(&self) -> u64 {
        self.ring
            .header()
            .generation_id
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }

    /// Writes one slot stamped with the header's current generation.
    pub fn write(&self, token_id: u32, flags: u16, payload: &[u8]) -> Result<(), RingError> {
        let generation = self.ring.header().generation_id.load(Ordering::Acquire);
        self.write_as(generation, token_id, flags, payload)
    }

    /// H7: writes one slot stamped with `generation` -- the job's own id,
    /// fixed at submit time -- not the header's current one, which moves on
    /// as soon as the next job on this (shared) ring is submitted.
    pub fn write_as(
        &self,
        generation: u64,
        token_id: u32,
        flags: u16,
        payload: &[u8],
    ) -> Result<(), RingError> {
        let max = self.ring.max_payload();
        if payload.len() > max {
            return Err(RingError::PayloadTooLarge(payload.len(), max));
        }
        let header = self.ring.header();
        let seq = header.write_seq.load(Ordering::Relaxed);
        let idx = (seq % self.ring.capacity_slots as u64) as u32;
        let slot = self.ring.slot_ptr(idx);
        let seqlock = seqlock_atomic(slot);

        let cur = seqlock.load(Ordering::Relaxed);
        seqlock.store(cur.wrapping_add(1), Ordering::Release); // odd: writing
        // M19: the `Release` above keeps *earlier* work from moving after
        // the odd marker, which is not what a seqlock writer needs; it
        // needs the field stores below to stay *after* it, so a reader
        // that has already seen an even counter cannot also see a
        // half-written field. That requires a release fence here. On x86
        // stores are not reordered with stores, so the old code was
        // correct there by accident; on a weakly-ordered CPU (ARM, which
        // Architecture.md §12 lists as a target) it is not.
        std::sync::atomic::fence(Ordering::Release);

        u64_atomic(unsafe { slot.add(SLOT_SEQ_OFF) }).store(seq, Ordering::Relaxed);
        u64_atomic(unsafe { slot.add(SLOT_GENERATION_OFF) }).store(generation, Ordering::Relaxed);
        u32_atomic(unsafe { slot.add(SLOT_TOKEN_ID_OFF) }).store(token_id, Ordering::Relaxed);
        u16_atomic(unsafe { slot.add(SLOT_FLAGS_OFF) }).store(flags, Ordering::Relaxed);
        u16_atomic(unsafe { slot.add(SLOT_UTF8_LEN_OFF) })
            .store(payload.len() as u16, Ordering::Relaxed);
        // SAFETY: we hold the only writer handle for this ring (single-writer
        // contract), `payload.len() <= max_payload` was checked above, and
        // we have marked the slot odd, so a well-behaved reader (see
        // `RingReader::try_read`) will retry rather than read these bytes
        // mid-write.
        unsafe { atomic_copy_to_slot(slot.add(SLOT_PAYLOAD_OFF), payload) };

        // The `Release` here is the half that works as written: it keeps
        // the field stores above from moving past the even marker.
        seqlock.store(cur.wrapping_add(2), Ordering::Release); // even: stable
        header.write_seq.store(seq + 1, Ordering::Release);
        Ok(())
    }
}

/// Single-reader handle; see `RingWriter` for the same caveat.
pub struct RingReader<'a> {
    ring: &'a Ring,
    next_seq: u64,
    only_generation: Option<u64>,
}

impl RingReader<'_> {
    /// Returns the next token piece, or `None` if the writer hasn't produced
    /// one yet. Retries internally on a torn read (writer mid-write) or a
    /// stale-generation slot (a cancel happened); if the reader has fallen
    /// behind by a full lap of the ring, it resyncs to the oldest slot the
    /// writer hasn't yet overwritten rather than returning corrupted data.
    pub fn try_read(&mut self) -> Option<TokenPiece> {
        let header = self.ring.header();
        loop {
            let write_seq = header.write_seq.load(Ordering::Acquire);
            if self.next_seq >= write_seq {
                return None;
            }
            if write_seq - self.next_seq > self.ring.capacity_slots as u64 {
                // Lapped: resync to the oldest slot the writer hasn't overwritten.
                self.next_seq = write_seq - self.ring.capacity_slots as u64;
            }

            let seq = self.next_seq;
            let idx = (seq % self.ring.capacity_slots as u64) as u32;
            let slot = self.ring.slot_ptr(idx);
            let seqlock = seqlock_atomic(slot);

            let before = seqlock.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue; // writer is mid-write to this slot
            }

            // See the comment above `seqlock_atomic` for why every field
            // here is read through a `Relaxed` atomic rather than a plain
            // pointer read, even though the seqlock check below is what
            // actually establishes whether the read was torn.
            let slot_seq = u64_atomic(unsafe { slot.add(SLOT_SEQ_OFF) }).load(Ordering::Relaxed);
            let generation =
                u64_atomic(unsafe { slot.add(SLOT_GENERATION_OFF) }).load(Ordering::Relaxed);
            let token_id =
                u32_atomic(unsafe { slot.add(SLOT_TOKEN_ID_OFF) }).load(Ordering::Relaxed);
            let flags = u16_atomic(unsafe { slot.add(SLOT_FLAGS_OFF) }).load(Ordering::Relaxed);
            let len =
                u16_atomic(unsafe { slot.add(SLOT_UTF8_LEN_OFF) }).load(Ordering::Relaxed) as usize;
            // M1: `len` comes from shared memory, so it is untrusted even
            // though our own writer never stores more than `max_payload`
            // -- a torn read, a corrupted mapping or a hostile peer can
            // all produce a larger value, and copying it would read past
            // the slot (past the whole mapping, for the last slot). Clamp
            // the copy, then decide below -- once the seqlock says the
            // read was stable -- whether this was a tear to retry or a
            // genuinely corrupt slot to skip.
            let max = self.ring.max_payload();
            // SAFETY: the copy length is at most `max_payload`, so it stays
            // inside this slot, which the mapping covers in full.
            let payload =
                unsafe { atomic_copy_from_slot(slot.add(SLOT_PAYLOAD_OFF), len.min(max)) };

            // M19: the mirror of the writer's missing fence. `Acquire` on
            // the load below orders *later* reads after it; a seqlock
            // reader needs the field reads above to stay *before* it, or
            // the stability check is made against values read after it.
            std::sync::atomic::fence(Ordering::Acquire);
            let after = seqlock.load(Ordering::Relaxed);
            if after != before {
                continue; // torn read; retry the same seq
            }
            if len > max {
                // A stable read of an impossible length: the slot is
                // corrupt, so skip it rather than hand its bytes up or
                // spin on it forever.
                self.next_seq = seq + 1;
                continue;
            }

            // The seqlock alone only proves this read wasn't torn — it does
            // NOT prove the slot still holds the message we asked for. If
            // the writer lapped us entirely between our top-of-loop check
            // and this read (recycled this exact slot for a later message,
            // leaving the seqlock in a new-but-still-stable state), `before
            // == after` can hold while `slot_seq != seq`. Checking the
            // slot's own stamped sequence number is what actually detects
            // that: the `write_seq`-distance heuristic this replaced could
            // both false-negative (miss a lap) and false-positive.
            if slot_seq != seq {
                continue; // lapped; top-of-loop resync handles it next iteration
            }

            self.next_seq = seq + 1;
            let wanted = match self.only_generation {
                Some(only) => only,
                None => header.generation_id.load(Ordering::Acquire),
            };
            if generation != wanted {
                continue; // another job's slot, or a stale (cancelled) one; skip it
            }
            return Some(TokenPiece {
                token_id,
                flags,
                payload,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn payloads(mut reader: RingReader<'_>) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| reader.try_read().map(|p| p.payload)).collect()
    }

    /// D2: the slot flags are a wire contract with C4's C++ side, which
    /// pins the same two literals with `static_assert`
    /// (cpp/libneuroos/include/libneuroos/shm_ring.hpp). FLAG_CANCEL has no
    /// Rust reader, so this assertion is the only thing that would catch a
    /// renumbering here.
    #[test]
    fn wire_flag_values_are_pinned() {
        assert_eq!(FLAG_EOS, 1, "must match C++ kFlagEos");
        assert_eq!(FLAG_CANCEL, 2, "must match C++ kFlagCancel");
    }

    /// M1: a zero-capacity ring would divide by zero in `write_as` /
    /// `try_read` (`seq % capacity_slots`), so it must be rejected at
    /// construction.
    #[test]
    fn create_rejects_a_zero_capacity_ring() {
        let err = match Ring::create("zero-capacity", 0, 64) {
            Ok(_) => panic!("expected Ring::create to reject a zero capacity"),
            Err(err) => err,
        };
        assert!(matches!(err, RingError::BadHeader(_)), "{err:?}");
    }

    /// `Ring` is not `Debug`, so `unwrap_err` is unavailable.
    fn open_err(fd: OwnedFd) -> RingError {
        match Ring::open(fd) {
            Ok(_) => panic!("expected Ring::open to reject this header"),
            Err(err) => err,
        }
    }

    /// Builds a memfd whose header declares `capacity_slots`/`slot_size`
    /// (and `version`) without going through `Ring::create`, so `open`'s
    /// own validation is what is under test. `fill` sizes the fd to match
    /// what the header declares.
    fn raw_ring_fd(version: u16, capacity_slots: u32, slot_size: u32, size: usize) -> OwnedFd {
        let fd = create_memfd("raw-ring", size).unwrap();
        let map = SharedMap::new(memfd_as_fd(&fd), size).unwrap();
        let base = map.as_ptr();
        unsafe {
            std::ptr::write(base.cast::<u32>(), MAGIC);
            std::ptr::write(base.add(4).cast::<u16>(), version);
            std::ptr::write(base.add(6).cast::<u16>(), 0u16);
            std::ptr::write(base.add(8).cast::<u32>(), capacity_slots);
            std::ptr::write(base.add(12).cast::<u32>(), slot_size);
        }
        fd
    }

    #[test]
    fn open_rejects_a_zero_capacity_header() {
        let fd = raw_ring_fd(VERSION, 0, 64, HEADER_SIZE);
        let err = open_err(fd);
        assert!(matches!(err, RingError::BadHeader(_)), "{err:?}");
    }

    /// M1: `create` rejects a slot too small to hold the 32-byte slot
    /// metadata, but `open` trusted the header, leaving `max_payload` to
    /// underflow.
    #[test]
    fn open_rejects_a_slot_size_too_small_for_metadata() {
        let fd = raw_ring_fd(VERSION, 2, 16, HEADER_SIZE + 32);
        let err = open_err(fd);
        assert!(matches!(err, RingError::BadHeader(_)), "{err:?}");
    }

    /// M1: the header carries a `version` that nothing ever checked, so a
    /// peer built against a future slot layout would be read with this
    /// layout's offsets.
    #[test]
    fn open_rejects_an_unknown_version() {
        let fd = raw_ring_fd(VERSION + 1, 2, 64, HEADER_SIZE + 128);
        let err = open_err(fd);
        assert!(matches!(err, RingError::BadHeader(_)), "{err:?}");
    }

    /// M1: `utf8_len` comes from shared memory a peer (or a corrupted
    /// mapping) controls. Without a `<= max_payload` bound the reader
    /// copies past the end of the slot -- past the end of the mapping for
    /// the last slot. The slot is corrupt, so it is skipped, not returned.
    #[test]
    fn a_corrupt_utf8_len_does_not_read_past_the_slot() {
        let ring = Ring::create("corrupt-len", 2, 64).unwrap();
        let writer = ring.writer();
        writer.write(0, 0, b"first").unwrap();
        writer.write(1, 0, b"second").unwrap();
        // Corrupt the first slot's length past the end of the mapping.
        u16_atomic(unsafe { ring.slot_ptr(0).add(SLOT_UTF8_LEN_OFF) })
            .store(u16::MAX, Ordering::Relaxed);

        let mut reader = ring.reader();
        let piece = reader
            .try_read()
            .expect("the intact slot is still readable");
        assert_eq!(piece.token_id, 1);
        assert_eq!(piece.payload, b"second".to_vec());
        assert_eq!(reader.try_read(), None);
    }

    /// H7: job A is still decoding when job B is submitted on the same
    /// ring (bumping the header). A's remaining slots must still reach
    /// only A's reader, and B's only B's.
    #[test]
    fn a_reader_for_one_generation_never_sees_another_jobs_slots() {
        let ring = Ring::create("generation-test", 16, 64).unwrap();
        let writer = ring.writer();
        let job_a = writer.cancel();
        writer.write_as(job_a, 0, 0, b"a1").unwrap();
        let job_b = writer.cancel(); // B submitted while A is running
        writer.write_as(job_a, 1, 0, b"a2").unwrap();
        writer.write_as(job_b, 2, 0, b"b1").unwrap();
        writer.write_as(job_a, 3, FLAG_EOS, b"").unwrap();

        assert_eq!(
            payloads(ring.reader_for_generation(job_a)),
            vec![b"a1".to_vec(), b"a2".to_vec(), Vec::new()]
        );
        assert_eq!(
            payloads(ring.reader_for_generation(job_b)),
            vec![b"b1".to_vec()]
        );
    }
}
