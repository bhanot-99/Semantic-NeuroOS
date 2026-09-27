// u32-LE length-prefixed protobuf framing (Architecture.md §5.1), C++ side.
// Must interoperate byte-for-byte with crates/neuroos-ipc/src/framing.rs:
// same 4-byte little-endian length prefix, same 4 MiB default max frame.
#pragma once

#include <cstdint>
#include <optional>
#include <vector>

#include "libneuroos/expected.hpp"
#include "libneuroos/ipc_error.hpp"
#include "neuroos/v1/envelope.pb.h"

namespace neuroos::ipc {

constexpr std::uint32_t kDefaultMaxFrame = 4u * 1024 * 1024;

// Blocking read/write on a connected stream-socket fd. One call per frame;
// the caller owns the fd and its lifetime.
Expected<std::optional<std::vector<std::uint8_t>>, IpcError> read_frame(int fd,
                                                                        std::uint32_t max_frame);
Expected<void, IpcError> write_frame(int fd, const std::uint8_t* payload, std::size_t len,
                                     std::uint32_t max_frame);

Expected<std::optional<neuroos::v1::Envelope>, IpcError> read_envelope(int fd,
                                                                       std::uint32_t max_frame);
Expected<void, IpcError> write_envelope(int fd, const neuroos::v1::Envelope& env,
                                        std::uint32_t max_frame);

// Same as `write_envelope`, but attaches `fd_to_send` as SCM_RIGHTS ancillary
// data on the same sendmsg() call that carries the frame (Architecture.md
// §5.1: "The memfd is passed with SCM_RIGHTS over inference.sock"). The
// receiving end must call `read_envelope_with_fd`, or it will see this as an
// ordinary frame and silently drop the fd (the kernel closes an
// unconsumed SCM_RIGHTS fd once the receiving process's message queue entry
// is read without `recvmsg`'s control buffer — never leaks across
// processes, but the caller loses the intended handoff).
Expected<void, IpcError> write_envelope_with_fd(int fd, const neuroos::v1::Envelope& env,
                                                std::uint32_t max_frame, int fd_to_send);

struct EnvelopeWithFd {
    neuroos::v1::Envelope envelope;
    int fd = -1; // -1 if the peer sent no ancillary fd
};
Expected<std::optional<EnvelopeWithFd>, IpcError> read_envelope_with_fd(int fd,
                                                                        std::uint32_t max_frame);

} // namespace neuroos::ipc
