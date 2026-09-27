#include "engine.hpp"

#include <spdlog/spdlog.h>

#include <algorithm>
#include <memory>
#include <mutex>
#include <random>

#include "grammar.hpp"
#include "libneuroos/sha256.hpp"

namespace neuroos::inference {

namespace {

void ensure_backend_initialized() {
    static std::once_flag once;
    std::call_once(once, [] { llama_backend_init(); });
}

// llama.cpp's own vocab getter takes a size hint via context, but token->text
// pieces are typically small; grow and retry once if a piece is unusually large.
std::string token_to_text(const llama_vocab* vocab, llama_token token) {
    char buf[256];
    int n = llama_token_to_piece(vocab, token, buf, sizeof(buf), /*lstrip=*/0, /*special=*/false);
    if (n < 0) {
        std::vector<char> big(static_cast<std::size_t>(-n));
        n = llama_token_to_piece(vocab, token, big.data(), static_cast<int>(big.size()), 0, false);
        if (n < 0) {
            return {};
        }
        return std::string(big.data(), static_cast<std::size_t>(n));
    }
    return std::string(buf, static_cast<std::size_t>(n));
}

std::vector<llama_token> tokenize(const llama_vocab* vocab, const std::string& text,
                                  bool add_special) {
    int n_max = static_cast<int>(text.size()) + 16;
    std::vector<llama_token> tokens(static_cast<std::size_t>(n_max));
    int n = llama_tokenize(vocab, text.data(), static_cast<int>(text.size()), tokens.data(), n_max,
                           add_special, /*parse_special=*/true);
    if (n < 0) {
        tokens.resize(static_cast<std::size_t>(-n));
        n = llama_tokenize(vocab, text.data(), static_cast<int>(text.size()), tokens.data(),
                           static_cast<int>(tokens.size()), add_special, true);
    }
    tokens.resize(static_cast<std::size_t>(std::max(n, 0)));
    return tokens;
}

} // namespace

Model::Model(llama_model* model) : model_(model), vocab_(llama_model_get_vocab(model)) {}

Model::~Model() {
    if (model_ != nullptr) {
        llama_model_free(model_);
    }
}

neuroos::Expected<std::shared_ptr<Model>, EngineError>
Model::load(const std::string& model_path, const std::string& expected_sha256) {
    ensure_backend_initialized();

    if (!expected_sha256.empty()) {
        auto digest = neuroos::crypto::sha256_file(model_path);
        if (!digest) {
            return make_unexpected(
                EngineError{"cannot hash model file: " + digest.error().message});
        }
        if (*digest != expected_sha256) {
            return make_unexpected(
                EngineError{"model file " + model_path + " sha256 mismatch: expected " +
                            expected_sha256 + ", got " + *digest +
                            " (models/manifest.toml entry doesn't match the file on disk — "
                            "re-run scripts/fetch-models.sh)"});
        }
        spdlog::info("model {} sha256 verified", model_path);
    } else {
        spdlog::warn("loading model {} without a sha256 check (no expected hash given)",
                     model_path);
    }

    llama_model_params params = llama_model_default_params();
    params.use_mmap = true; // FR-INF-01: read-only mmap, not a full read into RAM
    params.use_mlock = false;

    llama_model* model = llama_model_load_from_file(model_path.c_str(), params);
    if (model == nullptr) {
        return make_unexpected(EngineError{"llama_model_load_from_file failed for " + model_path});
    }
    return std::shared_ptr<Model>(new Model(model));
}

ModelInfo Model::info() const {
    char desc[256] = {0};
    llama_model_desc(model_, desc, sizeof(desc));
    return ModelInfo{
        .model_name = desc,
        .context_length = static_cast<std::uint32_t>(llama_model_n_ctx_train(model_)),
        .vocab_size = static_cast<std::uint64_t>(llama_vocab_n_tokens(vocab_)),
        .build_info = "neuroos-inference (BitNet b1.58, llama.cpp fork)",
    };
}

std::uint32_t Model::tokenize_count(const std::string& text) const {
    return static_cast<std::uint32_t>(tokenize(vocab_, text, /*add_special=*/false).size());
}

Context::Context(std::shared_ptr<Model> model, llama_context* ctx, std::uint32_t /*n_ctx*/)
    : model_(std::move(model)), ctx_(ctx) {}

Context::~Context() {
    if (ctx_ != nullptr) {
        llama_free(ctx_);
    }
}

Context::Context(Context&& other) noexcept
    : model_(std::move(other.model_)), ctx_(other.ctx_),
      resident_tokens_(std::move(other.resident_tokens_)) {
    other.ctx_ = nullptr;
}

Context& Context::operator=(Context&& other) noexcept {
    if (this != &other) {
        if (ctx_ != nullptr) {
            llama_free(ctx_);
        }
        model_ = std::move(other.model_);
        ctx_ = other.ctx_;
        resident_tokens_ = std::move(other.resident_tokens_);
        other.ctx_ = nullptr;
    }
    return *this;
}

neuroos::Expected<Context, EngineError>
Context::create(std::shared_ptr<Model> model, std::uint32_t n_ctx, std::uint32_t n_threads) {
    llama_context_params params = llama_context_default_params();
    params.n_ctx = n_ctx;
    params.n_batch = n_ctx;
    params.n_threads = static_cast<std::int32_t>(n_threads);
    params.n_threads_batch = static_cast<std::int32_t>(n_threads);

    llama_context* ctx = llama_init_from_model(model->raw(), params);
    if (ctx == nullptr) {
        return make_unexpected(EngineError{"llama_init_from_model failed"});
    }
    return Context(std::move(model), ctx, n_ctx);
}

neuroos::Expected<void, EngineError>
Context::generate(const std::string& prompt, std::uint32_t max_tokens, float temperature,
                  std::uint64_t seed, const std::string& grammar_gbnf,
                  const std::function<void(const GeneratedToken&)>& on_token,
                  const std::function<bool()>& should_cancel) {
    const llama_vocab* vocab = model_->vocab();
    std::vector<llama_token> prompt_tokens = tokenize(vocab, prompt, /*add_special=*/true);
    if (prompt_tokens.size() > n_ctx()) {
        return make_unexpected(EngineError{"prompt (" + std::to_string(prompt_tokens.size()) +
                                           " tokens) exceeds context size (" +
                                           std::to_string(n_ctx()) + ")"});
    }

    // KV-cache prefix reuse (Architecture.md §9.1's "prompt-prefix KV cache
    // for the 128-token system block" lever, generalized to whatever prefix
    // this lane's last request and this one actually share): find the
    // common prefix with what's already resident, drop everything in the
    // KV cache after that point, and only decode the new suffix.
    std::size_t common = 0;
    while (common < resident_tokens_.size() && common < prompt_tokens.size() &&
           resident_tokens_[common] == prompt_tokens[common]) {
        ++common;
    }
    llama_memory_t mem = llama_get_memory(ctx_);
    if (common < resident_tokens_.size()) {
        llama_memory_seq_rm(mem, /*seq_id=*/0, static_cast<llama_pos>(common), -1);
    }
    resident_tokens_.resize(common);

    if (prompt_tokens.size() > common) {
        std::vector<llama_token> suffix(prompt_tokens.begin() + static_cast<long>(common),
                                        prompt_tokens.end());
        llama_batch batch =
            llama_batch_get_one(suffix.data(), static_cast<std::int32_t>(suffix.size()));
        // positions must be set explicitly since we're resuming at `common`,
        // not decoding from position 0 (llama_batch_get_one assumes seq 0
        // starting fresh unless the context already tracks position — it
        // does, via the memory's sequence position, so no manual pos array
        // is needed here: llama_decode continues from wherever seq 0 left
        // off after the llama_memory_seq_rm above).
        int rc = llama_decode(ctx_, batch);
        if (rc != 0) {
            return make_unexpected(
                EngineError{"llama_decode (prompt) failed, rc=" + std::to_string(rc)});
        }
        resident_tokens_.insert(resident_tokens_.end(), suffix.begin(), suffix.end());
    }

    llama_sampler_chain_params sparams = llama_sampler_chain_default_params();
    std::unique_ptr<llama_sampler, void (*)(llama_sampler*)> chain(
        llama_sampler_chain_init(sparams), &llama_sampler_free);
    auto grammar = grammar_gbnf.empty() ? nullptr : build_grammar_sampler(vocab, grammar_gbnf);
    if (!grammar_gbnf.empty() && grammar == nullptr) {
        return make_unexpected(EngineError{"failed to parse grammar_gbnf"});
    }
    if (grammar != nullptr) {
        llama_sampler_chain_add(chain.get(), grammar); // chain now owns it
    }
    if (temperature <= 0.0F) {
        llama_sampler_chain_add(chain.get(), llama_sampler_init_greedy());
    } else {
        llama_sampler_chain_add(chain.get(), llama_sampler_init_temp(temperature));
        llama_sampler_chain_add(chain.get(),
                                llama_sampler_init_dist(seed == 0 ? std::random_device{}() : seed));
    }

    for (std::uint32_t generated = 0; generated < max_tokens; ++generated) {
        if (should_cancel()) {
            break; // FR-INF-05/06: bounded to at most one decode step
        }
        llama_token next = llama_sampler_sample(chain.get(), ctx_, -1);
        llama_sampler_accept(chain.get(), next);

        bool eos = llama_vocab_is_eog(vocab, next);
        GeneratedToken piece{static_cast<std::uint32_t>(next), token_to_text(vocab, next), eos};
        on_token(piece);
        if (eos) {
            break;
        }

        resident_tokens_.push_back(next);
        llama_batch next_batch = llama_batch_get_one(&next, 1);
        int rc = llama_decode(ctx_, next_batch);
        if (rc != 0) {
            return make_unexpected(
                EngineError{"llama_decode (decode step) failed, rc=" + std::to_string(rc)});
        }
    }
    return {};
}

} // namespace neuroos::inference
