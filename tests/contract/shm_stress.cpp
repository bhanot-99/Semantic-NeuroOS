// P0-S07 spike S-02: same-process writer/reader thread stress test, C++
// side. Mirrors crates/neuroos-shm/tests/ring_stress.rs's
// concurrent_writer_reader_no_torn_reads: a fast writer racing a slower
// reader is expected to lose some messages to lapping (by design, see
// shm_ring.hpp), but must never corrupt a payload or return token_ids out
// of order. Build with the `tsan` CMake preset for the ThreadSanitizer run.
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <optional>
#include <string>
#include <thread>

#include "libneuroos/shm.hpp"

namespace {
constexpr std::uint32_t kCapacity = 256;
constexpr std::uint32_t kSlotSize = 128;
std::atomic<bool> g_writer_done{false};
} // namespace

int main(int argc, char** argv) {
    std::uint32_t messages = 200000;
    if (argc > 1) {
        messages = static_cast<std::uint32_t>(std::stoul(argv[1]));
    }

    auto ring = neuroos::shm::Ring::create("neuroos-shm-stress-cpp", kCapacity, kSlotSize);
    auto writer = ring.writer();
    auto reader = ring.reader();

    std::thread writer_thread([&] {
        for (std::uint32_t token_id = 0; token_id < messages; ++token_id) {
            std::string payload = "token-" + std::to_string(token_id);
            if (!writer.write(token_id, 0, reinterpret_cast<const std::uint8_t*>(payload.data()),
                              payload.size())) {
                std::fprintf(stderr, "write failed for token_id=%u\n", token_id);
                std::exit(1);
            }
        }
        // Set from the writer thread itself, not after `join()` on the main
        // thread: the reader loop below runs on the main thread, so nothing
        // else could ever set this flag while that loop is still spinning.
        g_writer_done.store(true, std::memory_order_release);
    });

    std::uint32_t received = 0;
    std::optional<std::uint32_t> last_token_id;
    bool writer_done = false;
    for (;;) {
        auto piece = reader.try_read();
        if (piece) {
            std::string expected = "token-" + std::to_string(piece->token_id);
            std::string got(piece->payload.begin(), piece->payload.end());
            if (got != expected) {
                std::fprintf(stderr, "corrupted payload for token_id %u: got %s\n", piece->token_id,
                             got.c_str());
                std::exit(1);
            }
            if (last_token_id && piece->token_id <= *last_token_id) {
                std::fprintf(stderr, "token_id went backwards or repeated: %u after %u\n",
                             piece->token_id, *last_token_id);
                std::exit(1);
            }
            last_token_id = piece->token_id;
            ++received;
        } else if (writer_done) {
            break;
        } else {
            writer_done = g_writer_done.load(std::memory_order_acquire);
        }
    }

    writer_thread.join();
    // one more drain pass in case the writer finished between our last two checks above
    while (auto piece = reader.try_read()) {
        std::string expected = "token-" + std::to_string(piece->token_id);
        std::string got(piece->payload.begin(), piece->payload.end());
        if (got != expected || (last_token_id && piece->token_id <= *last_token_id)) {
            std::fprintf(stderr, "corruption in final drain pass\n");
            return 1;
        }
        last_token_id = piece->token_id;
        ++received;
    }

    if (received == 0) {
        std::fprintf(stderr, "reader observed zero messages\n");
        return 1;
    }
    std::printf("shm_stress: %u/%u messages observed intact\n", received, messages);
    return 0;
}
