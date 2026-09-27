// IT (phases.md §5.3): drives a real, running neuroos-inference over its
// real inference.sock with the real BitNet model — GetInfo, AttachRing,
// Generate (reading real streamed tokens off the real memfd ring via the
// SCM_RIGHTS fd it hands back), and Cancel. Same "plain main(), explicit
// checks, exit code" convention as cpp_ipc_smoke.cpp (no framework
// vendored). Expects an already-running neuroos-inference at
// $XDG_RUNTIME_DIR/neuroos/inference.sock — see tests/contract/
// inference_smoke.sh, which starts one against the real downloaded model.
#include <unistd.h>

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <thread>

#include "libneuroos/framing.hpp"
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

std::string env_or(const char* name, const std::string& fallback) {
    const char* v = std::getenv(name);
    return (v != nullptr) ? std::string(v) : fallback;
}

int connect_or_die(const std::string& sock_path) {
    auto fd = neuroos::ipc::connect(sock_path, std::chrono::milliseconds(2000));
    check(fd.has_value(), ("connect to " + sock_path + " must succeed (is neuroos-inference running?)").c_str());
    return fd.value();
}

} // namespace

int main() {
    std::string runtime_dir = env_or("XDG_RUNTIME_DIR", "/run/user/" + std::to_string(::getuid()));
    std::string sock_path = runtime_dir + "/neuroos/inference.sock";

    // 1. GetInfo: real model metadata.
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        req.set_request_id(1);
        req.mutable_get_info_request()->set_tokenize_text("hello world");
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write GetInfoRequest");
        auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read GetInfoResponse");
        const auto& info = resp.value()->get_info_response();
        check(!info.model_name().empty(), "model_name must be non-empty");
        check(info.vocab_size() > 0, "vocab_size must be positive");
        check(info.token_count() > 0, "tokenize_text must produce at least one token");
        std::printf("OK: GetInfo -> model=%s vocab=%llu context=%u tokenize(\"hello world\")=%u tokens\n",
                   info.model_name().c_str(), static_cast<unsigned long long>(info.vocab_size()),
                   info.context_length(), info.token_count());
        ::close(fd);
    }

    // 2. AttachRing: real SCM_RIGHTS fd handoff for a fresh ring.
    int ring_fd = -1;
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        req.set_request_id(2);
        req.mutable_attach_ring_request()->set_ring_name("smoke-test-ring");
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write AttachRingRequest");
        auto resp = neuroos::ipc::read_envelope_with_fd(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read AttachRingResponse");
        check(resp.value()->envelope.attach_ring_response().ok(), "attach_ring_response.ok must be true");
        check(resp.value()->fd >= 0, "must receive the ring fd via SCM_RIGHTS");
        ring_fd = resp.value()->fd;
        std::printf("OK: AttachRing -> received ring fd\n");
        ::close(fd);
    }
    auto ring = neuroos::shm::Ring::open(ring_fd);
    auto reader = ring.reader();

    // 3. Generate: a short, real completion from the real model, streamed
    // through the real ring.
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        req.set_request_id(3);
        auto* gen = req.mutable_generate_request();
        gen->set_prompt("The capital of France is");
        gen->set_max_tokens(8);
        gen->set_lane(neuroos::v1::LANE_INTERACTIVE);
        gen->set_ring_name("smoke-test-ring");
        gen->set_temperature(0.0F); // greedy: deterministic, no seed needed
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write GenerateRequest");
        auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read GenerateResponse");
        check(resp.value()->generate_response().accepted(), "GenerateRequest must be accepted");
        ::close(fd);

        std::string generated;
        int pieces = 0;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(30);
        while (std::chrono::steady_clock::now() < deadline) {
            auto piece = reader.try_read();
            if (!piece.has_value()) {
                std::this_thread::sleep_for(std::chrono::milliseconds(20));
                continue;
            }
            generated.append(reinterpret_cast<const char*>(piece->payload.data()), piece->payload.size());
            ++pieces;
            if ((piece->flags & neuroos::shm::kFlagEos) != 0 || pieces >= 8) {
                break;
            }
        }
        check(pieces > 0, "must have read at least one real generated token from the ring");
        std::printf("OK: Generate -> %d real token(s) from the model: \"%s\"\n", pieces,
                   generated.c_str());
    }

    // 4. Cancel: a background job, cancelled before it can finish.
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        req.set_request_id(4);
        auto* gen = req.mutable_generate_request();
        gen->set_prompt("Once upon a time");
        gen->set_max_tokens(500); // long enough that cancel actually races a real decode step
        gen->set_lane(neuroos::v1::LANE_BACKGROUND);
        gen->set_ring_name("smoke-test-ring");
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write background GenerateRequest");
        auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read background GenerateResponse");
        check(resp.value()->generate_response().accepted(), "background GenerateRequest must be accepted");
        std::uint64_t generation_id = resp.value()->generate_response().generation_id();
        ::close(fd);

        std::this_thread::sleep_for(std::chrono::milliseconds(100));

        int cfd = connect_or_die(sock_path);
        neuroos::v1::Envelope creq;
        creq.set_schema_version(1);
        creq.set_request_id(5);
        creq.mutable_cancel_request()->set_generation_id(generation_id);
        check(neuroos::ipc::write_envelope(cfd, creq, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write CancelRequest");
        auto cresp = neuroos::ipc::read_envelope(cfd, neuroos::ipc::kDefaultMaxFrame);
        check(cresp.has_value() && cresp.value().has_value(), "read CancelResponse");
        check(cresp.value()->cancel_response().cancelled(), "cancel_response.cancelled must be true");
        std::printf("OK: Cancel -> a real in-flight background generation was cancelled\n");
        ::close(cfd);
    }

    std::printf("all cpp_inference_smoke checks passed\n");
    return 0;
}
