// SHA-256 file hashing (models/manifest.toml integrity checks, FR-INF-01:
// "Load BitNet ... via read-only mmap" implies loading only a verified
// file). Thin wrapper over OpenSSL's EVP API — no reason to hand-roll
// crypto for a security-relevant integrity check.
#pragma once

#include <string>

#include "libneuroos/expected.hpp"

namespace neuroos::crypto {

struct Sha256Error {
    std::string message;
};

// Lowercase hex digest of `path`'s contents, streamed (doesn't load the
// whole file into memory — model files are gigabytes).
Expected<std::string, Sha256Error> sha256_file(const std::string& path);

} // namespace neuroos::crypto
