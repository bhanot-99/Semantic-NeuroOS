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
#include <optional>
#include <string>
#include <thread>
#include <unordered_map>

#include "engine.hpp"
#include "neuroos/v1/inference.pb.h"
#include "ring.hpp"

namespace neuroos::inference {

struct Job {
    std::uint64_t generation_id = 0; // the target ring's generation_id, captured at submit time
    std::string prompt;
    std::uint32_t max_tokens = 0;
    float temperature = 0.0F;
    std::uint64_t seed = 0;
    std::string grammar_gbnf;
    std::string ring_name;
    float repetition_penalty = 1.0F; // BUG-005: 1.0 = disabled
};

constexpr std::size_t kMaxQueueDepth = 4;

class LaneScheduler {
  public:
    LaneScheduler(Context interactive_ctx, Context background_ctx, RingRegistry& rings);
    ~LaneScheduler();
    LaneScheduler(const LaneScheduler&) = delete;
    LaneScheduler& operator=(const LaneScheduler&) = delete;
    // Owns two llama contexts and a worker thread per lane: not movable either.
    LaneScheduler(LaneScheduler&&) = delete;
    LaneScheduler& operator=(LaneScheduler&&) = delete;

    // Returns false (caller reports !accepted) if the target lane's bounded
    // queue is already full.
    bool submit(neuroos::v1::Lane lane, Job job);

    // Finds `generation_id` whether queued or currently running (on either
    // lane) and cancels it: a job that has not started yet is removed from
    // its lane's queue (M12 — it used to be left in place, so the whole
    // prompt was still prefilled and decoded for an answer nobody was
    // waiting for), and a running job gets its cancel flag set, which the
    // engine checks once per decode step (so it stops within one step —
    // FR-INF-05). Either way the job's stream ends with a
    // kFlagEos|kFlagCancel slot (H8), so a reader never waits out its
    // deadline. Returns true iff `generation_id` was found.
    //
    // It does *not* bump the ring's generation counter: since H7 each slot
    // carries the generation of the job that wrote it and readers filter on
    // their own, so bumping the header would only renumber the *next* job.
    bool cancel(std::uint64_t generation_id);

  private:
    struct QueueEntry {
        Job job;
        std::shared_ptr<std::atomic<bool>> cancelled;
    };

    void worker_loop(std::deque<QueueEntry>& queue, std::mutex& queue_mutex,
                     std::condition_variable& queue_cv, Context& ctx, bool is_interactive,
                     const std::function<bool()>& extra_yield_check);

    // Removes `generation_id` from `queue` if it is still waiting there,
    // returning the entry so the caller can end its stream. `queue_mutex`
    // is taken here and never while `generations_mutex_` is held, matching
    // `submit`'s order (queue first).
    std::optional<QueueEntry> take_queued(std::deque<QueueEntry>& queue, std::mutex& queue_mutex,
                                          bool is_interactive, std::uint64_t generation_id);

    // interactive_active_ = a job is running on the interactive lane, or
    // one is waiting in its queue. Call with interactive_mutex_ held.
    void refresh_interactive_active();

    // Ends `generation_id`'s stream on `ring_name` with one terminal slot
    // (H8). Every write into a ring goes through `ring_mutex_`: a ring name
    // is shared, so a worker thread and a cancelling server thread can
    // otherwise write the same ring at once, which the single-writer
    // seqlock does not allow.
    void write_terminal(const std::string& ring_name, std::uint64_t generation_id,
                        std::uint16_t flags);

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
    // Guarded by interactive_mutex_; feeds refresh_interactive_active().
    bool interactive_running_{false};

    std::mutex ring_mutex_;

    std::mutex generations_mutex_;
    std::unordered_map<std::uint64_t, std::shared_ptr<std::atomic<bool>>> generations_;

    std::atomic<bool> shutdown_{false};
    std::thread interactive_thread_;
    std::thread background_thread_;
};

} // namespace neuroos::inference
