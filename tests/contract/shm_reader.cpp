// P0-S07 cross-language interop tool (C++ side); see shm_writer.rs for the protocol.
#include <fcntl.h>
#include <unistd.h>

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <string>
#include <thread>

#include "libneuroos/shm.hpp"

int main(int argc, char** argv) {
    if (argc != 3) {
        std::cerr << "usage: shm_reader <proc_fd_path> <count>\n";
        return 2;
    }
    const std::string path = argv[1];
    std::uint32_t count = static_cast<std::uint32_t>(std::stoul(argv[2]));

    int fd = ::open(path.c_str(), O_RDWR);
    if (fd < 0) {
        std::cerr << "open(" << path << ") failed: " << std::strerror(errno) << "\n";
        return 1;
    }
    auto ring = neuroos::shm::Ring::open(fd);
    auto reader = ring.reader();

    std::uint32_t received = 0;
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (received < count) {
        auto piece = reader.try_read();
        if (piece) {
            std::string expected = "token-" + std::to_string(received);
            std::string got(piece->payload.begin(), piece->payload.end());
            if (piece->token_id != received || got != expected) {
                std::printf("FAIL expected token_id=%u payload=%s, got token_id=%u payload=%s\n",
                            received, expected.c_str(), piece->token_id, got.c_str());
                return 1;
            }
            ++received;
        } else {
            if (std::chrono::steady_clock::now() > deadline) {
                std::printf("FAIL timed out after %u/%u messages\n", received, count);
                return 1;
            }
            std::this_thread::yield();
        }
    }
    std::printf("OK %u\n", received);
    return 0;
}
