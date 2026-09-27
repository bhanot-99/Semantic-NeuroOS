// Two priority lanes (PRD FR-INF-06): interactive preempts, background
// yields between decode steps. Each lane is one worker thread with its own
// `Context` (own KV cache) over the same shared `Model`; jobs within a lane
// run one at a time, in submission order (bounded queue per lane).
#pragma once

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>

#include "engine.hpp"
#include "neuroos/v1/inference.pb.h"
#include "ring.hpp"

namespace neuroos::inference {

struct Job {
    std::uint64_t generation_id; // the target ring's generation_id, captured at submit time
    std::string prompt;
    std::uint32_t max_tokens;
    float temperature;
    std::uint64_t seed;
    std::string grammar_gbnf;
    std::string ring_name;
};

constexpr std::size_t kMaxQueueDepth = 4;

class LaneScheduler {
  public:
    LaneScheduler(Context interactive_ctx, Context background_ctx, RingRegistry& rings);
    ~LaneScheduler();
    LaneScheduler(const LaneScheduler&) = delete;
    LaneScheduler& operator=(const LaneScheduler&) = delete;

    // Returns false (caller reports !accepted) if the target lane's bounded
    // queue is already full.
    bool submit(neuroos::v1::Lane lane, Job job);

    // Finds `generation_id` whether queued or currently running (on either
    // lane) and cancels it: dequeues it if not started, or sets its cancel
    // flag (checked once per decode step, so the engine stops within one
    // step — FR-INF-05) and bumps the ring's own generation counter (so the
    // reader discards any already-written, now-stale slots) if running.
    // Returns true iff `generation_id` was found.
    bool cancel(std::uint64_t generation_id);

  private:
    struct QueueEntry {
        Job job;
        std::shared_ptr<std::atomic<bool>> cancelled;
    };

    void worker_loop(std::deque<QueueEntry>& queue, std::mutex& queue_mutex,
                     std::condition_variable& queue_cv, Context& ctx, bool is_interactive,
                     const std::function<bool()>& extra_yield_check);

    RingRegistry& rings_;
    Context interactive_ctx_;
    Context background_ctx_;

    std::mutex interactive_mutex_;
    std::condition_variable interactive_cv_;
    std::deque<QueueEntry> interactive_queue_;

    std::mutex background_mutex_;
    std::condition_variable background_cv_;
    std::deque<QueueEntry> background_queue_;

    // Set for the duration of any interactive job (queued or running) so
    // the background worker's yield check can block between its own decode
    // steps without losing generation state (Context's KV cache persists
    // across the pause — no re-tokenization, no lost work).
    std::atomic<bool> interactive_active_{false};

    std::mutex generations_mutex_;
    std::unordered_map<std::uint64_t, std::shared_ptr<std::atomic<bool>>> generations_;

    std::atomic<bool> shutdown_{false};
    std::thread interactive_thread_;
    std::thread background_thread_;
};

} // namespace neuroos::inference
