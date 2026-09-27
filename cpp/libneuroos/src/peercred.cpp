#include "libneuroos/peercred.hpp"

#include <sys/socket.h>

#include <cerrno>
#include <cstring>

namespace neuroos::ipc {

Expected<PeerCred, IpcError> peer_cred(int fd) {
    struct ucred cred {};
    socklen_t len = sizeof(cred);
    if (::getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &len) != 0) {
        return make_unexpected(IpcError::io(std::strerror(errno)));
    }
    return PeerCred{static_cast<std::uint32_t>(cred.uid), static_cast<std::uint32_t>(cred.gid),
                    static_cast<std::int32_t>(cred.pid)};
}

} // namespace neuroos::ipc
