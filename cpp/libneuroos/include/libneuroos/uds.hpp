// UDS server/client (Architecture.md §5.1), C++ side. Mirrors
// crates/neuroos-ipc/src/{server,client}.rs: filesystem-path sockets only,
// SO_PEERCRED-checked on accept, blocking (one std::jthread per connection
// is the intended usage, not async I/O — rules.md §"C++ threads").
#pragma once

#include <chrono>
#include <cstdint>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "libneuroos/expected.hpp"
#include "libneuroos/ipc_error.hpp"
#include "libneuroos/peercred.hpp"

namespace neuroos::ipc {

class UdsServer {
  public:
    static Expected<UdsServer, IpcError> bind(const std::string& path,
                                              std::vector<std::uint32_t> allowed_uids);

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
    Expected<std::optional<std::pair<int, PeerCred>>, IpcError> accept();

  private:
    explicit UdsServer(int listen_fd, std::vector<std::uint32_t> allowed_uids)
        : listen_fd_(listen_fd), allowed_uids_(std::move(allowed_uids)) {}

    int listen_fd_ = -1;
    std::vector<std::uint32_t> allowed_uids_;
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
