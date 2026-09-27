// neuroos-inference [C4] entry point (Architecture.md §6.5, PRD F-05).
#include <spdlog/spdlog.h>
#include <unistd.h>

#include <csignal>
#include <thread>

#include "config.hpp"
#include "engine.hpp"
#include "lanes.hpp"
#include "libneuroos/health_server.hpp"
#include "libneuroos/paths.hpp"
#include "ring.hpp"
#include "server.hpp"

namespace {

volatile std::sig_atomic_t g_shutdown = 0;
void on_signal(int) {
    g_shutdown = 1;
}

} // namespace

int main() {
    spdlog::set_pattern("[%Y-%m-%d %H:%M:%S.%e] [%^%l%$] neuroos-inference: %v");

    std::signal(SIGTERM, on_signal);
    std::signal(SIGINT, on_signal);

    auto config = neuroos::inference::load_config();
    std::string model_path = neuroos::inference::resolve_model_path(config);
    spdlog::info("loading model {} (threads={}, max_context_tokens={})", model_path, config.threads,
                 config.max_context_tokens);

    auto model = neuroos::inference::Model::load(model_path, config.model_sha256.value_or(""));
    if (!model) {
        spdlog::error("model load failed: {}", model.error().message);
        return 1;
    }
    auto info = model.value()->info();
    spdlog::info("model loaded: {} (vocab={}, n_ctx_train={})", info.model_name, info.vocab_size,
                 info.context_length);

    auto interactive_ctx = neuroos::inference::Context::create(
        model.value(), config.max_context_tokens, config.threads);
    if (!interactive_ctx) {
        spdlog::error("interactive context create failed: {}", interactive_ctx.error().message);
        return 1;
    }
    auto background_ctx = neuroos::inference::Context::create(
        model.value(), config.max_context_tokens, config.threads);
    if (!background_ctx) {
        spdlog::error("background context create failed: {}", background_ctx.error().message);
        return 1;
    }

    neuroos::inference::RingRegistry rings;
    neuroos::inference::LaneScheduler lanes(std::move(interactive_ctx.value()),
                                            std::move(background_ctx.value()), rings);

    std::uint32_t my_uid = static_cast<std::uint32_t>(::getuid());

    neuroos::health::HealthServer health("neuroos-inference v0.1.0");
    std::thread health_thread([&health, my_uid] {
        health.serve(neuroos::paths::component_health_sock("neuroos-inference"), {my_uid});
    });

    std::thread server_thread([&] {
        neuroos::inference::serve(neuroos::paths::runtime_dir() + "/inference.sock", {my_uid},
                                  model.value(), lanes, rings);
    });

    while (g_shutdown == 0) {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
    }
    spdlog::info("shutting down");
    // health_thread/server_thread loop forever inside blocking accept();
    // process exit reclaims them (matches neuroos-healthd's own shutdown
    // model — no graceful in-flight-request drain in Phase 2 scope).
    std::quick_exit(0);
}
