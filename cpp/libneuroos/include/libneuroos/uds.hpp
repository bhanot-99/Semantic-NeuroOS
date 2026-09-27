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

    // Blocking accept. Returns `nullopt` (after closing the connection) if
    // the peer's UID isn't in the allowlist, so callers loop and keep
    // serving rather than treating this as fatal — same contract as the
    // Rust side's `UdsServer::accept`.
    Expected<std::optional<std::pair<int, PeerCred>>, IpcError> accept();

  private:
    explicit UdsServer(int listen_fd, std::vector<std::uint32_t> allowed_uids)
        : listen_fd_(listen_fd), allowed_uids_(std::move(allowed_uids)) {}

    int listen_fd_ = -1;
    std::vector<std::uint32_t> allowed_uids_;
};

// Connects to `path` within `timeout`. The returned fd is blocking and owned
// by the caller (close() it when done).
Expected<int, IpcError> connect(const std::string& path, std::chrono::milliseconds timeout);

} // namespace neuroos::ipc
