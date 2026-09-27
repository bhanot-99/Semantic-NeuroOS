#include "libneuroos/paths.hpp"

#include <unistd.h>

#include <cstdlib>

namespace neuroos::paths {

namespace {

std::string env_or(const char* name, const std::string& fallback) {
    const char* v = std::getenv(name);
    return (v != nullptr) ? std::string(v) : fallback;
}

std::string home_dir() {
    return env_or("HOME", "/");
}

} // namespace

std::string runtime_dir() {
    std::string base = env_or("XDG_RUNTIME_DIR", "/run/user/" + std::to_string(::getuid()));
    return base + "/neuroos";
}

std::string component_health_sock(const std::string& component) {
    return runtime_dir() + "/" + component + ".health.sock";
}

std::string config_dir() {
    return env_or("XDG_CONFIG_HOME", home_dir() + "/.config") + "/neuroos";
}

std::string config_file() {
    return env_or("NEUROOS_CONFIG", config_dir() + "/config.toml");
}

} // namespace neuroos::paths
