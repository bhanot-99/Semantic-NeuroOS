// P0-S07 cross-language interop tool (C++ side); see shm_writer.rs for the protocol.
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <iostream>
#include <string>

#include "libneuroos/shm.hpp"

int main(int argc, char** argv) {
    if (argc != 4) {
        std::cerr << "usage: shm_writer <capacity_slots> <slot_size> <count>\n";
        return 2;
    }
    std::uint32_t capacity = static_cast<std::uint32_t>(std::stoul(argv[1]));
    std::uint32_t slot_size = static_cast<std::uint32_t>(std::stoul(argv[2]));
    std::uint32_t count = static_cast<std::uint32_t>(std::stoul(argv[3]));

    auto ring = neuroos::shm::Ring::create("neuroos-shm-interop-cpp", capacity, slot_size);
    auto writer = ring.writer();
    for (std::uint32_t token_id = 0; token_id < count; ++token_id) {
        std::string payload = "token-" + std::to_string(token_id);
        if (!writer.write(token_id, 0, reinterpret_cast<const std::uint8_t*>(payload.data()),
                          payload.size())) {
            std::cerr << "write failed for token_id=" << token_id << "\n";
            return 1;
        }
    }

    std::printf("READY /proc/%d/fd/%d\n", ::getpid(), ring.fd());
    std::fflush(stdout);

    std::string line;
    std::getline(std::cin, line);
    return 0;
}
