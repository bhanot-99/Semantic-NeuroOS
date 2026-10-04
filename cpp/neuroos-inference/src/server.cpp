#include "server.hpp"

#include <spdlog/spdlog.h>
#include <unistd.h>

#include <chrono>
#include <sstream>
#include <thread>

#include "libneuroos/framing.hpp"
#include "libneuroos/uds.hpp"
#include "neuroos/v1/envelope.pb.h"

namespace neuroos::inference {

namespace {

std::uint64_t now_ns() {
    return static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                          std::chrono::system_clock::now().time_since_epoch())
                                          .count());
}

void handle_generate(const neuroos::v1::GenerateRequest& req, neuroos::v1::Envelope& resp,
                     LaneScheduler& lanes, RingRegistry& rings, const Model& model,
                     std::uint32_t max_context_tokens) {
    auto* out = resp.mutable_generate_response();
    if (req.ring_name().empty()) {
        out->set_accepted(false);
        out->set_error("ring_name is required (AttachRingRequest it first)");
        return;
    }
    if (!rings.exists(req.ring_name())) {
        out->set_accepted(false);
        out->set_error("unknown ring_name: " + req.ring_name() +
                       " (call AttachRingRequest before Generate)");
        return;
    }
    // phases.md §5.3 FI: "oversized prompt rejected" — reject synchronously,
    // before ever queuing the job, rather than letting the worker discover
    // it asynchronously (Context::generate's own oversized-prompt check
    // exists too, as defense in depth, but its failure is only visible in
    // the server log, not to the client — this is the client-visible gate).
    std::uint32_t prompt_tokens = model.tokenize_count(req.prompt());
    if (prompt_tokens > max_context_tokens) {
        out->set_accepted(false);
        out->set_error("prompt (" + std::to_string(prompt_tokens) +
                       " tokens) exceeds max_context_tokens (" +
                       std::to_string(max_context_tokens) + ")");
        return;
    }
    auto writer = rings.get_or_create(req.ring_name(), 0, 0);
    // BUG-004: fence this job's slots off from whatever a prior job on this
    // same (likely reused) ring_name already wrote -- see
    // RingWriter::next_generation's doc comment.
    std::uint64_t generation_id = writer.next_generation();

    // proto3 float fields default to 0.0, not 1.0, so a caller that never
    // sets repetition_penalty (every caller predating BUG-005's fix) must
    // not be read as "penalize with strength 0.0" -- that's not a real
    // disabled value (1.0 is), just an unset field; treat it as disabled.
    float repetition_penalty = req.repetition_penalty() > 0.0F ? req.repetition_penalty() : 1.0F;
    Job job{generation_id, req.prompt(),       req.max_tokens(), req.temperature(),
            req.seed(),    req.grammar_gbnf(), req.ring_name(),  repetition_penalty};
    bool accepted =
        lanes.submit(req.lane() == neuroos::v1::LANE_BACKGROUND ? neuroos::v1::LANE_BACKGROUND
                                                                : neuroos::v1::LANE_INTERACTIVE,
                     std::move(job));
    out->set_generation_id(generation_id);
    out->set_accepted(accepted);
    if (!accepted) {
        out->set_error("lane queue is full");
    }
}

void handle_distill(const neuroos::v1::DistillRequest& req, neuroos::v1::Envelope& resp,
                    LaneScheduler& lanes, RingRegistry& rings) {
    auto* out = resp.mutable_distill_response();
    if (req.ring_name().empty() || !rings.exists(req.ring_name())) {
        out->set_accepted(false);
        out->set_error("unknown or missing ring_name");
        return;
    }
    std::ostringstream prompt;
    for (const auto& chunk : req.chunks()) {
        prompt << chunk << "\n";
    }
    auto writer = rings.get_or_create(req.ring_name(), 0, 0);
    // BUG-004: same fencing as handle_generate, above.
    std::uint64_t generation_id = writer.next_generation();

    Job job{generation_id,
            prompt.str(),
            req.max_tokens(),
            /*temperature=*/0.7F,
            /*seed=*/0,
            "",
            req.ring_name(),
            /*repetition_penalty=*/1.1F};
    bool accepted = lanes.submit(neuroos::v1::LANE_BACKGROUND, std::move(job));
    out->set_generation_id(generation_id);
    out->set_accepted(accepted);
    if (!accepted) {
        out->set_error("background lane queue is full");
    }
}

void handle_connection(int fd, std::shared_ptr<Model> model, LaneScheduler& lanes,
                       RingRegistry& rings, std::uint32_t max_context_tokens) {
    for (;;) {
        auto req = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        if (!req || !req.value().has_value()) {
            break;
        }
        const neuroos::v1::Envelope& in = *req.value();
        neuroos::v1::Envelope resp;
        resp.set_schema_version(1);
        resp.set_trace_id(in.trace_id());
        resp.set_request_id(in.request_id());
        resp.set_sent_at_ns(now_ns());

        int fd_to_send = -1;
        switch (in.body_case()) {
        case neuroos::v1::Envelope::kGenerateRequest:
            handle_generate(in.generate_request(), resp, lanes, rings, *model, max_context_tokens);
            break;
        case neuroos::v1::Envelope::kCancelRequest: {
            bool cancelled = lanes.cancel(in.cancel_request().generation_id());
            resp.mutable_cancel_response()->set_cancelled(cancelled);
            break;
        }
        case neuroos::v1::Envelope::kDistillRequest:
            handle_distill(in.distill_request(), resp, lanes, rings);
            break;
        case neuroos::v1::Envelope::kAttachRingRequest: {
            const auto& areq = in.attach_ring_request();
            rings.get_or_create(areq.ring_name(), areq.capacity_slots(), areq.slot_size());
            resp.mutable_attach_ring_response()->set_ok(true);
            fd_to_send = rings.fd_for(areq.ring_name());
            break;
        }
        case neuroos::v1::Envelope::kGetInfoRequest: {
            auto info = model->info();
            auto* out = resp.mutable_get_info_response();
            out->set_model_name(info.model_name);
            out->set_context_length(info.context_length);
            out->set_vocab_size(info.vocab_size);
            out->set_build_info(info.build_info);
            if (!in.get_info_request().tokenize_text().empty()) {
                out->set_token_count(model->tokenize_count(in.get_info_request().tokenize_text()));
            }
            break;
        }
        default:
            continue; // not ours (e.g. HealthRequest goes to the separate health server)
        }

        bool wrote;
        if (fd_to_send >= 0) {
            wrote = static_cast<bool>(neuroos::ipc::write_envelope_with_fd(
                fd, resp, neuroos::ipc::kDefaultMaxFrame, fd_to_send));
        } else {
            wrote = static_cast<bool>(
                neuroos::ipc::write_envelope(fd, resp, neuroos::ipc::kDefaultMaxFrame));
        }
        if (!wrote) {
            break;
        }
    }
    ::close(fd);
}

} // namespace

void serve(const std::string& socket_path, std::vector<std::uint32_t> allowed_uids,
           std::shared_ptr<Model> model, LaneScheduler& lanes, RingRegistry& rings,
           std::uint32_t max_context_tokens) {
    auto server = neuroos::ipc::UdsServer::bind(socket_path, std::move(allowed_uids));
    if (!server) {
        spdlog::error("inference.sock bind failed: {}", server.error().message);
        return;
    }
    spdlog::info("inference.sock listening at {}", socket_path);
    for (;;) {
        auto accepted = server.value().accept();
        if (!accepted) {
            // M2: a failure that costs only this connection now returns
            // `nullopt`, so an error means the listening socket itself is
            // gone and retrying would spin at full CPU.
            spdlog::error("inference.sock listener is unusable; stopped serving: {}",
                          accepted.error().message);
            break;
        }
        if (!accepted.value().has_value()) {
            continue; // this one connection failed; keep serving
        }
        int fd = accepted.value()->first;
        std::thread(handle_connection, fd, model, std::ref(lanes), std::ref(rings),
                    max_context_tokens)
            .detach();
    }
}

} // namespace neuroos::inference
