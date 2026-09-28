#include "bench.hpp"

#include <spdlog/spdlog.h>

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <vector>

#include "engine.hpp"
#include "libneuroos/health_server.hpp"

namespace neuroos::inference {

namespace {

struct BenchResult {
    std::uint32_t n_ctx;
    std::uint32_t n_threads;
    std::uint32_t prompt_tokens;
    double ttft_ms;
    double prefill_ms_per_token;
    double decode_ms_per_token;
    std::uint64_t rss_bytes_after;
    std::string error; // non-empty on failure; other fields are 0 in that case
};

// A prompt long enough to tokenize to at least `target_tokens`, built from a
// repeating filler sentence (this measures real decode cost against real
// token embeddings, not a hand-picked "nice" prompt).
std::string filler_prompt(const Model& model, std::uint32_t target_tokens) {
    static const std::string kSentence = "The quick brown fox jumps over the lazy dog. ";
    std::string prompt;
    while (model.tokenize_count(prompt) < target_tokens) {
        prompt += kSentence;
    }
    return prompt;
}

BenchResult bench_one(const std::shared_ptr<Model>& model, std::uint32_t n_ctx,
                      std::uint32_t n_threads) {
    BenchResult result{n_ctx, n_threads, 0, 0.0, 0.0, 0.0, 0, ""};

    auto ctx_result = Context::create(model, n_ctx, n_threads);
    if (!ctx_result) {
        result.error = ctx_result.error().message;
        return result;
    }
    Context ctx = std::move(ctx_result.value());

    // Leave headroom for the generated tokens themselves within n_ctx.
    std::string prompt = filler_prompt(*model, n_ctx > 64 ? n_ctx - 64 : n_ctx / 2);
    result.prompt_tokens = model->tokenize_count(prompt);

    constexpr std::uint32_t kGenerateTokens = 16;
    std::vector<std::chrono::steady_clock::time_point> timestamps;
    timestamps.reserve(kGenerateTokens);
    auto start = std::chrono::steady_clock::now();

    auto gen_result = ctx.generate(
        prompt, kGenerateTokens, /*temperature=*/0.0F, /*seed=*/0, /*grammar_gbnf=*/"",
        /*repetition_penalty=*/1.0F,
        [&timestamps](const GeneratedToken&) {
            timestamps.push_back(std::chrono::steady_clock::now());
        },
        [] { return false; });
    if (!gen_result) {
        result.error = gen_result.error().message;
        return result;
    }
    if (timestamps.empty()) {
        result.error = "generated zero tokens (unexpected immediate EOS)";
        return result;
    }

    result.ttft_ms = std::chrono::duration<double, std::milli>(timestamps[0] - start).count();
    result.prefill_ms_per_token =
        result.prompt_tokens > 0 ? result.ttft_ms / result.prompt_tokens : 0.0;

    double decode_total_ms = 0.0;
    std::size_t decode_steps = 0;
    for (std::size_t i = 1; i < timestamps.size(); ++i) {
        decode_total_ms +=
            std::chrono::duration<double, std::milli>(timestamps[i] - timestamps[i - 1]).count();
        ++decode_steps;
    }
    result.decode_ms_per_token =
        decode_steps > 0 ? decode_total_ms / static_cast<double>(decode_steps) : 0.0;
    result.rss_bytes_after = neuroos::health::rss_bytes();
    return result;
}

void write_json(const std::vector<BenchResult>& results, const std::string& path) {
    std::ofstream out(path);
    out << "{\n  \"component\": \"neuroos-inference\",\n  \"results\": [\n";
    for (std::size_t i = 0; i < results.size(); ++i) {
        const auto& r = results[i];
        out << "    {\n"
            << "      \"n_ctx\": " << r.n_ctx << ",\n"
            << "      \"n_threads\": " << r.n_threads << ",\n"
            << "      \"prompt_tokens\": " << r.prompt_tokens << ",\n"
            << "      \"ttft_ms\": " << r.ttft_ms << ",\n"
            << "      \"prefill_ms_per_token\": " << r.prefill_ms_per_token << ",\n"
            << "      \"decode_ms_per_token\": " << r.decode_ms_per_token << ",\n"
            << "      \"rss_bytes_after\": " << r.rss_bytes_after << ",\n"
            << "      \"error\": \"" << r.error << "\"\n"
            << "    }" << (i + 1 < results.size() ? "," : "") << "\n";
    }
    out << "  ]\n}\n";
}

} // namespace

int run_benchmark(const Config& config) {
    std::string model_path = resolve_model_path(config);
    spdlog::info("bench: loading model {}", model_path);
    auto model = Model::load(model_path, config.model_sha256.value_or(""));
    if (!model) {
        spdlog::error("bench: model load failed: {}", model.error().message);
        return 1;
    }

    std::vector<BenchResult> results;
    // phases.md §5.3 PF: "Benchmark matrix (context x threads)... 128/512/1024".
    for (std::uint32_t n_ctx : {128U, 512U, 1024U}) {
        auto r = bench_one(model.value(), n_ctx, config.threads);
        if (!r.error.empty()) {
            spdlog::error("bench: n_ctx={} failed: {}", n_ctx, r.error);
        } else {
            spdlog::info("bench: n_ctx={} threads={} prompt_tokens={} ttft={:.2f}ms "
                         "prefill={:.3f}ms/tok decode={:.3f}ms/tok rss={}MiB",
                         r.n_ctx, r.n_threads, r.prompt_tokens, r.ttft_ms, r.prefill_ms_per_token,
                         r.decode_ms_per_token, r.rss_bytes_after / (1024 * 1024));
        }
        results.push_back(r);
    }
    // Thread-count sweep at a fixed context (phases.md §5.1 item 7).
    for (std::uint32_t threads : {1U, 4U, config.threads}) {
        auto r = bench_one(model.value(), /*n_ctx=*/512, threads);
        if (!r.error.empty()) {
            spdlog::error("bench: threads={} failed: {}", threads, r.error);
        } else {
            spdlog::info("bench: n_ctx=512 threads={} decode={:.3f}ms/tok", threads,
                         r.decode_ms_per_token);
        }
        results.push_back(r);
    }

    auto now = std::chrono::duration_cast<std::chrono::seconds>(
                   std::chrono::system_clock::now().time_since_epoch())
                   .count();
    std::filesystem::path out_dir = "reports/bench";
    std::error_code ec;
    std::filesystem::create_directories(out_dir, ec);
    std::string out_path = (out_dir / ("inference-" + std::to_string(now) + ".json")).string();
    write_json(results, out_path);
    spdlog::info("bench: wrote {}", out_path);
    return 0;
}

} // namespace neuroos::inference
