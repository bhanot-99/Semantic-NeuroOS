// H15: libneuroos's C++ Landlock sandbox (cpp/libneuroos/src/sandbox.cpp)
// really denies what its policy doesn't grant, on the running kernel.
// Landlock restricts the calling thread only, so each check runs its
// sandboxed half on a fresh std::thread and the process itself stays free.
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <thread>

#include <fcntl.h>
#include <unistd.h>

#include "libneuroos/sandbox.hpp"

namespace {

int failures = 0;

void check(bool ok, const char* what) {
    std::printf("%s: %s\n", ok ? "OK" : "FAIL", what);
    if (!ok) {
        ++failures;
    }
}

std::string make_dir(const char* tag) {
    std::string tmpl = std::string("/tmp/neuroos-sandbox-") + tag + "-XXXXXX";
    char* dir = ::mkdtemp(tmpl.data());
    return dir == nullptr ? std::string() : std::string(dir);
}

void write_file(const std::string& path, const char* text) {
    std::ofstream(path) << text;
}

// errno of open(path, flags), or 0 on success.
int open_errno(const std::string& path, int flags) {
    int fd = ::open(path.c_str(), flags | O_CLOEXEC, 0600);
    if (fd < 0) {
        return errno;
    }
    ::close(fd);
    return 0;
}

} // namespace

int main() {
    std::string allowed = make_dir("ro");
    std::string writable = make_dir("rw");
    std::string outside = make_dir("outside");
    write_file(allowed + "/a", "ok");
    write_file(outside + "/secret", "no");

    neuroos::sandbox::Policy policy = neuroos::sandbox::Policy::baseline();
    policy.read_only.push_back(allowed);
    policy.read_write.push_back(writable);
    policy.read_only.push_back("/nonexistent/neuroos/models"); // skipped, not fatal

    bool restricted = false;
    int read_allowed = -1, read_outside = -1, write_ro = -1, write_rw = -1;
    std::thread([&] {
        restricted = static_cast<bool>(neuroos::sandbox::restrict_self(policy));
        read_allowed = open_errno(allowed + "/a", O_RDONLY);
        read_outside = open_errno(outside + "/secret", O_RDONLY);
        write_ro = open_errno(allowed + "/new", O_WRONLY | O_CREAT);
        write_rw = open_errno(writable + "/new", O_WRONLY | O_CREAT);
    }).join();

    check(restricted, "restrict_self succeeds (ABI v5+)");
    check(read_allowed == 0, "a read-only path is readable");
    check(read_outside == EACCES, "a path outside the policy is denied");
    check(write_ro == EACCES, "a read-only path cannot be written");
    check(write_rw == 0, "a read-write path can be written");
    check(open_errno(outside + "/secret", O_RDONLY) == 0,
          "the unsandboxed main thread is unaffected");

    std::string cleanup = "rm -rf '" + allowed + "' '" + writable + "' '" + outside + "'";
    if (std::system(cleanup.c_str()) != 0) {
        std::printf("warning: could not clean up temp dirs\n");
    }
    if (failures == 0) {
        std::printf("all cpp_sandbox_smoke checks passed\n");
    }
    return failures == 0 ? 0 : 1;
}
