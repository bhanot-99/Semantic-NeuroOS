// neuroos-inference [C4] entry point (Architecture.md §6.5, PRD F-05).
#include <spdlog/spdlog.h>
#include <unistd.h>

#include <csignal>
#include <filesystem>
#include <string>
#include <thread>

#include "bench.hpp"
#include "config.hpp"
#include "engine.hpp"
#include "lanes.hpp"
#include "libneuroos/health_server.hpp"
#include "libneuroos/paths.hpp"
#include "libneuroos/sandbox.hpp"
#include "ring.hpp"
#include "server.hpp"

namespace {

volatile std::sig_atomic_t g_shutdown = 0;
void on_signal(int) {
    g_shutdown = 1;
}

} // namespace

int main(int argc, char** argv) {
    spdlog::set_pattern("[%Y-%m-%d %H:%M:%S.%e] [%^%l%$] neuroos-inference: %v");

    auto config = neuroos::inference::load_config();

    if (argc > 1 && std::string(argv[1]) == "--bench") {
        return neuroos::inference::run_benchmark(config);
    }

    std::signal(SIGTERM, on_signal);
    std::signal(SIGINT, on_signal);

    std::string model_path = neuroos::inference::resolve_model_path(config);

    // H15 / Architecture.md §8.2's C4 row: the model (read) and its own
    // sockets in the runtime dir, nothing else. Applied before the model
    // loads and before any thread exists, so every thread inherits it.
    // Fail closed (rules.md §5.5).
    std::error_code mkdir_error;
    std::filesystem::create_directories(neuroos::paths::runtime_dir(), mkdir_error);
    neuroos::sandbox::Policy policy = neuroos::sandbox::Policy::baseline();
    policy.read_only.push_back(std::filesystem::path(model_path).parent_path().string());
    policy.read_write.push_back(neuroos::paths::runtime_dir());
    if (auto sandboxed = neuroos::sandbox::restrict_self(policy); !sandboxed) {
        spdlog::error("failed to enter the Landlock sandbox: {}", sandboxed.error().message);
        return 1;
    }
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
                                  model.value(), lanes, rings, config.max_context_tokens);
    });

    while (g_shutdown == 0) {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
    }
    spdlog::info("shutting down");
    // health_thread/server_thread loop forever inside blocking accept(), and
    // their per-connection threads hold references into `lanes`/`model`/etc
    // that outlive this scope. std::quick_exit (not std::exit): std::exit
    // runs static destructors while those detached threads are still live —
    // tried it while measuring coverage, and it segfaults (a destructor
    // racing a still-running worker thread's use of the same object).
    // quick_exit skips destructors entirely, matching neuroos-healthd's own
    // documented shutdown model (no graceful in-flight-request drain in
    // Phase 2 scope): the OS reclaims everything on process exit regardless.
    std::quick_exit(0);
}
