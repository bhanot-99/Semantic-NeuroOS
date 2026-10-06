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

// M1: the layout checks `Ring::create`'s arguments and an opened ring's
// (peer-written, untrusted) header both go through. Must stay in step with
// `validate_layout` in crates/neuroos-shm/src/ring.rs: either side may
// create a ring the other opens. Returns the total mapping size.
std::size_t validate_layout(std::uint16_t version, std::uint32_t capacity_slots,
                            std::uint32_t slot_size) {
    if (version != kVersion) {
        throw RingError("unsupported ring version");
    }
    if (capacity_slots == 0) {
        // `seq % capacity_slots` in both `RingWriter::write_as` and
        // `RingReader::try_read` would divide by zero.
        throw RingError("capacity_slots must be at least 1");
    }
    if (static_cast<std::size_t>(slot_size) <= kSlotPayloadOff) {
        throw RingError("slot_size too small to hold slot metadata");
    }
    if (slot_size % 8 != 0) {
        throw RingError("slot_size must be a multiple of 8");
    }
    return kHeaderSize + static_cast<std::size_t>(capacity_slots) * slot_size;
}

std::size_t fd_size(int fd) {
    struct stat st {};
    if (::fstat(fd, &st) != 0) {
        throw RingError(std::string("fstat failed: ") + std::strerror(errno));
    }
    return static_cast<std::size_t>(st.st_size);
}

// L4: `Ring::create` and `Ring::open` both own the fd they are given or
// make, and both call helpers that throw (`map_shared`, `fd_size`,
// `validate_layout`). The hand-written `::close(fd)` on each throwing path
// missed exactly the ones that go through `throw` inside a helper rather
// than a local `if`, so an mmap failure in `Ring::open` leaked the ring's
// memfd. Ownership now rides on a scope guard, and is released only once
// the Ring itself holds it.
class OwnedFd {
  public:
    explicit OwnedFd(int fd) : fd_(fd) {}
    ~OwnedFd() {
        if (fd_ >= 0) {
            ::close(fd_);
        }
    }
    OwnedFd(const OwnedFd&) = delete;
    OwnedFd& operator=(const OwnedFd&) = delete;
    OwnedFd(OwnedFd&&) = delete;
    OwnedFd& operator=(OwnedFd&&) = delete;

    int get() const {
        return fd_;
    }
    int release() {
        int fd = fd_;
        fd_ = -1;
        return fd;
    }

  private:
    int fd_ = -1;
};

// L4: same for the mapping -- `Ring::open`'s validation throws after the
// mmap has succeeded, and the `catch (...)` that undid it had to be kept
// in step by hand.
class OwnedMapping {
  public:
    OwnedMapping(void* base, std::size_t len) : base_(base), len_(len) {}
    ~OwnedMapping() {
        if (base_ != nullptr) {
            ::munmap(base_, len_);
        }
    }
    OwnedMapping(const OwnedMapping&) = delete;
    OwnedMapping& operator=(const OwnedMapping&) = delete;
    OwnedMapping(OwnedMapping&&) = delete;
    OwnedMapping& operator=(OwnedMapping&&) = delete;

    void* get() const {
        return base_;
    }
    void* release() {
        void* base = base_;
        base_ = nullptr;
        return base;
    }

  private:
    void* base_ = nullptr;
    std::size_t len_ = 0;
};

} // namespace

Ring::Ring(int fd, void* base, std::size_t len, std::uint32_t capacity_slots,
           std::uint32_t slot_size)
    : fd_(fd), base_(base), len_(len), view_(RingView(base, capacity_slots, slot_size)) {}

Ring Ring::create(const std::string& name, std::uint32_t capacity_slots, std::uint32_t slot_size) {
    std::size_t total = validate_layout(kVersion, capacity_slots, slot_size);
    // L4: closes the memfd if ftruncate or mmap throws below.
    OwnedFd fd(create_memfd_checked(name));
    if (::ftruncate(fd.get(), static_cast<off_t>(total)) != 0) {
        throw RingError(std::string("ftruncate failed: ") + std::strerror(errno));
    }
    OwnedMapping base(map_shared(fd.get(), total), total);
    Ring ring(fd.release(), base.release(), total, capacity_slots, slot_size);
    ring.view_.init_header(capacity_slots, slot_size);
    return ring;
}

Ring Ring::open(int fd_raw) {
    // L4: from here on the fd is owned, so every throw below -- including
    // the ones raised inside `fd_size`, `map_shared` and `validate_layout`
    // -- closes it.
    OwnedFd fd(fd_raw);
    std::size_t size = fd_size(fd.get());
    if (size < kHeaderSize) {
        throw RingError("fd too small to contain a header");
    }
    OwnedMapping base(map_shared(fd.get(), size), size);
    if (RingView::read_magic(base.get()) != kMagic) {
        throw RingError("magic mismatch");
    }
    std::uint32_t capacity_slots = RingView::read_capacity_slots(base.get());
    std::uint32_t slot_size = RingView::read_slot_size(base.get());
    if (validate_layout(RingView::read_version(base.get()), capacity_slots, slot_size) != size) {
        throw RingError("declared capacity does not match fd size");
    }
    return Ring(fd.release(), base.release(), size, capacity_slots, slot_size);
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
