// Fixed-bucket latency histogram, log-scale in nanoseconds. Mirrors
// crates/neuroos-health/src/histogram.rs (same bucket boundaries, so a
// LatencyHistogram from a C++ component is shaped exactly like one from a
// Rust component — neuroosctl's percentile code doesn't care which language
// produced it).
#pragma once

#include <array>
#include <chrono>
#include <cstdint>
#include <mutex>
#include <vector>

#include "neuroos/v1/health.pb.h"

namespace neuroos::health {

inline constexpr std::array<std::uint64_t, 15> kBucketUpperBoundsNs = {
    1'000ULL,
    5'000ULL,
    10'000ULL,
    50'000ULL,
    100'000ULL,
    500'000ULL,
    1'000'000ULL,
    5'000'000ULL,
    10'000'000ULL,
    50'000'000ULL,
    100'000'000ULL,
    500'000'000ULL,
    1'000'000'000ULL,
    10'000'000'000ULL,
    0xFFFF'FFFF'FFFF'FFFFULL, // overflow bucket
};

class Histogram {
  public:
    Histogram() : counts_(kBucketUpperBoundsNs.size(), 0) {}

    void record(std::chrono::nanoseconds d) {
        std::uint64_t ns = static_cast<std::uint64_t>(d.count() < 0 ? 0 : d.count());
        std::size_t idx = kBucketUpperBoundsNs.size() - 1;
        for (std::size_t i = 0; i < kBucketUpperBoundsNs.size(); ++i) {
            if (ns <= kBucketUpperBoundsNs[i]) {
                idx = i;
                break;
            }
        }
        std::lock_guard<std::mutex> lock(mutex_);
        counts_[idx] += 1;
        count_ += 1;
        sum_ns_ += ns;
    }

    neuroos::v1::LatencyHistogram to_proto() const {
        neuroos::v1::LatencyHistogram out;
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto b : kBucketUpperBoundsNs) {
            out.add_bucket_upper_bound_ns(b);
        }
        for (auto c : counts_) {
            out.add_bucket_counts(c);
        }
        out.set_count(count_);
        out.set_sum_ns(sum_ns_);
        return out;
    }

  private:
    mutable std::mutex mutex_;
    std::vector<std::uint64_t> counts_;
    std::uint64_t count_ = 0;
    std::uint64_t sum_ns_ = 0;
};

} // namespace neuroos::health
