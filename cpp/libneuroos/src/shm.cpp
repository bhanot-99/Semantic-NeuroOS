#include "libneuroos/shm.hpp"

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>

namespace neuroos::shm {

namespace {

int create_memfd_checked(const std::string& name) {
    int fd = static_cast<int>(::syscall(SYS_memfd_create, name.c_str(), MFD_CLOEXEC));
    if (fd < 0) {
        throw RingError(std::string("memfd_create failed: ") + std::strerror(errno));
    }
    return fd;
}

void* map_shared(int fd, std::size_t len) {
    void* p = ::mmap(nullptr, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) {
        throw RingError(std::string("mmap failed: ") + std::strerror(errno));
    }
    return p;
}

std::size_t fd_size(int fd) {
    struct stat st {};
    if (::fstat(fd, &st) != 0) {
        throw RingError(std::string("fstat failed: ") + std::strerror(errno));
    }
    return static_cast<std::size_t>(st.st_size);
}

} // namespace

Ring::Ring(int fd, void* base, std::size_t len, std::uint32_t capacity_slots,
           std::uint32_t slot_size)
    : fd_(fd), base_(base), len_(len), view_(RingView(base, capacity_slots, slot_size)) {}

Ring Ring::create(const std::string& name, std::uint32_t capacity_slots, std::uint32_t slot_size) {
    if (static_cast<std::size_t>(slot_size) <= kSlotPayloadOff) {
        throw RingError("slot_size too small to hold slot metadata");
    }
    if (slot_size % 8 != 0) {
        throw RingError("slot_size must be a multiple of 8");
    }
    std::size_t total = kHeaderSize + static_cast<std::size_t>(capacity_slots) * slot_size;
    int fd = create_memfd_checked(name);
    if (::ftruncate(fd, static_cast<off_t>(total)) != 0) {
        int err = errno;
        ::close(fd);
        throw RingError(std::string("ftruncate failed: ") + std::strerror(err));
    }
    void* base = map_shared(fd, total);
    Ring ring(fd, base, total, capacity_slots, slot_size);
    ring.view_.init_header(capacity_slots, slot_size);
    return ring;
}

Ring Ring::open(int fd) {
    std::size_t size = fd_size(fd);
    if (size < kHeaderSize) {
        ::close(fd);
        throw RingError("fd too small to contain a header");
    }
    void* base = map_shared(fd, size);
    if (RingView::read_magic(base) != kMagic) {
        ::munmap(base, size);
        ::close(fd);
        throw RingError("magic mismatch");
    }
    std::uint32_t capacity_slots = RingView::read_capacity_slots(base);
    std::uint32_t slot_size = RingView::read_slot_size(base);
    std::size_t expected = kHeaderSize + static_cast<std::size_t>(capacity_slots) * slot_size;
    if (expected != size) {
        ::munmap(base, size);
        ::close(fd);
        throw RingError("declared capacity does not match fd size");
    }
    if (slot_size % 8 != 0) {
        ::munmap(base, size);
        ::close(fd);
        throw RingError("slot_size must be a multiple of 8");
    }
    return Ring(fd, base, size, capacity_slots, slot_size);
}

Ring::~Ring() {
    if (base_ != nullptr) {
        ::munmap(base_, len_);
    }
    if (fd_ >= 0) {
        ::close(fd_);
    }
}

Ring::Ring(Ring&& other) noexcept
    : fd_(other.fd_), base_(other.base_), len_(other.len_), view_(other.view_) {
    other.fd_ = -1;
    other.base_ = nullptr;
    other.len_ = 0;
}

Ring& Ring::operator=(Ring&& other) noexcept {
    if (this != &other) {
        if (base_ != nullptr) {
            ::munmap(base_, len_);
        }
        if (fd_ >= 0) {
            ::close(fd_);
        }
        fd_ = other.fd_;
        base_ = other.base_;
        len_ = other.len_;
        view_ = other.view_;
        other.fd_ = -1;
        other.base_ = nullptr;
        other.len_ = 0;
    }
    return *this;
}

} // namespace neuroos::shm
