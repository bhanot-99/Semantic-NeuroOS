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
    interactive_active_.store(true, std::memory_order_release);
    interactive_queue_.push_back(QueueEntry{std::move(job), std::move(cancelled)});
    interactive_cv_.notify_one();
    return true;
}

bool LaneScheduler::cancel(std::uint64_t generation_id) {
    std::shared_ptr<std::atomic<bool>> flag;
    {
        std::lock_guard<std::mutex> lock(generations_mutex_);
        auto it = generations_.find(generation_id);
        if (it == generations_.end()) {
            return false;
        }
        flag = it->second;
    }
    flag->store(true, std::memory_order_release);
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
        }

        auto sink =
            rings_.get_or_create(entry.job.ring_name, /*capacity_slots=*/0, /*slot_size=*/0);
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
            [&sink, &tokens_produced, &eos_written, &entry](const GeneratedToken& token) {
                ++tokens_produced;
                std::uint16_t flags = token.is_eos ? neuroos::shm::kFlagEos : 0;
                eos_written = eos_written || token.is_eos;
                sink.write_as(entry.job.generation_id, token.token_id, flags,
                              reinterpret_cast<const std::uint8_t*>(token.text.data()),
                              token.text.size());
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
            sink.write_as(entry.job.generation_id, 0, flags, nullptr, 0);
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
            if (interactive_queue_.empty()) {
                interactive_active_.store(false, std::memory_order_release);
            }
        }
    }
}

} // namespace neuroos::inference
