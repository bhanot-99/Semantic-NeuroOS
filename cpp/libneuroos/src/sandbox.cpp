#include "libneuroos/sandbox.hpp"

#include <fcntl.h>
#include <linux/landlock.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <cerrno>
#include <cstdint>
#include <cstring>

namespace neuroos::sandbox {

namespace {

// Landlock ABI v5 (Linux 6.10): every filesystem right up to IOCTL_DEV.
constexpr int kAbiVersion = 5;
constexpr std::uint64_t kAllRights = (LANDLOCK_ACCESS_FS_IOCTL_DEV << 1) - 1;
constexpr std::uint64_t kReadRights =
    LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;
// The rights that apply to a file itself (the rest only mean something on
// a directory, and the kernel rejects them on a file).
constexpr std::uint64_t kFileRights = LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_WRITE_FILE |
                                      LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_TRUNCATE |
                                      LANDLOCK_ACCESS_FS_IOCTL_DEV;

class Fd {
  public:
    explicit Fd(int fd) : fd_(fd) {}
    ~Fd() {
        if (fd_ >= 0) {
            ::close(fd_);
        }
    }
    Fd(const Fd&) = delete;
    Fd& operator=(const Fd&) = delete;
    // Scope guard for one Landlock ruleset fd: never moved, so the close in
    // the destructor always happens exactly where the fd was opened.
    Fd(Fd&&) = delete;
    Fd& operator=(Fd&&) = delete;
    int get() const {
        return fd_;
    }

  private:
    int fd_;
};

SandboxError errno_error(const std::string& what) {
    return SandboxError{what + ": " + std::strerror(errno)};
}

// Adds `path` with `rights` (narrowed to file rights for a file). A missing
// path is skipped.
Expected<void, SandboxError> add_path(int ruleset, const std::string& path, std::uint64_t rights) {
    Fd fd(::open(path.c_str(), O_PATH | O_CLOEXEC));
    if (fd.get() < 0) {
        if (errno == ENOENT) {
            return {};
        }
        return make_unexpected(errno_error("open " + path));
    }
    struct stat st {};
    if (::fstat(fd.get(), &st) != 0) {
        return make_unexpected(errno_error("fstat " + path));
    }
    if (!S_ISDIR(st.st_mode)) {
        rights &= kFileRights;
    }
    landlock_path_beneath_attr rule{};
    rule.allowed_access = rights;
    rule.parent_fd = fd.get();
    if (::syscall(SYS_landlock_add_rule, ruleset, LANDLOCK_RULE_PATH_BENEATH, &rule, 0) != 0) {
        return make_unexpected(errno_error("landlock_add_rule " + path));
    }
    return {};
}

} // namespace

Policy Policy::baseline() {
    return Policy{
        .read_only = {"/usr/lib", "/lib", "/lib64", "/etc/ld.so.cache", "/proc",
                      "/sys/devices/system/cpu", "/sys/fs/cgroup", "/etc/localtime",
                      "/usr/share/zoneinfo", "/dev/urandom"},
        .read_write = {"/dev/null"},
    };
}

Expected<void, SandboxError> restrict_self(const Policy& policy) {
    long abi = ::syscall(SYS_landlock_create_ruleset, nullptr, 0, LANDLOCK_CREATE_RULESET_VERSION);
    if (abi < kAbiVersion) {
        return make_unexpected(SandboxError{
            abi < 0 ? "Landlock is unavailable or disabled; refusing to run unsandboxed"
                    : "the kernel's Landlock ABI is older than v5; refusing to run unsandboxed"});
    }
    landlock_ruleset_attr attr{};
    attr.handled_access_fs = kAllRights;
    Fd ruleset(static_cast<int>(::syscall(SYS_landlock_create_ruleset, &attr, sizeof(attr), 0)));
    if (ruleset.get() < 0) {
        return make_unexpected(errno_error("landlock_create_ruleset"));
    }
    for (const auto& path : policy.read_only) {
        if (auto r = add_path(ruleset.get(), path, kReadRights); !r) {
            return r;
        }
    }
    for (const auto& path : policy.read_write) {
        if (auto r = add_path(ruleset.get(), path, kAllRights); !r) {
            return r;
        }
    }
    if (::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        return make_unexpected(errno_error("prctl(PR_SET_NO_NEW_PRIVS)"));
    }
    if (::syscall(SYS_landlock_restrict_self, ruleset.get(), 0) != 0) {
        return make_unexpected(errno_error("landlock_restrict_self"));
    }
    return {};
}

} // namespace neuroos::sandbox
