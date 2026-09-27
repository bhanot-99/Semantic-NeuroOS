#include "libneuroos/sha256.hpp"

#include <openssl/evp.h>

#include <array>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <memory>

namespace neuroos::crypto {

Expected<std::string, Sha256Error> sha256_file(const std::string& path) {
    std::unique_ptr<std::FILE, int (*)(std::FILE*)> file(std::fopen(path.c_str(), "rb"),
                                                         &std::fclose);
    if (!file) {
        return make_unexpected(Sha256Error{"failed to open " + path + ": " + std::strerror(errno)});
    }

    std::unique_ptr<EVP_MD_CTX, void (*)(EVP_MD_CTX*)> ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    if (!ctx || EVP_DigestInit_ex(ctx.get(), EVP_sha256(), nullptr) != 1) {
        return make_unexpected(Sha256Error{"failed to initialize SHA-256 context"});
    }

    std::array<unsigned char, 1 << 16> buf{};
    for (;;) {
        std::size_t n = std::fread(buf.data(), 1, buf.size(), file.get());
        if (n > 0 && EVP_DigestUpdate(ctx.get(), buf.data(), n) != 1) {
            return make_unexpected(Sha256Error{"SHA-256 update failed"});
        }
        if (n < buf.size()) {
            if (std::ferror(file.get()) != 0) {
                return make_unexpected(Sha256Error{"read error on " + path});
            }
            break; // EOF
        }
    }

    unsigned char digest[EVP_MAX_MD_SIZE];
    unsigned int digest_len = 0;
    if (EVP_DigestFinal_ex(ctx.get(), digest, &digest_len) != 1) {
        return make_unexpected(Sha256Error{"SHA-256 finalize failed"});
    }

    static const char* kHex = "0123456789abcdef";
    std::string hex;
    hex.reserve(static_cast<std::size_t>(digest_len) * 2);
    for (unsigned int i = 0; i < digest_len; ++i) {
        hex.push_back(kHex[(digest[i] >> 4) & 0xF]);
        hex.push_back(kHex[digest[i] & 0xF]);
    }
    return hex;
}

} // namespace neuroos::crypto
