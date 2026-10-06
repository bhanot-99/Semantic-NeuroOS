#include "libneuroos/framing.hpp"

#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>

namespace neuroos::ipc {

namespace {

// L4: owns a received SCM_RIGHTS fd until the caller takes it. Every error
// path out of `read_envelope_with_fd` used to have to remember to close it
// by hand, and the two body-read failures did not, leaking C4's token-ring
// memfd to a client that could trigger a short read at will.
class FdGuard {
  public:
    FdGuard() = default;
    explicit FdGuard(int fd) : fd_(fd) {}
    ~FdGuard() {
        reset();
    }
    FdGuard(const FdGuard&) = delete;
    FdGuard& operator=(const FdGuard&) = delete;
    FdGuard(FdGuard&& other) noexcept : fd_(other.fd_) {
        other.fd_ = -1;
    }
    FdGuard& operator=(FdGuard&& other) noexcept {
        if (this != &other) {
            reset();
            fd_ = other.fd_;
            other.fd_ = -1;
        }
        return *this;
    }

    int get() const {
        return fd_;
    }
    bool valid() const {
        return fd_ >= 0;
    }

    // Replaces the held fd, closing whatever was there. L4: a peer can
    // attach SCM_RIGHTS to more than one recvmsg of the same 4-byte
    // header; the old code just overwrote the variable and leaked the
    // earlier fd.
    void replace(int fd) {
        reset();
        fd_ = fd;
    }

    // Hands ownership to the caller.
    int release() {
        int fd = fd_;
        fd_ = -1;
        return fd;
    }

    void reset() {
        if (fd_ >= 0) {
            ::close(fd_);
            fd_ = -1;
        }
    }

  private:
    int fd_ = -1;
};

// L4: how many SCM_RIGHTS fds one frame's control buffer makes room for.
// The protocol sends at most one (C4's token-ring memfd), but receiving a
// few means a peer that sends more gets them closed rather than leaked
// into the kernel's MSG_CTRUNC path.
constexpr std::size_t kMaxReceivedFds = 4;

// Reads exactly `len` bytes into `buf`, retrying on EINTR and on partial
// reads. Returns false on a clean EOF with zero bytes read so far (the
// "no more frames" case); a partial read followed by EOF is `Truncated`.
Expected<bool, IpcError> read_exact(int fd, std::uint8_t* buf, std::size_t len) {
    std::size_t got = 0;
    while (got < len) {
        ssize_t n = ::recv(fd, buf + got, len - got, 0);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return make_unexpected(IpcError::io(std::strerror(errno)));
        }
        if (n == 0) {
            if (got == 0) {
                return false; // clean EOF between frames
            }
            return make_unexpected(IpcError::truncated());
        }
        got += static_cast<std::size_t>(n);
    }
    return true;
}

Expected<void, IpcError> write_all(int fd, const std::uint8_t* buf, std::size_t len) {
    std::size_t sent = 0;
    while (sent < len) {
        ssize_t n = ::send(fd, buf + sent, len - sent, MSG_NOSIGNAL);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return make_unexpected(IpcError::io(std::strerror(errno)));
        }
        sent += static_cast<std::size_t>(n);
    }
    return {};
}

} // namespace

Expected<std::optional<std::vector<std::uint8_t>>, IpcError> read_frame(int fd,
                                                                        std::uint32_t max_frame) {
    std::uint8_t len_buf[4];
    auto got_header = read_exact(fd, len_buf, sizeof(len_buf));
    if (!got_header) {
        return make_unexpected(got_header.error());
    }
    if (!got_header.value()) {
        return std::optional<std::vector<std::uint8_t>>{std::nullopt};
    }
    std::uint32_t len;
    std::memcpy(&len, len_buf, sizeof(len)); // host is little-endian (x86_64/AArch64)
    if (len > max_frame) {
        return make_unexpected(IpcError::frame_too_large(len, max_frame));
    }
    std::vector<std::uint8_t> buf(len);
    if (len > 0) {
        auto got_body = read_exact(fd, buf.data(), buf.size());
        if (!got_body) {
            return make_unexpected(got_body.error());
        }
        if (!got_body.value()) {
            return make_unexpected(IpcError::truncated());
        }
    }
    return std::optional<std::vector<std::uint8_t>>{std::move(buf)};
}

Expected<void, IpcError> write_frame(int fd, const std::uint8_t* payload, std::size_t len,
                                     std::uint32_t max_frame) {
    if (len > max_frame) {
        return make_unexpected(
            IpcError::frame_too_large(static_cast<std::uint32_t>(len), max_frame));
    }
    std::uint32_t len32 = static_cast<std::uint32_t>(len);
    std::uint8_t len_buf[4];
    std::memcpy(len_buf, &len32, sizeof(len32));
    if (auto r = write_all(fd, len_buf, sizeof(len_buf)); !r) {
        return r;
    }
    return write_all(fd, payload, len);
}

Expected<std::optional<neuroos::v1::Envelope>, IpcError> read_envelope(int fd,
                                                                       std::uint32_t max_frame) {
    auto frame = read_frame(fd, max_frame);
    if (!frame) {
        return make_unexpected(frame.error());
    }
    if (!frame.value().has_value()) {
        return std::optional<neuroos::v1::Envelope>{std::nullopt};
    }
    neuroos::v1::Envelope env;
    const auto& buf = *frame.value();
    if (!env.ParseFromArray(buf.data(), static_cast<int>(buf.size()))) {
        return make_unexpected(IpcError::decode_failed("protobuf ParseFromArray failed"));
    }
    return std::optional<neuroos::v1::Envelope>{std::move(env)};
}

Expected<void, IpcError> write_envelope(int fd, const neuroos::v1::Envelope& env,
                                        std::uint32_t max_frame) {
    std::string buf = env.SerializeAsString();
    return write_frame(fd, reinterpret_cast<const std::uint8_t*>(buf.data()), buf.size(),
                       max_frame);
}

Expected<void, IpcError> write_envelope_with_fd(int fd, const neuroos::v1::Envelope& env,
                                                std::uint32_t max_frame, int fd_to_send) {
    std::string body = env.SerializeAsString();
    if (body.size() > max_frame) {
        return make_unexpected(
            IpcError::frame_too_large(static_cast<std::uint32_t>(body.size()), max_frame));
    }
    std::uint32_t len32 = static_cast<std::uint32_t>(body.size());
    std::vector<std::uint8_t> frame(sizeof(len32) + body.size());
    std::memcpy(frame.data(), &len32, sizeof(len32));
    std::memcpy(frame.data() + sizeof(len32), body.data(), body.size());

    iovec iov{.iov_base = frame.data(), .iov_len = frame.size()};
    alignas(struct cmsghdr) char cmsg_buf[CMSG_SPACE(sizeof(int))];
    msghdr msg{};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf;
    msg.msg_controllen = sizeof(cmsg_buf);

    cmsghdr* cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(int));
    std::memcpy(CMSG_DATA(cmsg), &fd_to_send, sizeof(int));

    for (;;) {
        ssize_t n = ::sendmsg(fd, &msg, MSG_NOSIGNAL);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return make_unexpected(IpcError::io(std::strerror(errno)));
        }
        if (static_cast<std::size_t>(n) != frame.size()) {
            // A short sendmsg would separate the SCM_RIGHTS control message
            // from part of the frame; this codebase's frames are small
            // enough (header responses, not the bulk token stream) that a
            // short write here indicates a real transport problem, not
            // something to silently continue from with `write_all`.
            return make_unexpected(
                IpcError::io("short sendmsg while attaching an fd: frame split across writes"));
        }
        return {};
    }
}

Expected<std::optional<EnvelopeWithFd>, IpcError> read_envelope_with_fd(int fd,
                                                                        std::uint32_t max_frame) {
    // The sender's single sendmsg() call carries the SCM_RIGHTS ancillary
    // data attached to the first bytes of the frame (the length prefix), not
    // spread across the whole frame. Linux/BSD only deliver ancillary data
    // to a recvmsg() call — a plain recv()/read() of those same bytes would
    // silently drop the fd (kernel closes it once the accompanying bytes are
    // consumed without anyone asking for the control data). So the header
    // itself must be read via recvmsg, not read_exact, or the fd is lost.
    std::uint8_t len_buf[4];
    // L4: closed on every error path below unless `release`d into the
    // returned EnvelopeWithFd.
    FdGuard received_fd;
    {
        std::size_t got = 0;
        while (got < sizeof(len_buf)) {
            iovec iov{.iov_base = len_buf + got, .iov_len = sizeof(len_buf) - got};
            // L4: room for more than one fd, so a peer that attaches
            // several does not get them silently truncated by the kernel
            // (MSG_CTRUNC) -- they are received and the extras closed
            // below, which is what makes the "one fd per frame" contract
            // enforceable rather than just assumed.
            alignas(struct cmsghdr) char cmsg_buf[CMSG_SPACE(sizeof(int) * kMaxReceivedFds)];
            msghdr msg{};
            msg.msg_iov = &iov;
            msg.msg_iovlen = 1;
            msg.msg_control = cmsg_buf;
            msg.msg_controllen = sizeof(cmsg_buf);

            // L4: MSG_CMSG_CLOEXEC, so a received fd does not survive an
            // exec(2) in this process. Every other fd this codebase opens
            // is already O_CLOEXEC (SOCK_CLOEXEC, MFD_CLOEXEC); this one
            // was the exception.
            ssize_t n = ::recvmsg(fd, &msg, MSG_CMSG_CLOEXEC);
            if (n < 0) {
                if (errno == EINTR) {
                    continue;
                }
                return make_unexpected(IpcError::io(std::strerror(errno)));
            }
            if (n == 0) {
                if (got == 0) {
                    return std::optional<EnvelopeWithFd>{std::nullopt}; // clean EOF between frames
                }
                return make_unexpected(IpcError::truncated());
            }
            // L4: walk *every* cmsg and every fd in it. The old code read
            // the first fd of the first header and assigned it over
            // whatever a previous iteration had stored, leaking both the
            // displaced fd and any extras.
            for (cmsghdr* cmsg = CMSG_FIRSTHDR(&msg); cmsg != nullptr;
                 cmsg = CMSG_NXTHDR(&msg, cmsg)) {
                if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) {
                    continue;
                }
                std::size_t payload = cmsg->cmsg_len - CMSG_LEN(0);
                std::size_t count = payload / sizeof(int);
                for (std::size_t i = 0; i < count; ++i) {
                    int one = -1;
                    std::memcpy(&one, CMSG_DATA(cmsg) + (i * sizeof(int)), sizeof(int));
                    // Keeping the last one matches the previous behaviour
                    // for the single-fd case; the difference is that the
                    // ones it supersedes are now closed, not leaked.
                    received_fd.replace(one);
                }
            }
            got += static_cast<std::size_t>(n);
        }
    }
    std::uint32_t len;
    std::memcpy(&len, len_buf, sizeof(len));
    if (len > max_frame) {
        return make_unexpected(IpcError::frame_too_large(len, max_frame));
    }

    std::vector<std::uint8_t> body(len);
    if (len > 0) {
        auto got_body = read_exact(fd, body.data(), body.size());
        if (!got_body) {
            return make_unexpected(got_body.error());
        }
        if (!got_body.value()) {
            return make_unexpected(IpcError::truncated());
        }
    }

    neuroos::v1::Envelope env;
    if (!env.ParseFromArray(body.data(), static_cast<int>(body.size()))) {
        return make_unexpected(IpcError::decode_failed("protobuf ParseFromArray failed"));
    }
    return std::optional<EnvelopeWithFd>{EnvelopeWithFd{std::move(env), received_fd.release()}};
}

} // namespace neuroos::ipc
