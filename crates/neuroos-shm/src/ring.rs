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

pub const FLAG_EOS: u16 = 1 << 0;
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
        let total = HEADER_SIZE + capacity_slots as usize * slot_size as usize;
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
        let expected = HEADER_SIZE + capacity_slots as usize * slot_size as usize;
        if expected != size {
            return Err(RingError::BadHeader(
                "declared capacity does not match fd size",
            ));
        }
        if !slot_size.is_multiple_of(8) {
            return Err(RingError::BadHeader("slot_size must be a multiple of 8"));
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
            // SAFETY: `len` was just read from this same slot; it is at most
            // `slot_size - SLOT_PAYLOAD_OFF` because the writer checked that
            // bound before ever storing a `utf8_len` this large.
            let payload = unsafe { atomic_copy_from_slot(slot.add(SLOT_PAYLOAD_OFF), len) };

            let after = seqlock.load(Ordering::Acquire);
            if after != before {
                continue; // torn read; retry the same seq
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
