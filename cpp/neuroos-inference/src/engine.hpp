// BitNet inference engine wrapper (PRD FR-INF-01..07). Wraps llama.cpp's C
// API (cpp/third_party/bitnet.cpp/3rdparty/llama.cpp/include/llama.h) —
// BitNet's own fork, with i2_s quantization + AVX2 kernels ADR-0006's spike
// proved live on the reference machine (17.9 t/s decode, 2.41B params).
#pragma once

#include <atomic>
#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <vector>

#include "libneuroos/expected.hpp"
#include "llama.h"

namespace neuroos::inference {

struct EngineError {
    std::string message;
};

struct ModelInfo {
    std::string model_name;
    std::uint32_t context_length;
    std::uint64_t vocab_size;
    std::string build_info;
};

struct GeneratedToken {
    std::uint32_t token_id;
    std::string text; // detokenized UTF-8 piece
    bool is_eos;
};

// Owns the loaded model weights (read-only mmap — FR-INF-01). Shared,
// read-only across every `Context` (one per lane); llama_model itself is
// safe to use concurrently from multiple contexts once loaded.
class Model {
  public:
    static neuroos::Expected<std::shared_ptr<Model>, EngineError>
    load(const std::string& model_path, const std::string& expected_sha256);

    ~Model();
    Model(const Model&) = delete;
    Model& operator=(const Model&) = delete;

    ModelInfo info() const;
    const llama_vocab* vocab() const {
        return vocab_;
    }
    llama_model* raw() const {
        return model_;
    }
    std::uint32_t tokenize_count(const std::string& text) const;

  private:
    explicit Model(llama_model* model);

    llama_model* model_ = nullptr;
    const llama_vocab* vocab_ = nullptr;
};

// One lane's decode state: its own llama_context (own KV cache), reused
// across requests. Not thread-safe — each lane (lanes.cpp) owns exactly one
// Context and calls it from a single worker thread.
class Context {
  public:
    static neuroos::Expected<Context, EngineError>
    create(std::shared_ptr<Model> model, std::uint32_t n_ctx, std::uint32_t n_threads);
    ~Context();
    Context(Context&& other) noexcept;
    Context& operator=(Context&& other) noexcept;
    Context(const Context&) = delete;
    Context& operator=(const Context&) = delete;

    // Tokenizes `prompt`, reuses whatever leading tokens are already
    // resident in this context's KV cache from a prior call (Architecture.md
    // §9.1's "prompt-prefix KV cache for the 128-token system block" lever,
    // generalized: whatever prefix happens to repeat between consecutive
    // requests on this lane is reused, not just a hardcoded 128 tokens),
    // decodes the rest, then samples up to `max_tokens` more, calling
    // `on_token` for each one. Stops early if `should_cancel()` returns true
    // between decode steps (checked every step, so cancellation and
    // background-lane preemption both bound to "at most one decode step" —
    // PRD FR-INF-05/FR-INF-06).
    //
    // `grammar_gbnf` empty => unconstrained sampling. `temperature` 0.0 =>
    // greedy. `seed` 0 => a random seed.
    neuroos::Expected<void, EngineError>
    generate(const std::string& prompt, std::uint32_t max_tokens, float temperature,
             std::uint64_t seed, const std::string& grammar_gbnf,
             const std::function<void(const GeneratedToken&)>& on_token,
             const std::function<bool()>& should_cancel);

    std::uint32_t n_ctx() const {
        return llama_n_ctx(ctx_);
    }

  private:
    Context(std::shared_ptr<Model> model, llama_context* ctx, std::uint32_t n_ctx);

    std::shared_ptr<Model> model_;
    llama_context* ctx_ = nullptr;
    std::vector<llama_token> resident_tokens_; // what's currently in this ctx's KV cache, seq 0
};

} // namespace neuroos::inference
