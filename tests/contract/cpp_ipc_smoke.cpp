// Phase 2 (P2 prerequisite infra): standalone smoke test for libneuroos's
// C++ IPC framing + UDS server/client + SCM_RIGHTS fd passing, all new this
// phase. Same "plain main(), explicit checks, exit code" convention as the
// existing shm_reader/shm_writer/shm_stress tools — deliberately NOT
// assert(): this project's default CMake build type is RelWithDebInfo,
// which defines NDEBUG and silently compiles every assert() to nothing, so
// a real test binary here must use its own always-on check.
#include <sys/mman.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
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
        int client_fd = accepted.value()->first;

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
        ::close(client_fd);
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
        int client_fd = accepted.value()->first;

        auto ring = neuroos::shm::Ring::create("smoke-ring", 4, 128);
        bool wrote_token = ring.writer().write(/*token_id=*/7, /*flags=*/0,
                                               reinterpret_cast<const std::uint8_t*>("hi"), 2);
        check(wrote_token, "ring write must succeed");

        neuroos::v1::Envelope resp;
        resp.set_schema_version(1);
        auto wrote = neuroos::ipc::write_envelope_with_fd(
            client_fd, resp, neuroos::ipc::kDefaultMaxFrame, ring.fd());
        check(wrote.has_value(), "server must write the response with the attached fd");
        ::close(client_fd);
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

} // namespace

int main() {
    test_envelope_round_trip();
    test_scm_rights_fd_passing();
    test_health_server_round_trip();
    test_accept_error_classification();
    std::printf("all cpp_ipc_smoke checks passed\n");
    return 0;
}
