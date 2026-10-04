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

    // 4. Cancel: a background job, cancelled before it can finish. Measures
    // real cancellation latency (phases.md §5.4: "Cancellation latency <= 1
    // decode step (measured)") by timing how long real tokens keep arriving
    // in the ring after the CancelRequest is sent.
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

        std::this_thread::sleep_for(std::chrono::milliseconds(200));
        while (reader.try_read().has_value()) {
        } // drain whatever already arrived so the post-cancel count below is clean

        auto cancel_sent_at = std::chrono::steady_clock::now();
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
        ::close(cfd);

        int tokens_after_cancel = 0;
        std::chrono::steady_clock::time_point last_token_at = cancel_sent_at;
        auto drain_until = std::chrono::steady_clock::now() + std::chrono::milliseconds(1000);
        while (std::chrono::steady_clock::now() < drain_until) {
            if (reader.try_read().has_value()) {
                ++tokens_after_cancel;
                last_token_at = std::chrono::steady_clock::now();
            } else {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
        }
        double latency_ms =
            std::chrono::duration<double, std::milli>(last_token_at - cancel_sent_at).count();
        std::printf("OK: Cancel -> stopped after %d more token(s), last one %.1f ms after "
                   "CancelRequest was sent (a single real decode step is ~55-75 ms per "
                   "reports/bench/)\n",
                   tokens_after_cancel, latency_ms);
    }

    // 5. Interactive preemption under background load (PRD FR-INF-06,
    // phases.md §5.4: "Interactive preemption verified under background
    // load"): start a long real background generation, then submit a real
    // interactive request on a separate ring and confirm it completes
    // promptly instead of waiting for the background job.
    {
        auto interactive_ring_fd_of = [&](const std::string& name) {
            int fd = connect_or_die(sock_path);
            neuroos::v1::Envelope req;
            req.set_schema_version(1);
            req.mutable_attach_ring_request()->set_ring_name(name);
            check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
                 "write AttachRingRequest (preemption test)");
            auto r = neuroos::ipc::read_envelope_with_fd(fd, neuroos::ipc::kDefaultMaxFrame);
            check(r.has_value() && r.value().has_value() && r.value()->fd >= 0,
                 "AttachRing (preemption test) must hand back a real fd");
            ::close(fd);
            return r.value()->fd;
        };

        neuroos::shm::Ring bg_ring = neuroos::shm::Ring::open(interactive_ring_fd_of("preempt-bg-ring"));
        neuroos::shm::Ring it_ring = neuroos::shm::Ring::open(interactive_ring_fd_of("preempt-it-ring"));

        int bfd = connect_or_die(sock_path);
        neuroos::v1::Envelope breq;
        breq.set_schema_version(1);
        auto* bgen = breq.mutable_generate_request();
        bgen->set_prompt("The history of the Roman Empire began");
        std::uint32_t bg_max_tokens = 400;
        if (const char* override_tokens = std::getenv("NEUROOS_TEST_BG_MAX_TOKENS")) {
            bg_max_tokens = static_cast<std::uint32_t>(std::strtoul(override_tokens, nullptr, 10));
        }
        bgen->set_max_tokens(bg_max_tokens);
        bgen->set_lane(neuroos::v1::LANE_BACKGROUND);
        bgen->set_ring_name("preempt-bg-ring");
        check(neuroos::ipc::write_envelope(bfd, breq, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write background load GenerateRequest");
        auto bresp = neuroos::ipc::read_envelope(bfd, neuroos::ipc::kDefaultMaxFrame);
        check(bresp.has_value() && bresp.value().has_value() && bresp.value()->generate_response().accepted(),
             "background load GenerateRequest must be accepted");
        std::uint64_t bg_generation_id = bresp.value()->generate_response().generation_id();
        ::close(bfd);

        std::this_thread::sleep_for(std::chrono::milliseconds(150)); // let the background job start decoding

        auto it_start = std::chrono::steady_clock::now();
        int ifd = connect_or_die(sock_path);
        neuroos::v1::Envelope ireq;
        ireq.set_schema_version(1);
        auto* igen = ireq.mutable_generate_request();
        igen->set_prompt("2 + 2 =");
        igen->set_max_tokens(4);
        igen->set_lane(neuroos::v1::LANE_INTERACTIVE);
        igen->set_ring_name("preempt-it-ring");
        igen->set_temperature(0.0F);
        check(neuroos::ipc::write_envelope(ifd, ireq, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write interactive GenerateRequest under background load");
        auto iresp = neuroos::ipc::read_envelope(ifd, neuroos::ipc::kDefaultMaxFrame);
        check(iresp.has_value() && iresp.value().has_value() && iresp.value()->generate_response().accepted(),
             "interactive GenerateRequest must be accepted even while background is running");
        ::close(ifd);

        auto it_reader = it_ring.reader();
        int it_pieces = 0;
        int it_deadline_s = 10;
        if (const char* override_s = std::getenv("NEUROOS_TEST_PREEMPTION_DEADLINE_S")) {
            it_deadline_s = std::atoi(override_s);
        }
        auto it_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(it_deadline_s);
        while (std::chrono::steady_clock::now() < it_deadline && it_pieces < 4) {
            if (it_reader.try_read().has_value()) {
                ++it_pieces;
            } else {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
        }
        double it_latency_ms = std::chrono::duration<double, std::milli>(
                                   std::chrono::steady_clock::now() - it_start)
                                   .count();
        check(it_pieces == 4, "interactive job must complete under background load, not stall");
        // A generous bound: 4 interactive tokens plus scheduling/preemption
        // overhead should complete in well under the time 400 background
        // tokens would take (400 * ~60 ms/token ~= 24 s) if preemption
        // wasn't working and interactive had to wait its turn. Configurable
        // via NEUROOS_TEST_PREEMPTION_MAX_MS for a slow (e.g. -O0 coverage)
        // build of neuroos-inference itself, which is proportionally slower
        // across the board, not specifically because preemption broke.
        double max_ms = 5000.0;
        if (const char* override_ms = std::getenv("NEUROOS_TEST_PREEMPTION_MAX_MS")) {
            max_ms = std::strtod(override_ms, nullptr);
        }
        check(it_latency_ms < max_ms,
             "interactive job took too long under background load - preemption may not be working");
        std::printf("OK: interactive preemption -> %d interactive token(s) completed in %.1f ms "
                   "while a real 400-token background job was running\n",
                   it_pieces, it_latency_ms);

        // Clean up the still-running background job.
        int cfd2 = connect_or_die(sock_path);
        neuroos::v1::Envelope creq2;
        creq2.mutable_cancel_request()->set_generation_id(bg_generation_id);
        neuroos::ipc::write_envelope(cfd2, creq2, neuroos::ipc::kDefaultMaxFrame);
        neuroos::ipc::read_envelope(cfd2, neuroos::ipc::kDefaultMaxFrame);
        ::close(cfd2);
        (void)bg_ring;
    }

    // 5b. M12: a job cancelled while it is still *queued* must be taken
    // out of the queue, not merely flagged. Before the fix, a queued job
    // ran its full prompt prefill and decode once the job ahead of it
    // finished, and only then wrote its end-of-stream slot -- so the
    // cancelling client waited out the whole job ahead of it for an answer
    // it had already abandoned. Two interactive jobs are submitted
    // back-to-back on separate rings; the second one is cancelled at once
    // and must be terminated immediately, long before the first (400
    // tokens, ~24 s of real decoding) could possibly finish.
    {
        int afd = connect_or_die(sock_path);
        neuroos::v1::Envelope areq;
        areq.set_schema_version(1);
        areq.mutable_attach_ring_request()->set_ring_name("queued-cancel-ring");
        check(neuroos::ipc::write_envelope(afd, areq, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write AttachRingRequest for the queued-cancel ring");
        auto aresp = neuroos::ipc::read_envelope_with_fd(afd, neuroos::ipc::kDefaultMaxFrame);
        check(aresp.has_value() && aresp.value().has_value() && aresp.value()->fd >= 0,
             "receive the queued-cancel ring fd");
        auto qc_ring = neuroos::shm::Ring::open(aresp.value()->fd);
        ::close(afd);

        auto submit = [&](const char* ring_name, std::uint32_t max_tokens) {
            int fd = connect_or_die(sock_path);
            neuroos::v1::Envelope req;
            req.set_schema_version(1);
            auto* gen = req.mutable_generate_request();
            gen->set_prompt("Count slowly from one to two hundred.");
            gen->set_max_tokens(max_tokens);
            gen->set_lane(neuroos::v1::LANE_INTERACTIVE);
            gen->set_ring_name(ring_name);
            gen->set_temperature(0.0F);
            check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
                 "write GenerateRequest");
            auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
            check(resp.has_value() && resp.value().has_value() &&
                      resp.value()->generate_response().accepted(),
                 "GenerateRequest must be accepted");
            std::uint64_t id = resp.value()->generate_response().generation_id();
            ::close(fd);
            return id;
        };

        std::uint64_t running_id = submit("smoke-test-ring", 400);
        std::uint64_t queued_id = submit("queued-cancel-ring", 400);

        auto qc_reader = qc_ring.reader_for_generation(queued_id);
        auto cancel_sent_at = std::chrono::steady_clock::now();
        int cfd = connect_or_die(sock_path);
        neuroos::v1::Envelope creq;
        creq.mutable_cancel_request()->set_generation_id(queued_id);
        check(neuroos::ipc::write_envelope(cfd, creq, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write CancelRequest for the queued job");
        auto cresp = neuroos::ipc::read_envelope(cfd, neuroos::ipc::kDefaultMaxFrame);
        check(cresp.has_value() && cresp.value().has_value() &&
                  cresp.value()->cancel_response().cancelled(),
             "cancelling a queued job must report cancelled");
        ::close(cfd);

        // A single real decode step is ~55-75 ms, and the job ahead has 400
        // of them to run; 3 s is far below that and far above the cost of
        // writing one slot.
        bool terminated = false;
        int tokens = 0;
        auto deadline = cancel_sent_at + std::chrono::seconds(3);
        while (std::chrono::steady_clock::now() < deadline) {
            auto piece = qc_reader.try_read();
            if (!piece.has_value()) {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
                continue;
            }
            if ((piece->flags & neuroos::shm::kFlagEos) != 0) {
                check((piece->flags & neuroos::shm::kFlagCancel) != 0,
                     "a cancelled job's terminal slot must carry kFlagCancel");
                terminated = true;
                break;
            }
            ++tokens;
        }
        double latency_ms = std::chrono::duration<double, std::milli>(
                                std::chrono::steady_clock::now() - cancel_sent_at)
                                .count();
        check(terminated,
             "a queued job cancelled before it started must end its stream at once, not after "
             "the job ahead of it finishes");
        check(tokens == 0, "a queued job that was cancelled must never produce a token");
        std::printf("OK: queued Cancel -> stream ended %.1f ms after CancelRequest, 0 tokens "
                   "decoded (the job ahead of it was still running)\n",
                   latency_ms);

        // Clean up the job that was actually running.
        int cfd2 = connect_or_die(sock_path);
        neuroos::v1::Envelope creq2;
        creq2.mutable_cancel_request()->set_generation_id(running_id);
        neuroos::ipc::write_envelope(cfd2, creq2, neuroos::ipc::kDefaultMaxFrame);
        neuroos::ipc::read_envelope(cfd2, neuroos::ipc::kDefaultMaxFrame);
        ::close(cfd2);
        // Drain the shared ring so step 6 starts from a clean reader.
        while (reader.try_read().has_value()) {
        }
    }

    // 6. GBNF grammar (PRD FR-INF-04, phases.md §5.2 P2-S05): force the real
    // model to answer with exactly "yes" or "no", nothing else.
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        auto* gen = req.mutable_generate_request();
        gen->set_prompt("Is water wet? Answer with exactly one word.");
        gen->set_max_tokens(3);
        gen->set_lane(neuroos::v1::LANE_INTERACTIVE);
        gen->set_ring_name("smoke-test-ring");
        gen->set_temperature(0.0F);
        gen->set_grammar_gbnf("root ::= \"yes\" | \"no\"\n");
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write grammar-constrained GenerateRequest");
        auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read grammar-constrained GenerateResponse");
        check(resp.value()->generate_response().accepted(), "grammar-constrained GenerateRequest must be accepted");
        ::close(fd);

        std::string generated;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(15);
        while (std::chrono::steady_clock::now() < deadline) {
            auto piece = reader.try_read();
            if (!piece.has_value()) {
                std::this_thread::sleep_for(std::chrono::milliseconds(20));
                continue;
            }
            generated.append(reinterpret_cast<const char*>(piece->payload.data()), piece->payload.size());
            if ((piece->flags & neuroos::shm::kFlagEos) != 0 || generated == "yes" || generated == "no") {
                break;
            }
        }
        check(generated == "yes" || generated == "no",
             ("grammar must force output to exactly \"yes\" or \"no\", got \"" + generated + "\"").c_str());
        std::printf("OK: GBNF grammar -> real model output constrained to exactly \"%s\"\n",
                   generated.c_str());
    }

    // 7. Oversized prompt rejected synchronously (phases.md §5.3 FI), not
    // silently dropped after being queued.
    {
        int fd = connect_or_die(sock_path);
        neuroos::v1::Envelope req;
        req.set_schema_version(1);
        auto* gen = req.mutable_generate_request();
        // GetInfo (step 1) confirmed context_length; max_context_tokens for
        // this run's config is 512 (see inference_smoke.sh) — one word
        // repeated 2000 times tokenizes to well over that.
        std::string huge_prompt;
        for (int i = 0; i < 2000; ++i) {
            huge_prompt += "word ";
        }
        gen->set_prompt(huge_prompt);
        gen->set_max_tokens(1);
        gen->set_lane(neuroos::v1::LANE_INTERACTIVE);
        gen->set_ring_name("smoke-test-ring");
        check(neuroos::ipc::write_envelope(fd, req, neuroos::ipc::kDefaultMaxFrame).has_value(),
             "write oversized GenerateRequest");
        auto resp = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        check(resp.has_value() && resp.value().has_value(), "read oversized-prompt GenerateResponse");
        check(!resp.value()->generate_response().accepted(), "oversized prompt must be rejected, not accepted");
        check(!resp.value()->generate_response().error().empty(), "rejection must include an error message");
        std::printf("OK: oversized prompt rejected synchronously: \"%s\"\n",
                   resp.value()->generate_response().error().c_str());
        ::close(fd);
    }

    std::printf("all cpp_inference_smoke checks passed\n");
    return 0;
}
