#include "lanes.hpp"

#include <spdlog/spdlog.h>

#include <chrono>

namespace neuroos::inference {

LaneScheduler::LaneScheduler(Context interactive_ctx, Context background_ctx, RingRegistry& rings)
    : rings_(rings), interactive_ctx_(std::move(interactive_ctx)),
      background_ctx_(std::move(background_ctx)) {
    interactive_thread_ = std::thread([this] {
        worker_loop(interactive_queue_, interactive_mutex_, interactive_cv_, interactive_ctx_,
                    /*is_interactive=*/true, /*extra_yield_check=*/[] { return false; });
    });
    background_thread_ = std::thread([this] {
        worker_loop(background_queue_, background_mutex_, background_cv_, background_ctx_,
                    /*is_interactive=*/false, /*extra_yield_check=*/[this] {
                        // Yield (block, don't abort): the interactive lane's
                        // job is prioritized. Context state (KV cache,
                        // resident_tokens_) is untouched by this pause, so
                        // generate() picks up exactly where it left off —
                        // PRD FR-INF-06, "background... yields between
                        // decode steps".
                        while (interactive_active_.load(std::memory_order_acquire) &&
                               !shutdown_.load(std::memory_order_acquire)) {
                            std::this_thread::sleep_for(std::chrono::milliseconds(2));
                        }
                        return false; // never itself a cancel; real cancels come via `cancelled`
                    });
    });
}

LaneScheduler::~LaneScheduler() {
    shutdown_.store(true, std::memory_order_release);
    interactive_cv_.notify_all();
    background_cv_.notify_all();
    if (interactive_thread_.joinable()) {
        interactive_thread_.join();
    }
    if (background_thread_.joinable()) {
        background_thread_.join();
    }
}

bool LaneScheduler::submit(neuroos::v1::Lane lane, Job job) {
    // Only register the job in `generations_` once it's actually queued
    // (not on a rejected/full-queue path), so there's never a window where
    // an entry exists but no queue slot backs it — nothing to erase-on-
    // failure, so no race there either.
    auto cancelled = std::make_shared<std::atomic<bool>>(false);

    if (lane == neuroos::v1::LANE_BACKGROUND) {
        std::lock_guard<std::mutex> lock(background_mutex_);
        if (background_queue_.size() >= kMaxQueueDepth) {
            return false;
        }
        {
            std::lock_guard<std::mutex> gen_lock(generations_mutex_);
            generations_[job.generation_id] = cancelled;
        }
        background_queue_.push_back(QueueEntry{std::move(job), std::move(cancelled)});
        background_cv_.notify_one();
        return true;
    }

    std::lock_guard<std::mutex> lock(interactive_mutex_);
    if (interactive_queue_.size() >= kMaxQueueDepth) {
        return false;
    }
    {
        std::lock_guard<std::mutex> gen_lock(generations_mutex_);
        generations_[job.generation_id] = cancelled;
    }
    interactive_queue_.push_back(QueueEntry{std::move(job), std::move(cancelled)});
    refresh_interactive_active();
    interactive_cv_.notify_one();
    return true;
}

std::optional<LaneScheduler::QueueEntry> LaneScheduler::take_queued(std::deque<QueueEntry>& queue,
                                                                    std::mutex& queue_mutex,
                                                                    bool is_interactive,
                                                                    std::uint64_t generation_id) {
    std::lock_guard<std::mutex> lock(queue_mutex);
    for (auto it = queue.begin(); it != queue.end(); ++it) {
        if (it->job.generation_id != generation_id) {
            continue;
        }
        QueueEntry entry = std::move(*it);
        queue.erase(it);
        if (is_interactive) {
            refresh_interactive_active();
        }
        return entry;
    }
    return std::nullopt;
}

void LaneScheduler::refresh_interactive_active() {
    interactive_active_.store(interactive_running_ || !interactive_queue_.empty(),
                              std::memory_order_release);
}

void LaneScheduler::write_terminal(const std::string& ring_name, std::uint64_t generation_id,
                                   std::uint16_t flags) {
    // L3/ADR-0013: the ring was attached before the job was accepted, so
    // this is a lookup and never a creation. A miss means the client
    // detached its ring, so there is nowhere to put the terminal slot.
    auto sink = rings_.get(ring_name);
    if (!sink) {
        spdlog::warn("no ring for generation {}; end-of-stream slot not written", generation_id);
        return;
    }
    std::lock_guard<std::mutex> lock(ring_mutex_);
    if (!sink->write_split(generation_id, 0, flags, nullptr, 0)) {
        spdlog::warn("failed to write the end-of-stream slot for generation {}", generation_id);
    }
}

bool LaneScheduler::cancel(std::uint64_t generation_id) {
    // M12: a job still waiting in a queue is taken out of it, so neither
    // its prompt prefill nor a single decode step ever runs. The queue
    // mutexes are taken first and never under generations_mutex_, matching
    // `submit`'s order.
    auto queued =
        take_queued(interactive_queue_, interactive_mutex_, /*is_interactive=*/true, generation_id);
    if (!queued.has_value()) {
        queued = take_queued(background_queue_, background_mutex_, /*is_interactive=*/false,
                             generation_id);
    }

    std::shared_ptr<std::atomic<bool>> flag;
    {
        std::lock_guard<std::mutex> lock(generations_mutex_);
        auto it = generations_.find(generation_id);
        if (it == generations_.end()) {
            // Only reachable for a job that finished between the lookup in
            // `server.cpp` and here; a dequeued job is always registered.
            return queued.has_value();
        }
        flag = it->second;
        if (queued.has_value() && it->second == queued->cancelled) {
            // Nothing will run it now, so nothing else will clean this up.
            generations_.erase(it);
        }
    }
    // Set even for a dequeued job: `submit` handed the same shared flag to
    // this scheduler's bookkeeping, and any reader of it must see the cancel.
    flag->store(true, std::memory_order_release);
    if (queued.has_value()) {
        // The worker that would have ended this stream never sees the job,
        // so the terminal slot (H8) is written here instead.
        write_terminal(
            queued->job.ring_name, queued->job.generation_id,
            static_cast<std::uint16_t>(neuroos::shm::kFlagEos | neuroos::shm::kFlagCancel));
    }
    return true;
}

void LaneScheduler::worker_loop(std::deque<QueueEntry>& queue, std::mutex& queue_mutex,
                                std::condition_variable& queue_cv, Context& ctx,
                                bool is_interactive,
                                const std::function<bool()>& extra_yield_check) {
    for (;;) {
        QueueEntry entry;
        {
            std::unique_lock<std::mutex> lock(queue_mutex);
            queue_cv.wait(lock, [&] { return !queue.empty() || shutdown_.load(); });
            if (shutdown_.load(std::memory_order_acquire) && queue.empty()) {
                return;
            }
            entry = std::move(queue.front());
            queue.pop_front();
            if (is_interactive) {
                interactive_running_ = true;
                refresh_interactive_active();
            }
        }

        // L3/ADR-0013: as in `write_terminal`, a lookup and never a
        // creation. A miss means the client detached its ring, so there is
        // nowhere to put this job's tokens and it is dropped with a log
        // rather than generated into a ring nobody reads.
        auto sink_opt = rings_.get(entry.job.ring_name);
        if (!sink_opt) {
            spdlog::warn("generation {} dropped: no ring to write into", entry.job.generation_id);
            if (is_interactive) {
                std::lock_guard<std::mutex> lock(interactive_mutex_);
                interactive_running_ = false;
                refresh_interactive_active();
            }
            continue;
        }
        auto& sink = *sink_opt;
        int tokens_produced = 0;
        bool eos_written = false;
        auto should_cancel = [&entry, &extra_yield_check, this] {
            if (shutdown_.load(std::memory_order_acquire)) {
                return true;
            }
            if (extra_yield_check()) {
                return true;
            }
            return entry.cancelled->load(std::memory_order_acquire);
        };

        auto result = ctx.generate(
            entry.job.prompt, entry.job.max_tokens, entry.job.temperature, entry.job.seed,
            entry.job.grammar_gbnf, entry.job.repetition_penalty,
            [&sink, &tokens_produced, &eos_written, &entry, this](const GeneratedToken& token) {
                ++tokens_produced;
                std::uint16_t flags = token.is_eos ? neuroos::shm::kFlagEos : 0;
                eos_written = eos_written || token.is_eos;
                // M12: a piece wider than one slot's payload used to be
                // dropped here without a trace (`write_as` returns false);
                // `write_split` spreads it over consecutive slots, which
                // the reader concatenates. A failure now at least says so.
                std::lock_guard<std::mutex> lock(ring_mutex_);
                if (!sink.write_split(entry.job.generation_id, token.token_id, flags,
                                      reinterpret_cast<const std::uint8_t*>(token.text.data()),
                                      token.text.size())) {
                    spdlog::warn("dropped a token piece of generation {}: ring write failed",
                                 entry.job.generation_id);
                }
            },
            should_cancel);
        // H8: every job's stream ends with an end-of-stream slot -- also
        // when it stopped at max_tokens or the context limit, was
        // cancelled, or failed -- so a reader never waits out its deadline
        // for a token that will never come.
        if (!eos_written) {
            std::uint16_t flags = neuroos::shm::kFlagEos;
            if (entry.cancelled->load(std::memory_order_acquire)) {
                flags |= neuroos::shm::kFlagCancel;
            }
            write_terminal(entry.job.ring_name, entry.job.generation_id, flags);
        }
        spdlog::debug("generation {} produced {} tokens, ok={}", entry.job.generation_id,
                      tokens_produced, static_cast<bool>(result));
        if (!result) {
            spdlog::warn("generation {} failed: {}", entry.job.generation_id,
                         result.error().message);
        }

        {
            // generation_id is now bumped per-job (server.cpp's
            // `RingWriter::next_generation`, BUG-004), so this collision is
            // rare rather than routine, but still defensive: a blind
            // `erase(generation_id)` here could delete a *different*,
            // still-running job's cancel flag if one happened to reuse this
            // id (e.g. after the counter wraps) and was inserted into
            // `generations_` between this job finishing and this cleanup
            // running — found via cpp_inference_smoke's real Cancel test
            // racing a real interactive job's tail latency against a
            // background job on the same ring. Only erase the entry if it's
            // still exactly the one this job registered.
            std::lock_guard<std::mutex> lock(generations_mutex_);
            auto it = generations_.find(entry.job.generation_id);
            if (it != generations_.end() && it->second == entry.cancelled) {
                generations_.erase(it);
            }
        }
        if (is_interactive) {
            std::lock_guard<std::mutex> lock(interactive_mutex_);
            interactive_running_ = false;
            refresh_interactive_active();
        }
    }
}

} // namespace neuroos::inference
