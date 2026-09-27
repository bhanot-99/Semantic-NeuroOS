// Standard filesystem locations (Architecture.md §7.1), C++ side. Mirrors
// crates/neuroos-common/src/paths.rs's runtime_dir()/component_health_sock()
// — every C++ component (neuroos-inference now, neuroos-voice later) needs
// these, so they live here rather than being duplicated per component.
#pragma once

#include <string>

namespace neuroos::paths {

// `$XDG_RUNTIME_DIR/neuroos/` = `/run/user/$UID/neuroos/` (mode 0700).
std::string runtime_dir();

// `<runtime_dir>/<component>.health.sock`
std::string component_health_sock(const std::string& component);

// `$XDG_CONFIG_HOME/neuroos/` (or `~/.config/neuroos/` if unset).
std::string config_dir();

// `$NEUROOS_CONFIG` if set, else `<config_dir>/config.toml`.
std::string config_file();

} // namespace neuroos::paths
