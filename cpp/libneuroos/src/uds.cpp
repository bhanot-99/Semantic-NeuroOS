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

// L16: 0700, not whatever the service's umask happens to be. Mirrors
// `create_dir_all_private` in crates/neuroos-ipc/src/server.rs: an
// existing directory is left alone, since the runtime dir is normally
// $XDG_RUNTIME_DIR, which systemd already owns and created 0700.
Expected<void, IpcError> make_parent_dirs(const std::string& path) {
    std::filesystem::path p(path);
    if (!p.has_parent_path()) {
        return {};
    }
    std::error_code ec;
    if (std::filesystem::is_directory(p.parent_path(), ec)) {
        return {};
    }
    std::filesystem::create_directories(p.parent_path(), ec);
    if (ec) {
        return make_unexpected(IpcError::io("create parent dirs: " + ec.message()));
    }
    // std::filesystem has no mode argument, so the umask is undone after
    // the fact. The window is narrow and the directory is empty in it.
    if (::chmod(p.parent_path().c_str(), S_IRWXU) != 0) {
        return make_unexpected(IpcError::io(std::strerror(errno)));
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

void ConnectionLimiter::acquire() {
    std::unique_lock<std::mutex> lock(mutex_);
    free_slot_.wait(lock, [this] { return in_use_ < max_; });
    ++in_use_;
}

void ConnectionLimiter::release() {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (in_use_ > 0) {
            --in_use_;
        }
    }
    free_slot_.notify_one();
}

std::size_t ConnectionLimiter::in_use() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return in_use_;
}

void Connection::reset() {
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
    if (limiter_) {
        limiter_->release();
        limiter_.reset();
    }
}

Connection::~Connection() {
    reset();
}

Connection::Connection(Connection&& other) noexcept
    : fd_(other.fd_), limiter_(std::move(other.limiter_)) {
    other.fd_ = -1;
}

Connection& Connection::operator=(Connection&& other) noexcept {
    if (this != &other) {
        reset();
        fd_ = other.fd_;
        limiter_ = std::move(other.limiter_);
        other.fd_ = -1;
    }
    return *this;
}

Expected<UdsServer, IpcError> UdsServer::bind(const std::string& path,
                                              std::vector<std::uint32_t> allowed_uids,
                                              std::size_t max_connections,
                                              std::chrono::seconds idle_timeout) {
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
    // L16: 0600 on the socket file, after bind(2) created it — it does not
    // exist before then, and a umask-dependent window is what this closes.
    if (::chmod(path.c_str(), S_IRUSR | S_IWUSR) != 0) {
        int saved_errno = errno;
        ::close(fd);
        return make_unexpected(IpcError::io(std::strerror(saved_errno)));
    }
    return UdsServer(fd, std::move(allowed_uids), max_connections, idle_timeout);
}

UdsServer::~UdsServer() {
    if (listen_fd_ >= 0) {
        ::close(listen_fd_);
    }
}

UdsServer::UdsServer(UdsServer&& other) noexcept
    : listen_fd_(other.listen_fd_), allowed_uids_(std::move(other.allowed_uids_)),
      limiter_(std::move(other.limiter_)), idle_timeout_(other.idle_timeout_) {
    other.listen_fd_ = -1;
}

UdsServer& UdsServer::operator=(UdsServer&& other) noexcept {
    if (this != &other) {
        if (listen_fd_ >= 0) {
            ::close(listen_fd_);
        }
        listen_fd_ = other.listen_fd_;
        allowed_uids_ = std::move(other.allowed_uids_);
        limiter_ = std::move(other.limiter_);
        idle_timeout_ = other.idle_timeout_;
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

Expected<std::optional<std::pair<Connection, PeerCred>>, IpcError> UdsServer::accept() {
    using Accepted = std::optional<std::pair<Connection, PeerCred>>;
    // L2: take a slot *before* accepting, so surplus peers wait in the
    // kernel's listen backlog rather than each costing a thread and an fd.
    limiter_->acquire();
    int client_fd = ::accept4(listen_fd_, nullptr, nullptr, SOCK_CLOEXEC);
    if (client_fd < 0) {
        int saved_errno = errno;
        limiter_->release();
        if (!accept_errno_is_per_connection(saved_errno)) {
            return make_unexpected(IpcError::io(std::strerror(saved_errno)));
        }
        if (accept_errno_is_resource_exhaustion(saved_errno)) {
            // Retrying immediately would spin at full CPU for as long as
            // the shortage lasts, which is the whole point of not treating
            // it as fatal.
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
        }
        return Accepted{std::nullopt};
    }
    // From here on the fd and the slot are owned together, so every exit
    // path below frees both (L2).
    Connection conn(client_fd, limiter_);

    // L2: a blocking read on this fd gives up after `idle_timeout_` with
    // EAGAIN, which `read_envelope` reports as a read error and every
    // serving loop treats as "connection over". Without it a peer that
    // connects and says nothing pins a detached thread for the lifetime
    // of the process. A failure here is not worth dropping the
    // connection over -- it just leaves this one unbounded, as before.
    timeval timeout{};
    timeout.tv_sec = static_cast<time_t>(idle_timeout_.count());
    timeout.tv_usec = 0;
    ::setsockopt(conn.fd(), SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));

    // A failed SO_PEERCRED lookup says nothing about the listening socket,
    // so it costs this connection and nothing more (M2).
    auto cred = peer_cred(conn.fd());
    if (!cred) {
        return Accepted{std::nullopt};
    }
    if (!is_allowed(cred.value(), allowed_uids_)) {
        return Accepted{std::nullopt};
    }
    return Accepted{std::make_pair(std::move(conn), cred.value())};
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
