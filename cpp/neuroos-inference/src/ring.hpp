// Named memfd ring registry (Architecture.md §5.5, P2-S02): C2 attaches to
// a named ring (AttachRingRequest) and gets its memfd via SCM_RIGHTS; C4
// (this component) creates the ring lazily on first attach and writes
// generated tokens into it via a RingWriter.
#pragma once

#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>

#include "libneuroos/shm.hpp"

namespace neuroos::inference {

// Architecture.md §5.5: "memfd... (default 256 KiB)"; 256 B/slot gives 1024
// slots, generous for detokenized UTF-8 pieces (a handful of bytes each).
constexpr std::uint32_t kDefaultRingCapacitySlots = 1024;
constexpr std::uint32_t kDefaultRingSlotSize = 256;

class RingRegistry {
  public:
    // Creates `name`'s ring on first call (capacity_slots/slot_size 0 means
    // "use the defaults above"); returns the same writer on every later call
    // for the same name, ignoring capacity/slot_size on those later calls
    // (the ring's shape is fixed at creation).
    neuroos::shm::RingWriter get_or_create(const std::string& name, std::uint32_t capacity_slots,
                                           std::uint32_t slot_size);

    // The ring's memfd for SCM_RIGHTS handoff, or -1 if `name` was never
    // attached/created.
    int fd_for(const std::string& name) const;

    bool exists(const std::string& name) const;

  private:
    mutable std::mutex mutex_;
    std::unordered_map<std::string, std::shared_ptr<neuroos::shm::Ring>> rings_;
};

} // namespace neuroos::inference
