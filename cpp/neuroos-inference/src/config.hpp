// [inference] section of the shared config.toml (rules.md §4: one
// config.toml, strict, fail fast on unknown keys at the Rust schema level —
// see crates/neuroos-common/src/config.rs's InferenceConfig, which this
// mirrors field-for-field). A missing file or missing [inference] table is
// not an error; it just means every field takes its default, matching
// neuroos_common::load_config's Rust-side behavior.
#pragma once

#include <cstdint>
#include <optional>
#include <string>

namespace neuroos::inference {

struct Config {
    // Absolute path override; empty means "derive from models_dir() and
    // models/manifest.toml's entry for C4" (see resolve_model_path).
    std::optional<std::string> model_path;
    // models/manifest.toml's sha256 for the model at model_path/the default
    // location (Model::load skips the check with a warning if empty — this
    // is defense-in-depth beyond scripts/fetch-models.sh's own fetch-time
    // verification, not this binary's only integrity gate).
    std::optional<std::string> model_sha256;
    std::uint32_t threads = 8;              // ADR-0006: reference machine has 8 physical cores
    std::uint32_t max_context_tokens = 512; // PRD FR-INF-02 default
};

// `$NEUROOS_CONFIG` if set, else `~/.config/neuroos/config.toml` — same
// resolution order as neuroos_common::paths::config_file().
std::string config_file_path();

Config load_config();

// Resolves the real model path used at startup: `config.model_path` if set,
// else `<models_dir>/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf`
// (models/manifest.toml's C4 entry, Architecture.md §7.1). `models_dir` is
// `/opt/neuroos/models` by default; use `config.model_path` to point at a
// local dev cache (e.g. `.dev-cache/models/...`) without root.
std::string resolve_model_path(const Config& config);

} // namespace neuroos::inference
