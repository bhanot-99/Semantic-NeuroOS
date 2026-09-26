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

    // Returns false if `payload` exceeds the slot's max payload size.
    bool write(std::uint32_t token_id, std::uint16_t flags, const std::uint8_t* payload,
               std::size_t len) {
        if (len > view_.max_payload()) {
            return false;
        }
        std::uint64_t generation = generation_id();
        std::uint64_t seq = view_.write_seq()->load(std::memory_order_relaxed);
        std::uint32_t idx = static_cast<std::uint32_t>(seq % view_.capacity_slots());
        std::uint8_t* slot = view_.slot_ptr(idx);
        auto* seqlock = seqlock_atomic(slot);

        std::uint32_t cur = seqlock->load(std::memory_order_relaxed);
        seqlock->store(cur + 1, std::memory_order_release); // odd: writing

        seq_atomic(slot)->store(seq, std::memory_order_relaxed);
        generation_atomic(slot)->store(generation, std::memory_order_relaxed);
        token_id_atomic(slot)->store(token_id, std::memory_order_relaxed);
        flags_atomic(slot)->store(flags, std::memory_order_relaxed);
        utf8_len_atomic(slot)->store(static_cast<std::uint16_t>(len), std::memory_order_relaxed);
        atomic_copy_to_slot(slot + kSlotPayloadOff, payload, len);

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
    explicit RingReader(RingView view) : view_(view) {}

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
            std::vector<std::uint8_t> payload;
            atomic_copy_from_slot(payload, slot + kSlotPayloadOff, len);

            std::uint32_t after = seqlock->load(std::memory_order_acquire);
            if (after != before) {
                continue; // torn read; retry the same seq
            }
            if (slot_seq != seq) {
                continue; // lapped mid-read; top-of-loop resync handles it next iteration
            }

            next_seq_ = seq + 1;
            std::uint64_t cur_gen = view_.generation_id()->load(std::memory_order_acquire);
            if (generation != cur_gen) {
                continue; // stale generation (a cancel happened); skip it
            }
            return TokenPiece{token_id, flags, std::move(payload)};
        }
    }

  private:
    RingView view_;
    std::uint64_t next_seq_ = 0;
};

} // namespace neuroos::shm
