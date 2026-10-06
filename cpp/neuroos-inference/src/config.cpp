#include "config.hpp"

#include <toml.hpp>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <sstream>

#include "libneuroos/paths.hpp"

namespace neuroos::inference {

std::string config_file_path() {
    return neuroos::paths::config_file();
}

const std::vector<std::string>& known_inference_keys() {
    static const std::vector<std::string> keys = {"model_path", "model_sha256", "threads",
                                                  "max_context_tokens"};
    return keys;
}

neuroos::Expected<Config, ConfigError> parse_config(const std::string& toml_text) {
    Config config;
    try {
        std::istringstream stream(toml_text);
        const auto data = toml::parse(stream, "config.toml");
        if (!data.is_table() || data.as_table().count("inference") == 0) {
            return config; // no [inference] table: defaults
        }
        const auto& inference = data.as_table().at("inference");
        if (!inference.is_table()) {
            return make_unexpected(ConfigError{"[inference] is not a table"});
        }
        // L5: reject an unrecognised key instead of ignoring it. A typo'd
        // `model_sha256` used to disable the model integrity check in
        // silence; now it stops startup, the way the Rust-side strict
        // parser already does for every other section.
        for (const auto& [key, value] : inference.as_table()) {
            (void)value;
            const auto& known = known_inference_keys();
            if (std::find(known.begin(), known.end(), key) == known.end()) {
                return make_unexpected(ConfigError{"unknown key in [inference]: " + key});
            }
        }
        // L5: `toml::find_or` returns its fallback for a key of the *wrong
        // type* just as readily as for a missing one, so `threads =
        // "eight"` used to read as the default 8. `toml::find` throws
        // instead, and the catch below turns that into a startup error.
        // The presence check keeps a missing key on its default.
        const auto& table = inference.as_table();
        if (table.count("threads") != 0) {
            config.threads = toml::find<std::uint32_t>(inference, "threads");
        }
        if (table.count("max_context_tokens") != 0) {
            config.max_context_tokens = toml::find<std::uint32_t>(inference, "max_context_tokens");
        }
        if (table.count("model_path") != 0) {
            std::string model_path = toml::find<std::string>(inference, "model_path");
            if (!model_path.empty()) {
                config.model_path = model_path;
            }
        }
        if (table.count("model_sha256") != 0) {
            std::string model_sha256 = toml::find<std::string>(inference, "model_sha256");
            if (!model_sha256.empty()) {
                config.model_sha256 = model_sha256;
            }
        }
    } catch (const std::exception& e) {
        // rules.md §8: wrap third-party code that throws at the boundary.
        // L5: converted to an error rather than a warning plus defaults --
        // a malformed config.toml is startup-fatal for every other
        // component (neuroos_common::load_config errors out too), and
        // falling back to defaults here meant a broken [inference] table
        // produced a *running* C4 with the wrong settings.
        return make_unexpected(
            ConfigError{std::string("failed to parse config.toml: ") + e.what()});
    }
    return config;
}

neuroos::Expected<Config, ConfigError> load_config() {
    const std::string path = config_file_path();
    std::error_code ec;
    if (!std::filesystem::exists(path, ec)) {
        return Config{}; // no config file: every field keeps its default
    }
    std::ifstream file(path, std::ios::binary);
    if (!file) {
        return make_unexpected(ConfigError{"failed to read " + path});
    }
    std::ostringstream text;
    text << file.rdbuf();
    auto config = parse_config(text.str());
    if (!config) {
        return make_unexpected(ConfigError{config.error().message + " (" + path + ")"});
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
