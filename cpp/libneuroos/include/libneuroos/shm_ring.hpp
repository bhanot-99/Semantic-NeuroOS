// memfd seqlock ring (reader + writer, C++ side) — Architecture.md §5.5.
//
// Layout must match crates/neuroos-shm/src/ring.rs exactly, including the
// `seq` field (not in the original architecture doc; added after a real bug
// was found with `tests/ring_stress.rs` during the P0-S07 spike — see that
// crate's `ring.rs` doc comments) and the choice to access every field,
// including the payload bytes, through relaxed atomics rather than plain
// reads/writes. That choice is not optional on the Rust side (confirmed by
// ThreadSanitizer) and this implementation follows the same rule so a
// stress test built the same way (`-fsanitize=thread`) is equally clean.
#pragma once

#include <atomic>
#include <cstdint>
#include <cstring>
#include <optional>
#include <string>
#include <vector>

namespace neuroos::shm {

constexpr std::uint32_t kMagic = 0x4E52'5452; // "NRTR"
constexpr std::uint16_t kVersion = 1;
constexpr std::size_t kHeaderSize = 64;
constexpr std::uint32_t kDefaultCapacitySlots = 1024;
constexpr std::uint32_t kDefaultSlotSize = 256;

constexpr std::uint16_t kFlagEos = 1u << 0;
constexpr std::uint16_t kFlagCancel = 1u << 1;

// slot byte layout (must match ring.rs's SLOT_*_OFF constants exactly):
//   [0..4)   seqlock: u32 (odd = writing, even = stable)
//   [4..8)   padding
//   [8..16)  seq: u64
//   [16..24) generation_id: u64
//   [24..28) token_id: u32
//   [28..30) flags: u16
//   [30..32) utf8_len: u16
//   [32..slot_size) utf8_bytes
constexpr std::size_t kSlotSeqlockOff = 0;
constexpr std::size_t kSlotSeqOff = 8;
constexpr std::size_t kSlotGenerationOff = 16;
constexpr std::size_t kSlotTokenIdOff = 24;
constexpr std::size_t kSlotFlagsOff = 28;
constexpr std::size_t kSlotUtf8LenOff = 30;
constexpr std::size_t kSlotPayloadOff = 32;

struct TokenPiece {
    std::uint32_t token_id;
    std::uint16_t flags;
    std::vector<std::uint8_t> payload;
};

// Maps an existing memfd (received via SCM_RIGHTS or fd inheritance) that
// was created by `Ring::create` on either side. Read-only view of the
// header's static fields; all mutable access goes through the atomic
// helpers below, matching the Rust side.
class RingView {
  public:
    RingView(void* base, std::size_t capacity_slots, std::size_t slot_size)
        : base_(static_cast<std::uint8_t*>(base)), capacity_slots_(capacity_slots),
          slot_size_(slot_size) {}

    std::uint8_t* slot_ptr(std::uint32_t idx) const {
        return base_ + kHeaderSize + static_cast<std::size_t>(idx) * slot_size_;
    }

    std::size_t max_payload() const {
        return slot_size_ - kSlotPayloadOff;
    }
    std::size_t capacity_slots() const {
        return capacity_slots_;
    }

    std::atomic<std::uint64_t>* generation_id() const {
        return reinterpret_cast<std::atomic<std::uint64_t>*>(base_ + 16);
    }
    std::atomic<std::uint64_t>* write_seq() const {
        return reinterpret_cast<std::atomic<std::uint64_t>*>(base_ + 24);
    }

    void init_header(std::uint32_t capacity_slots, std::uint32_t slot_size) {
        std::uint32_t magic = kMagic;
        std::uint16_t version = kVersion;
        std::uint16_t flags = 0;
        std::memcpy(base_ + 0, &magic, sizeof(magic));
        std::memcpy(base_ + 4, &version, sizeof(version));
        std::memcpy(base_ + 6, &flags, sizeof(flags));
        std::memcpy(base_ + 8, &capacity_slots, sizeof(capacity_slots));
        std::memcpy(base_ + 12, &slot_size, sizeof(slot_size));
        generation_id()->store(0, std::memory_order_relaxed);
        write_seq()->store(0, std::memory_order_relaxed);
    }

    static std::uint32_t read_magic(const void* base) {
        std::uint32_t magic;
        std::memcpy(&magic, static_cast<const std::uint8_t*>(base) + 0, sizeof(magic));
        return magic;
    }
    static std::uint16_t read_version(const void* base) {
        std::uint16_t v;
        std::memcpy(&v, static_cast<const std::uint8_t*>(base) + 4, sizeof(v));
        return v;
    }
    static std::uint32_t read_capacity_slots(const void* base) {
        std::uint32_t v;
        std::memcpy(&v, static_cast<const std::uint8_t*>(base) + 8, sizeof(v));
        return v;
    }
    static std::uint32_t read_slot_size(const void* base) {
        std::uint32_t v;
        std::memcpy(&v, static_cast<const std::uint8_t*>(base) + 12, sizeof(v));
        return v;
    }

  private:
    std::uint8_t* base_;
    std::size_t capacity_slots_;
    std::size_t slot_size_;
};

inline std::atomic<std::uint32_t>* seqlock_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint32_t>*>(slot + kSlotSeqlockOff);
}
inline std::atomic<std::uint64_t>* seq_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint64_t>*>(slot + kSlotSeqOff);
}
inline std::atomic<std::uint64_t>* generation_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint64_t>*>(slot + kSlotGenerationOff);
}
inline std::atomic<std::uint32_t>* token_id_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint32_t>*>(slot + kSlotTokenIdOff);
}
inline std::atomic<std::uint16_t>* flags_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint16_t>*>(slot + kSlotFlagsOff);
}
inline std::atomic<std::uint16_t>* utf8_len_atomic(std::uint8_t* slot) {
    return reinterpret_cast<std::atomic<std::uint16_t>*>(slot + kSlotUtf8LenOff);
}

inline void atomic_copy_to_slot(std::uint8_t* dst, const std::uint8_t* src, std::size_t len) {
    for (std::size_t i = 0; i < len; ++i) {
        reinterpret_cast<std::atomic<std::uint8_t>*>(dst + i)->store(src[i],
                                                                     std::memory_order_relaxed);
    }
}
inline void atomic_copy_from_slot(std::vector<std::uint8_t>& dst, const std::uint8_t* src,
                                  std::size_t len) {
    dst.resize(len);
    for (std::size_t i = 0; i < len; ++i) {
        dst[i] = reinterpret_cast<const std::atomic<std::uint8_t>*>(src + i)->load(
            std::memory_order_relaxed);
    }
}

// Single-writer handle.
class RingWriter {
  public:
    explicit RingWriter(RingView view) : view_(view) {}

    std::uint64_t generation_id() const {
        return view_.generation_id()->load(std::memory_order_acquire);
    }

    std::uint64_t cancel() {
        return view_.generation_id()->fetch_add(1, std::memory_order_acq_rel) + 1;
    }

    // BUG-004: same bump as `cancel()`, called when a *new* (non-cancelled)
    // job is assigned to this ring. A ring's name is reused across many
    // logical requests (e.g. `knowledge-text`), so without this, every job
    // on it shares the same `generation_id`; a reader that attaches after a
    // prior job already started writing (its own request raced ahead of
    // that prior job's still-running decode) can't tell those older slots
    // apart from its own and reads them as if they were its own answer,
    // finishing in milliseconds instead of waiting for its actual job to
    // run -- the client then races ahead and over-submits, which is what
    // actually fills `LaneScheduler`'s queue under "sequential" load, not a
    // slot leak in the scheduler itself. Bumping here makes every job's
    // slots distinguishable so a fresh reader correctly skips stale ones
    // (`RingReader::try_read`'s existing "stale generation" check) instead
    // of misattributing them.
    std::uint64_t next_generation() {
        return cancel();
    }

    // Returns false if `payload` exceeds the slot's max payload size. Stamps
    // the slot with the header's current generation.
    bool write(std::uint32_t token_id, std::uint16_t flags, const std::uint8_t* payload,
               std::size_t len) {
        return write_as(generation_id(), token_id, flags, payload, len);
    }

    // M12: writes `payload` across as many consecutive slots as it needs,
    // all stamped with `generation` and `token_id`; `flags` go on the last
    // slot only, so an end-of-stream marker still means "the stream ends
    // here". A piece wider than one slot's payload (`max_payload()`, 224 B
    // for the default 256 B slots) used to be dropped on the floor by the
    // single-slot `write_as`, silently losing text from the answer. A
    // reader concatenates payload bytes across slots (so does C5's
    // `read_all_tokens`), so a split piece is indistinguishable from
    // several pieces -- which is also why a multi-byte character may be
    // cut here without harm. Returns false only if a slot write itself
    // fails; an empty payload still writes exactly one slot (H8's terminal
    // marker).
    bool write_split(std::uint64_t generation, std::uint32_t token_id, std::uint16_t flags,
                     const std::uint8_t* payload, std::size_t len) {
        std::size_t max = view_.max_payload();
        if (len == 0) {
            return write_as(generation, token_id, flags, payload, 0);
        }
        std::size_t offset = 0;
        while (offset < len) {
            std::size_t take = len - offset < max ? len - offset : max;
            bool last = offset + take >= len;
            if (!write_as(generation, token_id, last ? flags : 0, payload + offset, take)) {
                return false;
            }
            offset += take;
        }
        return true;
    }

    // H7: stamps the slot with `generation` -- the job's own id, fixed when
    // the job was submitted -- rather than whatever the header holds at
    // write time. A ring name is shared by many requests, and the header
    // moves on as soon as the next one is submitted, possibly while this
    // job is still decoding.
    bool write_as(std::uint64_t generation, std::uint32_t token_id, std::uint16_t flags,
                  const std::uint8_t* payload, std::size_t len) {
        if (len > view_.max_payload()) {
            return false;
        }
        std::uint64_t seq = view_.write_seq()->load(std::memory_order_relaxed);
        std::uint32_t idx = static_cast<std::uint32_t>(seq % view_.capacity_slots());
        std::uint8_t* slot = view_.slot_ptr(idx);
        auto* seqlock = seqlock_atomic(slot);

        std::uint32_t cur = seqlock->load(std::memory_order_relaxed);
        seqlock->store(cur + 1, std::memory_order_release); // odd: writing
        // M19: the `release` above keeps *earlier* work from moving after
        // the odd marker, which is not what a seqlock writer needs; it
        // needs the field stores below to stay *after* it, so a reader
        // that has already seen an even counter cannot also see a
        // half-written field. That takes a release fence here. On x86
        // stores are not reordered with stores, so the old code was
        // correct there by accident; on a weakly-ordered CPU (ARM, which
        // Architecture.md §12 lists as a target) it is not. Must match
        // crates/neuroos-shm/src/ring.rs, since either language may be the
        // writer for a ring the other reads.
        std::atomic_thread_fence(std::memory_order_release);

        seq_atomic(slot)->store(seq, std::memory_order_relaxed);
        generation_atomic(slot)->store(generation, std::memory_order_relaxed);
        token_id_atomic(slot)->store(token_id, std::memory_order_relaxed);
        flags_atomic(slot)->store(flags, std::memory_order_relaxed);
        utf8_len_atomic(slot)->store(static_cast<std::uint16_t>(len), std::memory_order_relaxed);
        atomic_copy_to_slot(slot + kSlotPayloadOff, payload, len);

        // The `release` here is the half that works as written: it keeps
        // the field stores above from moving past the even marker.
        seqlock->store(cur + 2, std::memory_order_release); // even: stable
        view_.write_seq()->store(seq + 1, std::memory_order_release);
        return true;
    }

  private:
    RingView view_;
};

// Single-reader handle.
class RingReader {
  public:
    // With `only_generation` set, returns only that generation's slots (H7:
    // a client reading its own job's tokens, by the generation_id its
    // GenerateResponse carried); otherwise only the header's current one.
    explicit RingReader(RingView view, std::optional<std::uint64_t> only_generation = std::nullopt)
        : view_(view), only_generation_(only_generation) {}

    std::optional<TokenPiece> try_read() {
        for (;;) {
            std::uint64_t write_seq = view_.write_seq()->load(std::memory_order_acquire);
            if (next_seq_ >= write_seq) {
                return std::nullopt;
            }
            std::uint64_t capacity = static_cast<std::uint64_t>(view_.capacity_slots());
            if (write_seq - next_seq_ > capacity) {
                next_seq_ = write_seq - capacity; // lapped; resync
            }

            std::uint64_t seq = next_seq_;
            std::uint32_t idx = static_cast<std::uint32_t>(seq % capacity);
            std::uint8_t* slot = view_.slot_ptr(idx);
            auto* seqlock = seqlock_atomic(slot);

            std::uint32_t before = seqlock->load(std::memory_order_acquire);
            if (before % 2 == 1) {
                continue; // writer is mid-write to this slot
            }

            std::uint64_t slot_seq = seq_atomic(slot)->load(std::memory_order_relaxed);
            std::uint64_t generation = generation_atomic(slot)->load(std::memory_order_relaxed);
            std::uint32_t token_id = token_id_atomic(slot)->load(std::memory_order_relaxed);
            std::uint16_t flags = flags_atomic(slot)->load(std::memory_order_relaxed);
            std::size_t len = utf8_len_atomic(slot)->load(std::memory_order_relaxed);
            // M1: `len` lives in shared memory, so it is untrusted even
            // though our own writer never stores more than `max_payload`
            // -- a torn read, a corrupted mapping or a hostile peer can
            // all produce a larger value, and copying it would read past
            // the slot (past the whole mapping, for the last slot). Clamp
            // the copy, then decide below -- once the seqlock says the
            // read was stable -- whether this was a tear to retry or a
            // genuinely corrupt slot to skip.
            std::size_t max = view_.max_payload();
            std::vector<std::uint8_t> payload;
            atomic_copy_from_slot(payload, slot + kSlotPayloadOff, len < max ? len : max);

            // M19: the mirror of the writer's missing fence. `acquire` on
            // the load below orders *later* reads after it; a seqlock
            // reader needs the field reads above to stay *before* it, or
            // the stability check is made against values read after it.
            std::atomic_thread_fence(std::memory_order_acquire);
            std::uint32_t after = seqlock->load(std::memory_order_relaxed);
            if (after != before) {
                continue; // torn read; retry the same seq
            }
            if (slot_seq != seq) {
                continue; // lapped mid-read; top-of-loop resync handles it next iteration
            }
            if (len > max) {
                // A stable read of an impossible length: the slot is
                // corrupt, so skip it rather than hand its bytes up or
                // spin on it forever.
                next_seq_ = seq + 1;
                continue;
            }

            next_seq_ = seq + 1;
            std::uint64_t wanted = only_generation_.has_value()
                                       ? *only_generation_
                                       : view_.generation_id()->load(std::memory_order_acquire);
            if (generation != wanted) {
                continue; // another job's slot, or a stale (cancelled) one; skip it
            }
            return TokenPiece{token_id, flags, std::move(payload)};
        }
    }

  private:
    RingView view_;
    std::optional<std::uint64_t> only_generation_;
    std::uint64_t next_seq_ = 0;
};

} // namespace neuroos::shm
