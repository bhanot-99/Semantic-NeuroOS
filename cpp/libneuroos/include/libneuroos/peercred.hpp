// SO_PEERCRED peer authentication (Architecture.md §5.1), C++ side. Mirrors
// crates/neuroos-ipc/src/peercred.rs.
#pragma once

#include <cstdint>
#include <vector>

#include "libneuroos/expected.hpp"
#include "libneuroos/ipc_error.hpp"

namespace neuroos::ipc {

struct PeerCred {
    std::uint32_t uid;
    std::uint32_t gid;
    std::int32_t pid;
};

Expected<PeerCred, IpcError> peer_cred(int fd);

// Pure allow/deny decision, unit-testable without a real second UID.
inline bool is_allowed(const PeerCred& cred, const std::vector<std::uint32_t>& allowed_uids) {
    for (auto uid : allowed_uids) {
        if (uid == cred.uid) {
            return true;
        }
    }
    return false;
}

} // namespace neuroos::ipc
