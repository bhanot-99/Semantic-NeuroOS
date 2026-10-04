// Landlock filesystem sandbox, C++ side (Architecture.md AP-1, §8.2;
// BUGS.md H15). Same semantics as crates/neuroos-sandbox: a service locks
// itself in at startup, before touching user data or spawning threads
// (Landlock restricts the calling thread and everything it creates
// afterwards). Fail closed: restrict_self errors unless the kernel enforces
// the whole policy (ABI v5+). A path that doesn't exist is skipped -- that
// only narrows access.
#pragma once

#include <string>
#include <vector>

#include "libneuroos/expected.hpp"

namespace neuroos::sandbox {

struct SandboxError {
    std::string message;
};

struct Policy {
    std::vector<std::string> read_only;
    std::vector<std::string> read_write; // a file entry grants just that file

    // Shared libraries, /proc, CPU topology and cgroup limits, time zone
    // data, /dev/null and /dev/urandom -- what every service needs.
    static Policy baseline();
};

Expected<void, SandboxError> restrict_self(const Policy& policy);

} // namespace neuroos::sandbox
