// Unit checks for the parts of neuroos-inference that need no model and no
// running server: RingRegistry's L3 ceilings, the L5 config parser's
// strictness, and L5's seed folding. Same "plain main(), explicit checks,
// exit code" convention as cpp_ipc_smoke.cpp -- deliberately not assert(),
// which this project's default RelWithDebInfo build compiles away.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>

#include "config.hpp"
#include "engine.hpp"
#include "ring.hpp"

namespace {

void check(bool cond, const char* what) {
    if (!cond) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        std::exit(1);
    }
}

// L3: nothing frees a ring, so the registry must refuse to grow without
// bound. Before the fix any client could attach rings under fresh names
// until C4 ran out of fds and memory.
void test_ring_registry_is_bounded() {
    neuroos::inference::RingRegistry rings;
    for (std::size_t i = 0; i < neuroos::inference::kMaxRings; ++i) {
        auto writer = rings.get_or_create("ring-" + std::to_string(i), 0, 0);
        check(writer.has_value(), "a ring below the ceiling must be created");
    }
    check(rings.size() == neuroos::inference::kMaxRings, "the registry must be full");

    auto refused = rings.get_or_create("one-too-many", 0, 0);
    check(!refused.has_value(), "a new ring past the ceiling must be refused");
    check(rings.size() == neuroos::inference::kMaxRings, "a refusal must not grow the registry");

    // An already-created name keeps working, full registry or not: a
    // refusal must never break the clients that are already streaming.
    auto existing = rings.get_or_create("ring-0", 0, 0);
    check(existing.has_value(), "an existing ring must still be returned when full");
    std::printf("OK: the ring registry refuses to grow past kMaxRings\n");
}

// L3: capacity_slots and slot_size come straight off the wire and multiply
// into the memfd's size, so each has a ceiling and so does their product.
void test_ring_shape_is_bounded() {
    neuroos::inference::RingRegistry rings;
    check(!rings.get_or_create("too-many-slots",
                               neuroos::inference::kMaxRingCapacitySlots + 1, 256)
               .has_value(),
          "too many slots must be refused");
    check(!rings.get_or_create("too-wide-slots", 16,
                               neuroos::inference::kMaxRingSlotSize + 8)
               .has_value(),
          "too wide a slot must be refused");
    check(!rings.get_or_create("too-big", neuroos::inference::kMaxRingCapacitySlots,
                               neuroos::inference::kMaxRingSlotSize)
               .has_value(),
          "a ring over kMaxRingBytes must be refused");
    check(rings.size() == 0, "no refused request may create a ring");

    // The shape the real clients ask for is well inside the ceilings.
    check(rings.get_or_create("default-shape", 0, 0).has_value(),
          "the default shape must still be accepted");
    std::printf("OK: the ring registry bounds the shape a client may ask for\n");
}

// L5: a malformed [inference] table, or one with an unrecognised key, is
// an error. It used to warn and run on the defaults, which is how a
// typo'd `model_sha256` silently skipped the model integrity check.
void test_config_is_strict() {
    auto empty = neuroos::inference::parse_config("");
    check(empty.has_value(), "an empty config.toml is not an error");
    check(empty.value().threads == 8, "defaults must survive an empty config");
    check(!empty.value().model_sha256.has_value(), "no sha256 by default");

    auto other_section = neuroos::inference::parse_config("[monitor]\nenabled = true\n");
    check(other_section.has_value(), "a config.toml with no [inference] table is not an error");

    auto good = neuroos::inference::parse_config(
        "[inference]\nthreads = 4\nmax_context_tokens = 1024\nmodel_sha256 = \"abc\"\n");
    check(good.has_value(), "a well-formed [inference] table must parse");
    check(good.value().threads == 4, "threads must be read");
    check(good.value().max_context_tokens == 1024, "max_context_tokens must be read");
    check(good.value().model_sha256.value_or("") == "abc", "model_sha256 must be read");

    // The exact bug L5 names: one character wrong and the integrity check
    // used to vanish without a word.
    auto typo = neuroos::inference::parse_config("[inference]\nmodel_sha25 = \"abc\"\n");
    check(!typo.has_value(), "a typo'd key must be an error, not a silent default");
    check(typo.error().message.find("model_sha25") != std::string::npos,
          "the error must name the offending key");

    auto malformed = neuroos::inference::parse_config("[inference\nthreads = ");
    check(!malformed.has_value(), "unparseable TOML must be an error");

    auto wrong_type = neuroos::inference::parse_config("[inference]\nthreads = \"eight\"\n");
    check(!wrong_type.has_value(), "a key of the wrong type must be an error");
    std::printf("OK: [inference] parsing is strict and fails loudly\n");
}

// L5: llama_sampler_init_dist takes a uint32_t, so the request's 64-bit
// seed was narrowed by an implicit conversion -- two seeds differing only
// above bit 31 produced byte-identical output.
void test_seed_folding() {
    std::uint64_t low = 0x0000'0000'1234'5678ULL;
    std::uint64_t high_differs = 0xABCD'EF01'1234'5678ULL;
    check(neuroos::inference::sampler_seed(low) !=
              neuroos::inference::sampler_seed(high_differs),
          "two seeds differing only in their high 32 bits must not collide");

    check(neuroos::inference::sampler_seed(42) == neuroos::inference::sampler_seed(42),
          "seed folding must be deterministic");

    // A non-zero seed must never fold onto the "pick a random seed" value.
    std::uint64_t folds_to_zero = 0x0000'00FF'0000'00FFULL;
    check(static_cast<std::uint32_t>(folds_to_zero ^ (folds_to_zero >> 32)) == 0,
          "the probe must actually fold to 0");
    check(neuroos::inference::sampler_seed(folds_to_zero) != 0,
          "a non-zero seed must not become the random-seed sentinel");
    std::printf("OK: a 64-bit seed is folded, not truncated\n");
}

} // namespace

int main() {
    test_ring_registry_is_bounded();
    test_ring_shape_is_bounded();
    test_config_is_strict();
    test_seed_folding();
    std::printf("all cpp_inference_unit checks passed\n");
    return 0;
}
