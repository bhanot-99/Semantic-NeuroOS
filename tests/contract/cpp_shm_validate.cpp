// M1: the C++ ring rejects an untrusted header (zero capacity, a slot too
// small for the slot metadata, an unknown version) and never reads past a
// slot when the shared `utf8_len` is larger than the slot can hold. The
// layout checks must match crates/neuroos-shm/src/ring.rs's
// `validate_layout`, since either side may create a ring the other opens.
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "libneuroos/shm.hpp"

namespace {

int failures = 0;

void check(bool ok, const char* what) {
    std::printf("%s: %s\n", ok ? "OK" : "FAIL", what);
    if (!ok) {
        ++failures;
    }
}

// A memfd whose header is written by hand, so `Ring::open`'s validation --
// not `Ring::create`'s -- is what is under test.
int raw_ring_fd(std::uint16_t version, std::uint32_t capacity_slots, std::uint32_t slot_size,
                std::size_t size) {
    int fd = static_cast<int>(::syscall(SYS_memfd_create, "raw-ring", MFD_CLOEXEC));
    if (fd < 0 || ::ftruncate(fd, static_cast<off_t>(size)) != 0) {
        return -1;
    }
    void* base = ::mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (base == MAP_FAILED) {
        ::close(fd);
        return -1;
    }
    auto* bytes = static_cast<std::uint8_t*>(base);
    std::uint32_t magic = neuroos::shm::kMagic;
    std::uint16_t flags = 0;
    std::memcpy(bytes + 0, &magic, sizeof(magic));
    std::memcpy(bytes + 4, &version, sizeof(version));
    std::memcpy(bytes + 6, &flags, sizeof(flags));
    std::memcpy(bytes + 8, &capacity_slots, sizeof(capacity_slots));
    std::memcpy(bytes + 12, &slot_size, sizeof(slot_size));
    ::munmap(base, size);
    return fd;
}

bool open_is_rejected(std::uint16_t version, std::uint32_t capacity_slots,
                      std::uint32_t slot_size) {
    std::size_t size =
        neuroos::shm::kHeaderSize + static_cast<std::size_t>(capacity_slots) * slot_size;
    int fd = raw_ring_fd(version, capacity_slots, slot_size, size);
    if (fd < 0) {
        return false;
    }
    try {
        neuroos::shm::Ring ring = neuroos::shm::Ring::open(fd);
        return false; // `ring` owns and closes `fd`
    } catch (const neuroos::shm::RingError&) {
        return true; // `open` closed `fd` on every throwing path
    }
}

bool create_is_rejected(std::uint32_t capacity_slots, std::uint32_t slot_size) {
    try {
        neuroos::shm::Ring ring = neuroos::shm::Ring::create("validate", capacity_slots, slot_size);
        return false;
    } catch (const neuroos::shm::RingError&) {
        return true;
    }
}

// A `utf8_len` past the end of the mapping must not be copied; the corrupt
// slot is skipped and the intact one still arrives.
void corrupt_len_is_not_read() {
    neuroos::shm::Ring ring = neuroos::shm::Ring::create("corrupt-len", 2, 64);
    auto writer = ring.writer();
    const std::uint8_t first[] = {'f', 'i', 'r', 's', 't'};
    const std::uint8_t second[] = {'s', 'e', 'c', 'o', 'n', 'd'};
    check(writer.write(0, 0, first, sizeof(first)), "wrote the first slot");
    check(writer.write(1, 0, second, sizeof(second)), "wrote the second slot");

    auto reader = ring.reader();
    // Corrupt slot 0's length field through a second mapping of the same
    // memfd -- exactly the access a hostile peer process would have.
    std::size_t size = neuroos::shm::kHeaderSize + 2 * 64;
    void* peer = ::mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, ring.fd(), 0);
    check(peer != MAP_FAILED, "mapped the ring a second time");
    if (peer == MAP_FAILED) {
        return;
    }
    auto* slot0 = static_cast<std::uint8_t*>(peer) + neuroos::shm::kHeaderSize;
    neuroos::shm::utf8_len_atomic(slot0)->store(0xFFFF, std::memory_order_relaxed);

    auto piece = reader.try_read();
    check(piece.has_value() && piece->token_id == 1, "the corrupt slot is skipped");
    check(piece.has_value() && piece->payload.size() == sizeof(second),
          "the intact slot is still readable");
    check(!reader.try_read().has_value(), "nothing follows the intact slot");
    ::munmap(peer, size);
}

// M12: a token piece wider than one slot's payload used to be dropped
// silently (`write_as` returns false and lanes.cpp ignored it), so the
// client's answer was missing a chunk of text with nothing in the log.
// `write_split` spreads it over consecutive slots instead; the reader (and
// C5's `read_all_tokens`, M11) concatenates payload bytes, so the split is
// invisible to it.
void an_oversize_piece_is_split_not_dropped() {
    neuroos::shm::Ring ring = neuroos::shm::Ring::create("oversize", 32, 64);
    auto writer = ring.writer();
    // 64-byte slots: 32 bytes of payload each, so 100 bytes needs 4 slots.
    std::vector<std::uint8_t> big(100);
    for (std::size_t i = 0; i < big.size(); ++i) {
        big[i] = static_cast<std::uint8_t>('a' + (i % 26));
    }
    check(!writer.write_as(7, 1, 0, big.data(), big.size()),
          "a single write_as still refuses an oversize payload");
    check(writer.write_split(7, 1, neuroos::shm::kFlagEos, big.data(), big.size()),
          "write_split accepts an oversize payload");

    auto reader = ring.reader_for_generation(7);
    std::vector<std::uint8_t> got;
    bool saw_eos = false;
    int slots = 0;
    while (auto piece = reader.try_read()) {
        got.insert(got.end(), piece->payload.begin(), piece->payload.end());
        saw_eos = saw_eos || (piece->flags & neuroos::shm::kFlagEos) != 0;
        ++slots;
    }
    check(slots == 4, "100 bytes over 32-byte payloads takes 4 slots");
    check(got == big, "the reassembled bytes are exactly what was written");
    check(saw_eos, "the terminal flag survives the split");
}

void a_small_piece_still_takes_one_slot() {
    neuroos::shm::Ring ring = neuroos::shm::Ring::create("small", 8, 64);
    auto writer = ring.writer();
    const std::uint8_t text[] = {'h', 'i'};
    check(writer.write_split(1, 1, neuroos::shm::kFlagEos, text, sizeof(text)),
          "write_split accepts a payload that fits");
    auto reader = ring.reader_for_generation(1);
    auto piece = reader.try_read();
    check(piece.has_value() && piece->payload.size() == 2, "one slot, two bytes");
    check(piece.has_value() && (piece->flags & neuroos::shm::kFlagEos) != 0,
          "its flags are the ones passed in");
    check(!reader.try_read().has_value(), "no second slot was written");
}

// A terminal slot carries no payload at all (H8's end-of-stream marker).
void an_empty_piece_still_writes_one_slot() {
    neuroos::shm::Ring ring = neuroos::shm::Ring::create("empty", 8, 64);
    auto writer = ring.writer();
    check(writer.write_split(1, 0, neuroos::shm::kFlagEos, nullptr, 0),
          "write_split accepts an empty payload");
    auto reader = ring.reader_for_generation(1);
    auto piece = reader.try_read();
    check(piece.has_value() && piece->payload.empty(), "one empty slot");
    check(!reader.try_read().has_value(), "and only one");
}

} // namespace

int main() {
    check(create_is_rejected(0, 64), "create rejects a zero capacity");
    check(open_is_rejected(neuroos::shm::kVersion, 0, 64), "open rejects a zero-capacity header");
    check(open_is_rejected(neuroos::shm::kVersion, 2, 16),
          "open rejects a slot_size too small for the slot metadata");
    check(open_is_rejected(neuroos::shm::kVersion + 1, 2, 64), "open rejects an unknown version");
    corrupt_len_is_not_read();
    an_oversize_piece_is_split_not_dropped();
    a_small_piece_still_takes_one_slot();
    an_empty_piece_still_writes_one_slot();

    std::printf("%s\n", failures == 0 ? "all checks passed" : "FAILURES");
    return failures == 0 ? 0 : 1;
}
