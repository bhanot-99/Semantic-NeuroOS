#include "libneuroos/uds.hpp"

#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstring>
#include <filesystem>
#include <thread>

namespace neuroos::ipc {

namespace {

Expected<void, IpcError> make_parent_dirs(const std::string& path) {
    std::filesystem::path p(path);
    if (p.has_parent_path()) {
        std::error_code ec;
        std::filesystem::create_directories(p.parent_path(), ec);
        if (ec) {
            return make_unexpected(IpcError::io("create parent dirs: " + ec.message()));
        }
    }
    return {};
}

Expected<sockaddr_un, IpcError> make_sockaddr(const std::string& path) {
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    if (path.size() >= sizeof(addr.sun_path)) {
        return make_unexpected(IpcError::io("socket path too long for sockaddr_un: " + path));
    }
    std::memcpy(addr.sun_path, path.data(), path.size());
    addr.sun_path[path.size()] = '\0';
    return addr;
}

} // namespace

Expected<UdsServer, IpcError> UdsServer::bind(const std::string& path,
                                              std::vector<std::uint32_t> allowed_uids) {
    if (auto r = make_parent_dirs(path); !r) {
        return make_unexpected(r.error());
    }
    // Replace a stale socket file left by a crashed previous instance
    // (Architecture.md §5.1), matching the Rust side.
    if (::unlink(path.c_str()) != 0 && errno != ENOENT) {
        return make_unexpected(IpcError::io(std::strerror(errno)));
    }

    auto addr = make_sockaddr(path);
    if (!addr) {
        return make_unexpected(addr.error());
    }

    int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        return make_unexpected(IpcError::io(std::strerror(errno)));
    }
    if (::bind(fd, reinterpret_cast<sockaddr*>(&addr.value()), sizeof(sockaddr_un)) != 0) {
        int saved_errno = errno;
        ::close(fd);
        return make_unexpected(IpcError::io(std::strerror(saved_errno)));
    }
    if (::listen(fd, /*backlog=*/16) != 0) {
        int saved_errno = errno;
        ::close(fd);
        return make_unexpected(IpcError::io(std::strerror(saved_errno)));
    }
    return UdsServer(fd, std::move(allowed_uids));
}

UdsServer::~UdsServer() {
    if (listen_fd_ >= 0) {
        ::close(listen_fd_);
    }
}

UdsServer::UdsServer(UdsServer&& other) noexcept
    : listen_fd_(other.listen_fd_), allowed_uids_(std::move(other.allowed_uids_)) {
    other.listen_fd_ = -1;
}

UdsServer& UdsServer::operator=(UdsServer&& other) noexcept {
    if (this != &other) {
        if (listen_fd_ >= 0) {
            ::close(listen_fd_);
        }
        listen_fd_ = other.listen_fd_;
        allowed_uids_ = std::move(other.allowed_uids_);
        other.listen_fd_ = -1;
    }
    return *this;
}

bool accept_errno_is_resource_exhaustion(int err) {
    return err == EMFILE || err == ENFILE || err == ENOBUFS || err == ENOMEM;
}

bool accept_errno_is_per_connection(int err) {
    return err == ECONNABORTED || err == EINTR || err == EAGAIN || err == EWOULDBLOCK ||
           err == EPERM || err == ECONNRESET || err == ETIMEDOUT ||
           accept_errno_is_resource_exhaustion(err);
}

Expected<std::optional<std::pair<int, PeerCred>>, IpcError> UdsServer::accept() {
    int client_fd = ::accept4(listen_fd_, nullptr, nullptr, SOCK_CLOEXEC);
    if (client_fd < 0) {
        int saved_errno = errno;
        if (!accept_errno_is_per_connection(saved_errno)) {
            return make_unexpected(IpcError::io(std::strerror(saved_errno)));
        }
        if (accept_errno_is_resource_exhaustion(saved_errno)) {
            // Retrying immediately would spin at full CPU for as long as
            // the shortage lasts, which is the whole point of not treating
            // it as fatal.
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
        }
        return std::optional<std::pair<int, PeerCred>>{std::nullopt};
    }
    // A failed SO_PEERCRED lookup says nothing about the listening socket,
    // so it costs this connection and nothing more (M2).
    auto cred = peer_cred(client_fd);
    if (!cred) {
        ::close(client_fd);
        return std::optional<std::pair<int, PeerCred>>{std::nullopt};
    }
    if (!is_allowed(cred.value(), allowed_uids_)) {
        ::close(client_fd);
        return std::optional<std::pair<int, PeerCred>>{std::nullopt};
    }
    return std::optional<std::pair<int, PeerCred>>{std::make_pair(client_fd, cred.value())};
}

Expected<int, IpcError> connect(const std::string& path, std::chrono::milliseconds timeout) {
    auto addr = make_sockaddr(path);
    if (!addr) {
        return make_unexpected(addr.error());
    }

    int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0) {
        return make_unexpected(IpcError::io(std::strerror(errno)));
    }

    int rc = ::connect(fd, reinterpret_cast<sockaddr*>(&addr.value()), sizeof(sockaddr_un));
    if (rc != 0 && errno != EINPROGRESS) {
        int saved_errno = errno;
        ::close(fd);
        return make_unexpected(IpcError::io(std::strerror(saved_errno)));
    }
    if (rc != 0) {
        pollfd pfd{.fd = fd, .events = POLLOUT, .revents = 0};
        int poll_rc = ::poll(&pfd, 1, static_cast<int>(timeout.count()));
        if (poll_rc == 0) {
            ::close(fd);
            return make_unexpected(IpcError::io("connect deadline exceeded"));
        }
        if (poll_rc < 0) {
            int saved_errno = errno;
            ::close(fd);
            return make_unexpected(IpcError::io(std::strerror(saved_errno)));
        }
        int so_error = 0;
        socklen_t len = sizeof(so_error);
        if (::getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &len) != 0 || so_error != 0) {
            ::close(fd);
            return make_unexpected(
                IpcError::io(so_error != 0 ? std::strerror(so_error) : std::strerror(errno)));
        }
    }

    // Switch back to blocking: the rest of this codebase's framing I/O
    // assumes blocking semantics (one thread per connection).
    int flags = ::fcntl(fd, F_GETFL, 0);
    if (flags >= 0) {
        ::fcntl(fd, F_SETFL, flags & ~O_NONBLOCK);
    }
    return fd;
}

} // namespace neuroos::ipc
