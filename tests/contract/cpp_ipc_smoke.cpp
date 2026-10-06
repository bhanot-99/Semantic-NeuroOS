// Phase 2 (P2 prerequisite infra): standalone smoke test for libneuroos's
// C++ IPC framing + UDS server/client + SCM_RIGHTS fd passing, all new this
// phase. Same "plain main(), explicit checks, exit code" convention as the
// existing shm_reader/shm_writer/shm_stress tools — deliberately NOT
// assert(): this project's default CMake build type is RelWithDebInfo,
// which defines NDEBUG and silently compiles every assert() to nothing, so
// a real test binary here must use its own always-on check.
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <string>
#include <thread>

#include "libneuroos/framing.hpp"
#include "libneuroos/health_server.hpp"
#include "libneuroos/shm.hpp"
#include "libneuroos/uds.hpp"
#include "neuroos/v1/envelope.pb.h"

namespace {

void check(bool cond, const char* what) {
    if (!cond) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        std::exit(1);
    }
}

std::string temp_socket_path(const std::string& name) {
    return "/tmp/neuroos-cpp-ipc-smoke-" + std::to_string(::getpid()) + "-" + name + ".sock";
}

void test_envelope_round_trip() {
    const std::string path = temp_socket_path("roundtrip");
    ::unlink(path.c_str());

    auto server = neuroos::ipc::UdsServer::bind(path, {static_cast<std::uint32_t>(::getuid())});
    check(server.has_value(), "bind must succeed");

    std::thread server_thread([&server] {
        auto accepted = server.value().accept();
        check(accepted.has_value() && accepted.value().has_value(), "accept must succeed");
        // L2: the accepted connection owns its fd and its slot; it closes
        // the fd when this scope ends, so no explicit ::close below.
        auto conn = std::move(accepted.value()->first);
        int client_fd = conn.fd();

        auto req = neuroos::ipc::read_envelope(client_fd, neuroos::ipc::kDefaultMaxFrame);
        check(req.has_value() && req.value().has_value(), "server must read the request");
        check(req.value()->body_case() == neuroos::v1::Envelope::kHealthRequest,
              "request must be a HealthRequest");

        neuroos::v1::Envelope resp;
        resp.set_schema_version(1);
        resp.set_request_id(req.value()->request_id());
        resp.mutable_health_response()->set_status(neuroos::v1::STATUS_OK);
        resp.mutable_health_response()->set_build_info("cpp-ipc-smoke");
        auto wrote = neuroos::ipc::write_envelope(client_fd, resp, neuroos::ipc::kDefaultMaxFrame);
        check(wrote.has_value(), "server must write the response");
    });

    // give the server thread a moment to reach accept()
    std::this_thread::sleep_for(std::chrono::milliseconds(50));

    auto client_fd = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    check(client_fd.has_value(), "client connect must succeed");

    neuroos::v1::Envelope req;
    req.set_schema_version(1);
    req.set_request_id(42);
    req.mutable_health_request();
    auto wrote =
        neuroos::ipc::write_envelope(client_fd.value(), req, neuroos::ipc::kDefaultMaxFrame);
    check(wrote.has_value(), "client must write the request");

    auto resp = neuroos::ipc::read_envelope(client_fd.value(), neuroos::ipc::kDefaultMaxFrame);
    check(resp.has_value() && resp.value().has_value(), "client must read the response");
    check(resp.value()->request_id() == 42, "request_id must round-trip");
    check(resp.value()->health_response().status() == neuroos::v1::STATUS_OK,
          "status must round-trip");
    check(resp.value()->health_response().build_info() == "cpp-ipc-smoke",
          "build_info must round-trip");

    ::close(client_fd.value());
    server_thread.join();
    ::unlink(path.c_str());
    std::printf("OK: envelope round trip over a real UDS socket\n");
}

void test_scm_rights_fd_passing() {
    const std::string path = temp_socket_path("fdpass");
    ::unlink(path.c_str());

    auto server = neuroos::ipc::UdsServer::bind(path, {static_cast<std::uint32_t>(::getuid())});
    check(server.has_value(), "bind must succeed");

    std::thread server_thread([&server] {
        auto accepted = server.value().accept();
        check(accepted.has_value() && accepted.value().has_value(), "accept must succeed");
        // L2: the accepted connection owns its fd and its slot; it closes
        // the fd when this scope ends, so no explicit ::close below.
        auto conn = std::move(accepted.value()->first);
        int client_fd = conn.fd();

        auto ring = neuroos::shm::Ring::create("smoke-ring", 4, 128);
        bool wrote_token = ring.writer().write(/*token_id=*/7, /*flags=*/0,
                                               reinterpret_cast<const std::uint8_t*>("hi"), 2);
        check(wrote_token, "ring write must succeed");

        neuroos::v1::Envelope resp;
        resp.set_schema_version(1);
        auto wrote = neuroos::ipc::write_envelope_with_fd(
            client_fd, resp, neuroos::ipc::kDefaultMaxFrame, ring.fd());
        check(wrote.has_value(), "server must write the response with the attached fd");
    });

    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    auto client_fd = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    check(client_fd.has_value(), "client connect must succeed");

    auto got =
        neuroos::ipc::read_envelope_with_fd(client_fd.value(), neuroos::ipc::kDefaultMaxFrame);
    check(got.has_value() && got.value().has_value(), "client must read the response");
    check(got.value()->fd >= 0, "must have received the ring's fd via SCM_RIGHTS");

    auto received_ring = neuroos::shm::Ring::open(got.value()->fd);
    auto piece = received_ring.reader().try_read();
    check(piece.has_value(), "reader must see the written token");
    check(piece->token_id == 7, "token_id must round-trip");
    check(piece->payload.size() == 2 && piece->payload[0] == 'h' && piece->payload[1] == 'i',
          "payload bytes must round-trip");

    ::close(client_fd.value());
    server_thread.join();
    ::unlink(path.c_str());
    std::printf("OK: SCM_RIGHTS fd passing delivers a working ring fd\n");
}

void test_health_server_round_trip() {
    const std::string path = temp_socket_path("health");
    ::unlink(path.c_str());

    neuroos::health::HealthServer server("cpp-ipc-smoke v0");
    server.record_latency("op", std::chrono::milliseconds(5));
    server.incr_error("timeout");

    std::thread server_thread(
        [&server, &path] { server.serve(path, {static_cast<std::uint32_t>(::getuid())}); });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));

    auto client_fd = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    check(client_fd.has_value(), "client connect must succeed");

    neuroos::v1::Envelope req;
    req.set_schema_version(1);
    req.mutable_health_request();
    auto wrote =
        neuroos::ipc::write_envelope(client_fd.value(), req, neuroos::ipc::kDefaultMaxFrame);
    check(wrote.has_value(), "client must write the HealthRequest");

    auto resp = neuroos::ipc::read_envelope(client_fd.value(), neuroos::ipc::kDefaultMaxFrame);
    check(resp.has_value() && resp.value().has_value(), "client must read the HealthResponse");
    const auto& hr = resp.value()->health_response();
    check(hr.status() == neuroos::v1::STATUS_OK, "status must be OK");
    check(hr.build_info() == "cpp-ipc-smoke v0", "build_info must round-trip");
    check(hr.rss_bytes() > 0, "rss_bytes must be positive for a real process");
    check(hr.error_counters().at("timeout") == 1, "error counter must round-trip");
    check(hr.latency_histograms().at("op").count() == 1, "histogram count must round-trip");

    ::close(client_fd.value());
    server_thread.detach(); // serve() loops forever; the process exit reclaims it
    ::unlink(path.c_str());
    std::printf("OK: health server round trip over a real UDS socket\n");
}

// M2: a failure that costs one connection must not be classified as a
// dead listener, or every C++ server dies permanently on the first one.
// Must agree with `is_per_connection_error` in
// crates/neuroos-ipc/src/server.rs.
void test_accept_error_classification() {
    for (int err : {ECONNABORTED, EINTR, EAGAIN, EPERM, ECONNRESET, ETIMEDOUT, EMFILE, ENFILE,
                    ENOBUFS, ENOMEM}) {
        check(neuroos::ipc::accept_errno_is_per_connection(err),
              "a doomed connection must not doom the listener");
    }
    for (int err : {EBADF, EINVAL, ENOTSOCK, EFAULT}) {
        check(!neuroos::ipc::accept_errno_is_per_connection(err),
              "a broken listener must be reported as fatal");
    }
    check(neuroos::ipc::accept_errno_is_resource_exhaustion(EMFILE), "EMFILE must get the backoff");
    check(!neuroos::ipc::accept_errno_is_resource_exhaustion(ECONNABORTED),
          "ECONNABORTED must not get the backoff");
    std::printf("OK: accept error classification matches the Rust side\n");
}


// L2: the connection ceiling is real -- a one-slot server accepts the
// first peer and leaves the second in the listen backlog until the first
// Connection is destroyed. Before the fix accept() returned immediately
// for every peer and each cost a detached thread and an fd, with no bound.
void test_connection_limit() {
    const std::string path = temp_socket_path("conn-limit");
    ::unlink(path.c_str());

    auto server = neuroos::ipc::UdsServer::bind(path, {static_cast<std::uint32_t>(::getuid())},
                                                /*max_connections=*/1,
                                                /*idle_timeout=*/std::chrono::seconds(1));
    check(server.has_value(), "bind must succeed");

    auto first = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    auto second = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    check(first.has_value() && second.has_value(), "both peers must connect");

    auto accepted = server.value().accept();
    check(accepted.has_value() && accepted.value().has_value(), "the first peer is accepted");
    check(server.value().live_connections() == 1, "the one slot must be in use");

    // A second accept() must block while the slot is held. Prove it by
    // running it on a thread that cannot finish until the slot is freed.
    std::atomic<bool> second_accepted{false};
    std::thread waiter([&server, &second_accepted] {
        auto next = server.value().accept();
        check(next.has_value() && next.value().has_value(), "the second peer is accepted later");
        second_accepted.store(true);
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(150));
    check(!second_accepted.load(), "accept must block while the server is at max_connections");

    // Destroying the first connection closes its fd and frees its slot.
    accepted.value().reset();
    waiter.join();
    check(second_accepted.load(), "freeing a slot must let the next accept proceed");

    ::close(first.value());
    ::close(second.value());
    ::unlink(path.c_str());
    std::printf("OK: max_connections bounds the live connection count\n");
}

// L2: an accepted connection that never sends anything is closed after
// the idle timeout (SO_RCVTIMEO), instead of pinning its thread and fd
// for the lifetime of the process.
void test_idle_connection_is_closed() {
    const std::string path = temp_socket_path("idle");
    ::unlink(path.c_str());

    auto server = neuroos::ipc::UdsServer::bind(path, {static_cast<std::uint32_t>(::getuid())},
                                                neuroos::ipc::kDefaultMaxConnections,
                                                /*idle_timeout=*/std::chrono::seconds(1));
    check(server.has_value(), "bind must succeed");

    auto client = neuroos::ipc::connect(path, std::chrono::milliseconds(1000));
    check(client.has_value(), "client connect must succeed");

    auto accepted = server.value().accept();
    check(accepted.has_value() && accepted.value().has_value(), "accept must succeed");

    // The client is connected but silent: the read must give up, not hang.
    auto started = std::chrono::steady_clock::now();
    auto req = neuroos::ipc::read_envelope(accepted.value()->first.fd(),
                                           neuroos::ipc::kDefaultMaxFrame);
    auto elapsed = std::chrono::steady_clock::now() - started;
    check(!req.has_value(), "a silent peer's read must fail, not block forever");
    check(elapsed >= std::chrono::milliseconds(900), "the read must wait out the idle timeout");
    check(elapsed < std::chrono::seconds(5), "the read must not wait much past it");

    ::close(client.value());
    ::unlink(path.c_str());
    std::printf("OK: a silent peer's connection times out\n");
}

// L16: the socket file is 0600 and its parent runtime dir 0700, whatever
// the service's umask is. Mirrors the Rust-side test in
// crates/neuroos-ipc/src/server.rs.
void test_socket_permissions() {
    const std::string dir = "/tmp/neuroos-cpp-ipc-smoke-" + std::to_string(::getpid()) + "-perm";
    const std::string path = dir + "/perm.sock";
    std::filesystem::remove_all(dir);
    // A permissive umask is exactly the case the fix is for.
    mode_t previous = ::umask(0);

    auto server = neuroos::ipc::UdsServer::bind(path, {static_cast<std::uint32_t>(::getuid())});
    ::umask(previous);
    check(server.has_value(), "bind must succeed");

    struct stat st {};
    check(::stat(path.c_str(), &st) == 0, "the socket file must exist");
    check((st.st_mode & 07777) == 0600, "the socket file must be 0600");
    struct stat dst {};
    check(::stat(dir.c_str(), &dst) == 0, "the runtime dir must exist");
    check((dst.st_mode & 07777) == 0700, "the runtime dir must be 0700");

    std::filesystem::remove_all(dir);
    std::printf("OK: socket 0600 and runtime dir 0700 regardless of umask\n");
}

// L4: `Ring::open` owns the fd it is handed, so a ring that fails
// validation must not leak it. Opening a bad fd many times used to burn
// one fd per attempt and would eventually hit EMFILE.
void test_ring_open_does_not_leak_the_fd_on_failure() {
    auto open_fd_count = [] {
        std::size_t n = 0;
        for (const auto& entry : std::filesystem::directory_iterator("/proc/self/fd")) {
            (void)entry;
            ++n;
        }
        return n;
    };

    // Warm up the iterator's own allocations before measuring.
    (void)open_fd_count();
    std::size_t before = open_fd_count();
    for (int i = 0; i < 64; ++i) {
        // A memfd far too small to hold a ring header: `Ring::open`
        // throws, and must close the fd on the way out.
        int fd = static_cast<int>(::syscall(SYS_memfd_create, "leak-probe", MFD_CLOEXEC));
        check(fd >= 0, "memfd_create must succeed");
        check(::ftruncate(fd, 8) == 0, "ftruncate must succeed");
        bool threw = false;
        try {
            auto ring = neuroos::shm::Ring::open(fd);
            (void)ring;
        } catch (const neuroos::shm::RingError&) {
            threw = true;
        }
        check(threw, "an undersized fd must be rejected");
    }
    std::size_t after = open_fd_count();
    check(after <= before, "Ring::open must not leak an fd when it rejects one");
    std::printf("OK: Ring::open closes the fd it rejects\n");
}

} // namespace

int main() {
    test_envelope_round_trip();
    test_scm_rights_fd_passing();
    test_health_server_round_trip();
    test_accept_error_classification();
    test_connection_limit();
    test_idle_connection_is_closed();
    test_socket_permissions();
    test_ring_open_does_not_leak_the_fd_on_failure();
    std::printf("all cpp_ipc_smoke checks passed\n");
    return 0;
}
