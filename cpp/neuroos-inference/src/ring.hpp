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

// L3: distinct ring names this registry will ever create. Every ring is a
// live memfd (kDefaultRingCapacitySlots * kDefaultRingSlotSize = 256 KiB by
// default) plus an fd, and nothing ever frees one -- there is no DetachRing
// RPC -- so without a ceiling any client on inference.sock could exhaust
// C4's fds and its PRD §6.2 memory budget just by attaching rings under
// fresh names. The real clients use a small fixed set of names
// (`knowledge-text` for C5a's text path, C2's own ring in Phase 6), so this
// is headroom, not a limit anyone legitimately reaches. Reclaiming a ring
// needs a DetachRing RPC, which needs an ADR (rules.md §10.2.4) -- see
// BUGS.md L3.
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
