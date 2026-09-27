// Error type for libneuroos's IPC layer (framing + UDS), returned via
// `Expected<T, IpcError>` per rules.md's C++ error-handling rule.
#pragma once

#include <cstdint>
#include <string>

namespace neuroos::ipc {

enum class IpcErrorKind {
    kIo,
    kFrameTooLarge,
    kTruncated,
    kDecodeFailed,
    kPeerRejected,
};

struct IpcError {
    IpcErrorKind kind;
    std::string message;

    static IpcError io(const std::string& what) {
        return IpcError{IpcErrorKind::kIo, what};
    }
    static IpcError frame_too_large(std::uint32_t len, std::uint32_t max_frame) {
        return IpcError{IpcErrorKind::kFrameTooLarge, "frame of " + std::to_string(len) +
                                                          " bytes exceeds max frame size " +
                                                          std::to_string(max_frame) + " bytes"};
    }
    static IpcError truncated() {
        return IpcError{IpcErrorKind::kTruncated, "connection closed mid-frame"};
    }
    static IpcError decode_failed(const std::string& what) {
        return IpcError{IpcErrorKind::kDecodeFailed, "failed to decode protobuf payload: " + what};
    }
    static IpcError peer_rejected(std::uint32_t uid) {
        return IpcError{IpcErrorKind::kPeerRejected,
                        "peer uid " + std::to_string(uid) + " not in allowlist"};
    }
};

} // namespace neuroos::ipc
