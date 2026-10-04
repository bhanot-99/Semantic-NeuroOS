// memfd creation + mmap + the owning `Ring` wrapper (C++ side of P0-S07).
#pragma once

#include <cstdint>
#include <stdexcept>
#include <string>

#include "libneuroos/shm_ring.hpp"

namespace neuroos::shm {

class RingError : public std::runtime_error {
  public:
    using std::runtime_error::runtime_error;
};

// Owns a memfd and its shared mapping. Move-only.
class Ring {
  public:
    static Ring create(const std::string& name, std::uint32_t capacity_slots,
                       std::uint32_t slot_size);
    // Takes ownership of `fd` (e.g. received via SCM_RIGHTS, or inherited
    // across fork/exec); reads capacity/slot_size back from the header.
    static Ring open(int fd);

    ~Ring();
    Ring(Ring&& other) noexcept;
    Ring& operator=(Ring&& other) noexcept;
    Ring(const Ring&) = delete;
    Ring& operator=(const Ring&) = delete;

    int fd() const {
        return fd_;
    }
    RingWriter writer() const {
        return RingWriter(view_);
    }
    RingReader reader() const {
        return RingReader(view_);
    }
    RingReader reader_for_generation(std::uint64_t generation) const {
        return RingReader(view_, generation);
    }

  private:
    Ring(int fd, void* base, std::size_t len, std::uint32_t capacity_slots,
         std::uint32_t slot_size);

    int fd_ = -1;
    void* base_ = nullptr;
    std::size_t len_ = 0;
    RingView view_;
};

} // namespace neuroos::shm
