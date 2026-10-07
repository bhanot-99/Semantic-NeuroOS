// Named memfd ring registry (Architecture.md §5.5, P2-S02): C2 attaches to
// a named ring (AttachRingRequest) and gets its memfd via SCM_RIGHTS; C4
// (this component) creates the ring lazily on first attach and writes
// generated tokens into it via a RingWriter.
#pragma once

#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>

#include "libneuroos/shm.hpp"

namespace neuroos::inference {

// Architecture.md §5.5: "memfd... (default 256 KiB)"; 256 B/slot gives 1024
// slots, generous for detokenized UTF-8 pieces (a handful of bytes each).
constexpr std::uint32_t kDefaultRingCapacitySlots = 1024;
constexpr std::uint32_t kDefaultRingSlotSize = 256;

// L3: how many rings this registry holds at once. Every ring is a live
// memfd (kDefaultRingCapacitySlots * kDefaultRingSlotSize = 256 KiB by
// default) plus an fd, so without a ceiling any client on inference.sock
// could exhaust C4's fds and its PRD §6.2 memory budget just by attaching
// rings under fresh names.
//
// ADR-0013 made this a concurrency limit rather than a lifetime one:
// `detach` frees a ring, so a long-lived C4 can serve any number of
// distinct ring names over its life as long as no more than kMaxRings are
// attached at the same time. The real clients use a small fixed set of
// names (`knowledge-text` for C5a's text path, C2's own ring in Phase 6).
constexpr std::size_t kMaxRings = 16;

// L3: ceilings on the shape a client may ask for, since capacity_slots and
// slot_size come straight off the wire and multiply into the memfd's size.
// 16 MiB is 64x the default ring and far past anything a token stream needs.
constexpr std::uint32_t kMaxRingCapacitySlots = 65536;
constexpr std::uint32_t kMaxRingSlotSize = 64 * 1024;
constexpr std::uint64_t kMaxRingBytes = 16ULL * 1024 * 1024;

class RingRegistry {
  public:
    // Creates `name`'s ring on first call (capacity_slots/slot_size 0 means
    // "use the defaults above"); returns the same writer on every later call
    // for the same name, ignoring capacity/slot_size on those later calls
    // (the ring's shape is fixed at creation).
    //
    // L3: returns `nullopt` instead of creating when the registry is
    // already holding kMaxRings rings, or when the requested shape exceeds
    // the ceilings above. An existing name always succeeds.
    std::optional<neuroos::shm::RingWriter>
    get_or_create(const std::string& name, std::uint32_t capacity_slots, std::uint32_t slot_size);

    // A writer for an *existing* ring, or `nullopt` if `name` is not
    // attached.
    //
    // ADR-0013: the write paths (the lane workers and `write_terminal`)
    // must use this rather than `get_or_create`. They only ever run for a
    // job whose ring was attached first, so a miss means the client
    // detached it, and silently creating a replacement would stream tokens
    // into a fresh memfd nobody is mapped to -- the client would wait out
    // its whole deadline. With `get` they take the existing
    // "no ring to write into" path instead.
    std::optional<neuroos::shm::RingWriter> get(const std::string& name) const;

    // Drops `name`'s ring, closing its memfd once C4's last reference goes
    // (ADR-0013). Returns false if `name` was not attached, which is not an
    // error: DetachRing is idempotent.
    //
    // Safe to call with a generation still queued or running on that ring:
    // that job's next `get` misses and it is dropped with a log. The client
    // that detached the ring is the one that stopped reading it.
    bool detach(const std::string& name);

    // The ring's memfd for SCM_RIGHTS handoff, or -1 if `name` was never
    // attached/created.
    int fd_for(const std::string& name) const;

    bool exists(const std::string& name) const;

    std::size_t size() const;

  private:
    mutable std::mutex mutex_;
    std::unordered_map<std::string, std::shared_ptr<neuroos::shm::Ring>> rings_;
};

} // namespace neuroos::inference
