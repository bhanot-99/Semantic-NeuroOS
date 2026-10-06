// UDS server/client (Architecture.md §5.1), C++ side. Mirrors
// crates/neuroos-ipc/src/{server,client}.rs: filesystem-path sockets only,
// SO_PEERCRED-checked on accept, blocking (one std::jthread per connection
// is the intended usage, not async I/O — rules.md §"C++ threads").
#pragma once

#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "libneuroos/expected.hpp"
#include "libneuroos/ipc_error.hpp"
#include "libneuroos/peercred.hpp"

namespace neuroos::ipc {

// L2: concurrent connections one socket will hold open, and how long an
// accepted connection may sit without sending its next request before the
// server closes it. Mirrors DEFAULT_MAX_CONNECTIONS / DEFAULT_IDLE_TIMEOUT
// in crates/neuroos-ipc/src/server.rs; see those for the reasoning. Both
// C++ servers (inference.sock and every component's C++ health socket)
// spawn a detached std::thread per connection, so without a ceiling a peer
// in the UID allowlist could spawn threads until the process died.
constexpr std::size_t kDefaultMaxConnections = 64;
constexpr std::chrono::seconds kDefaultIdleTimeout{60};

// L2: a counting semaphore built on a mutex + condvar rather than
// std::counting_semaphore, because the ceiling has to be observable
// (`in_use`) for the smoke test to prove it holds.
class ConnectionLimiter {
  public:
    explicit ConnectionLimiter(std::size_t max_connections) : max_(max_connections) {}

    // Blocks until a slot is free, then takes it.
    void acquire();
    void release();
    std::size_t in_use() const;

  private:
    mutable std::mutex mutex_;
    std::condition_variable free_slot_;
    std::size_t max_ = kDefaultMaxConnections;
    std::size_t in_use_ = 0;
};

// L2: RAII owner of an accepted connection — its fd and its slot against
// the server's max_connections ceiling. Closing the fd and freeing the
// slot happen together, in the destructor, so a serving thread cannot
// leak one without the other. Move-only; move it into the thread that
// serves the connection and let it die with that thread.
class Connection {
  public:
    Connection(int fd, std::shared_ptr<ConnectionLimiter> limiter)
        : fd_(fd), limiter_(std::move(limiter)) {}
    ~Connection();
    Connection(Connection&& other) noexcept;
    Connection& operator=(Connection&& other) noexcept;
    Connection(const Connection&) = delete;
    Connection& operator=(const Connection&) = delete;

    int fd() const {
        return fd_;
    }

  private:
    void reset();

    int fd_ = -1;
    std::shared_ptr<ConnectionLimiter> limiter_;
};

class UdsServer {
  public:
    // L2: `max_connections` and `idle_timeout` default to the constants
    // above; the overload exists so a test can drive a one-slot server.
    static Expected<UdsServer, IpcError>
    bind(const std::string& path, std::vector<std::uint32_t> allowed_uids,
         std::size_t max_connections = kDefaultMaxConnections,
         std::chrono::seconds idle_timeout = kDefaultIdleTimeout);

    ~UdsServer();
    UdsServer(UdsServer&& other) noexcept;
    UdsServer& operator=(UdsServer&& other) noexcept;
    UdsServer(const UdsServer&) = delete;
    UdsServer& operator=(const UdsServer&) = delete;

    // Blocking accept. Returns `nullopt` (after closing the connection) for
    // anything that concerns only this one connection — a peer whose UID
    // isn't in the allowlist, a peer that vanished before accept() returned,
    // a momentary fd or memory shortage — so callers loop and keep serving.
    // An error means the listening socket itself is unusable and the loop
    // should stop (M2). Same contract as the Rust side's
    // `UdsServer::accept`.
    //
    // L2: blocks until a connection slot is free before accepting at all,
    // which leaves surplus peers in the kernel's listen backlog, and arms
    // the returned fd with SO_RCVTIMEO = `idle_timeout` so a silent peer
    // cannot pin a thread forever.
    Expected<std::optional<std::pair<Connection, PeerCred>>, IpcError> accept();

    std::chrono::seconds idle_timeout() const {
        return idle_timeout_;
    }
    std::size_t live_connections() const {
        return limiter_->in_use();
    }

  private:
    UdsServer(int listen_fd, std::vector<std::uint32_t> allowed_uids, std::size_t max_connections,
              std::chrono::seconds idle_timeout)
        : listen_fd_(listen_fd), allowed_uids_(std::move(allowed_uids)),
          limiter_(std::make_shared<ConnectionLimiter>(max_connections)),
          idle_timeout_(idle_timeout) {}

    int listen_fd_ = -1;
    std::vector<std::uint32_t> allowed_uids_;
    std::shared_ptr<ConnectionLimiter> limiter_;
    std::chrono::seconds idle_timeout_{kDefaultIdleTimeout};
};

// True for an accept(2) errno that costs only the connection being
// accepted, not the listening socket. Must stay in step with
// `is_per_connection_error` in crates/neuroos-ipc/src/server.rs. Exposed so
// the IPC smoke test can check the classification directly.
bool accept_errno_is_per_connection(int err);

// EMFILE/ENFILE/ENOBUFS/ENOMEM: the one group worth backing off for, since
// retrying immediately would spin at full CPU until the shortage passes.
bool accept_errno_is_resource_exhaustion(int err);

// Connects to `path` within `timeout`. The returned fd is blocking and owned
// by the caller (close() it when done).
Expected<int, IpcError> connect(const std::string& path, std::chrono::milliseconds timeout);

} // namespace neuroos::ipc
