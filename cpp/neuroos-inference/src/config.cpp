#include "config.hpp"

#include <spdlog/spdlog.h>
#include <toml.hpp>

#include <filesystem>

#include "libneuroos/paths.hpp"

namespace neuroos::inference {

std::string config_file_path() {
    return neuroos::paths::config_file();
}

Config load_config() {
    Config config;
    const std::string path = config_file_path();
    if (!std::filesystem::exists(path)) {
        return config; // no config file: every field keeps its default
    }

    try {
        const auto data = toml::parse(path);
        if (!data.is_table() || data.as_table().count("inference") == 0) {
            return config; // no [inference] table: defaults
        }
        // Fetch the [inference] sub-table once and use single-key find_or on
        // it: toml11's variadic multi-key find_or(data, "inference", "key",
        // fallback) fails to deduce when `fallback` is an lvalue reference
        // (as `config.threads` is here), so this sidesteps that entirely.
        const auto& inference = data.as_table().at("inference");
        config.threads = toml::find_or<std::uint32_t>(inference, "threads", 8u);
        config.max_context_tokens =
            toml::find_or<std::uint32_t>(inference, "max_context_tokens", 512u);
        std::string model_path = toml::find_or<std::string>(inference, "model_path", std::string{});
        if (!model_path.empty()) {
            config.model_path = model_path;
        }
        std::string model_sha256 =
            toml::find_or<std::string>(inference, "model_sha256", std::string{});
        if (!model_sha256.empty()) {
            config.model_sha256 = model_sha256;
        }
    } catch (const std::exception& e) {
        // rules.md §8: wrap third-party code that throws at the boundary.
        // A malformed config.toml is a startup-fatal condition for every
        // other component (neuroos_common::load_config errors out too), so
        // this logs and falls back to defaults rather than crashing —
        // engine.cpp's own model-path/sha256 checks are the real gate.
        spdlog::warn("failed to parse {}: {}; using inference config defaults", path, e.what());
    }
    return config;
}

std::string resolve_model_path(const Config& config) {
    if (config.model_path.has_value()) {
        return *config.model_path;
    }
    // models/manifest.toml's C4 entry (Architecture.md §7.1); models_dir()
    // mirrors crates/neuroos-common/src/paths.rs::models_dir().
    return "/opt/neuroos/models/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf";
}

} // namespace neuroos::inference
