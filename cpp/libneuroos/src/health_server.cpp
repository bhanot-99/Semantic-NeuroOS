#include "libneuroos/health_server.hpp"

#include <unistd.h>

#include <fstream>
#include <sstream>
#include <thread>

#include "libneuroos/framing.hpp"
#include "libneuroos/uds.hpp"
#include "neuroos/v1/envelope.pb.h"

namespace neuroos::health {

std::uint64_t rss_bytes() {
    std::ifstream status("/proc/self/status");
    std::string line;
    while (std::getline(status, line)) {
        if (line.rfind("VmRSS:", 0) != 0) {
            continue;
        }
        std::istringstream rest(line.substr(6));
        std::uint64_t kb = 0;
        if (rest >> kb) {
            return kb * 1024;
        }
        return 0;
    }
    return 0;
}

neuroos::v1::HealthResponse HealthServer::snapshot() const {
    neuroos::v1::HealthResponse resp;
    resp.set_status(static_cast<neuroos::v1::Status>(status_.load(std::memory_order_relaxed)));
    resp.set_rss_bytes(rss_bytes());
    resp.set_uptime_s(static_cast<std::uint64_t>(
        std::chrono::duration_cast<std::chrono::seconds>(std::chrono::steady_clock::now() - start_)
            .count()));
    resp.set_build_info(build_info_);
    {
        std::lock_guard<std::mutex> lock(hist_mutex_);
        for (const auto& [name, hist] : histograms_) {
            (*resp.mutable_latency_histograms())[name] = hist.to_proto();
        }
    }
    {
        std::lock_guard<std::mutex> lock(error_mutex_);
        for (const auto& [name, count] : error_counters_) {
            (*resp.mutable_error_counters())[name] = count;
        }
    }
    return resp;
}

namespace {

void handle_connection(int fd, const HealthServer& server) {
    for (;;) {
        auto req = neuroos::ipc::read_envelope(fd, neuroos::ipc::kDefaultMaxFrame);
        if (!req || !req.value().has_value()) {
            break;
        }
        if (req.value()->body_case() != neuroos::v1::Envelope::kHealthRequest) {
            continue;
        }
        neuroos::v1::Envelope resp;
        resp.set_schema_version(1);
        resp.set_trace_id(req.value()->trace_id());
        resp.set_request_id(req.value()->request_id());
        *resp.mutable_health_response() = server.snapshot();
        if (!neuroos::ipc::write_envelope(fd, resp, neuroos::ipc::kDefaultMaxFrame)) {
            break;
        }
    }
    ::close(fd);
}

} // namespace

void HealthServer::serve(const std::string& socket_path, std::vector<std::uint32_t> allowed_uids) {
    auto server = neuroos::ipc::UdsServer::bind(socket_path, std::move(allowed_uids));
    if (!server) {
        return; // caller's std::jthread simply exits; the component still runs without a health
                // endpoint
    }
    for (;;) {
        auto accepted = server.value().accept();
        if (!accepted) {
            // M2: `accept` now reports only an unusable *listening* socket
            // as an error -- a failure that costs one connection returns
            // `nullopt` instead -- so there is genuinely nothing left to
            // serve here. libneuroos deliberately links no logger, so the
            // component's own `serve` caller is where this gets reported.
            break;
        }
        if (!accepted.value().has_value()) {
            continue; // this one connection failed; keep serving
        }
        int fd = accepted.value()->first;
        std::thread(handle_connection, fd, std::cref(*this)).detach();
    }
}

} // namespace neuroos::health
