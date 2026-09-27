// Health endpoint server, C++ side (Architecture.md §6.5). Mirrors
// crates/neuroos-health: any component gets a health endpoint by starting
// one of these on a background thread. Serves HealthRequest -> HealthResponse
// over a UDS socket, SO_PEERCRED-checked against `allowed_uids` (healthd's
// UID, in practice).
#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#include "libneuroos/histogram.hpp"
#include "neuroos/v1/health.pb.h"

namespace neuroos::health {

std::uint64_t rss_bytes(); // 0 if /proc/self/status is unreadable

class HealthServer {
  public:
    explicit HealthServer(std::string build_info)
        : build_info_(std::move(build_info)), start_(std::chrono::steady_clock::now()) {}

    void set_status(neuroos::v1::Status status) {
        status_.store(static_cast<int>(status), std::memory_order_relaxed);
    }
    void incr_error(const std::string& name) {
        std::lock_guard<std::mutex> lock(error_mutex_);
        error_counters_[name] += 1;
    }
    void record_latency(const std::string& name, std::chrono::nanoseconds d) {
        Histogram* hist = nullptr;
        {
            std::lock_guard<std::mutex> lock(hist_mutex_);
            hist = &histograms_[name]; // default-constructs on first use
        }
        hist->record(d);
    }

    neuroos::v1::HealthResponse snapshot() const;

    // Blocks the calling thread forever (or until a fatal accept() error),
    // spawning one detached thread per connection. Call this from its own
    // std::jthread, not the main service loop.
    void serve(const std::string& socket_path, std::vector<std::uint32_t> allowed_uids);

  private:
    std::string build_info_;
    std::chrono::steady_clock::time_point start_;
    std::atomic<int> status_{static_cast<int>(neuroos::v1::STATUS_OK)};
    mutable std::mutex hist_mutex_;
    std::unordered_map<std::string, Histogram> histograms_;
    mutable std::mutex error_mutex_;
    std::unordered_map<std::string, std::uint64_t> error_counters_;
};

} // namespace neuroos::health
