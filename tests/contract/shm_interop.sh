#!/usr/bin/env bash
# Cross-language memfd seqlock ring interop (P0-S07 spike S-02): a writer in
# one language creates the ring, writes N messages, and announces a
# /proc/<pid>/fd/<n> path; a reader in the other language opens that path
# directly and verifies every message. Proves the wire format and seqlock
# protocol (crates/neuroos-shm/src/ring.rs, cpp/libneuroos/.../shm_ring.hpp)
# are byte-for-byte compatible across languages. Real production fd handoff
# is SCM_RIGHTS over inference.sock (Architecture.md §5.1); that is Phase 2
# wiring, not needed to prove the ring format/protocol itself.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
rust_writer="$root/target/debug/shm_writer"
rust_reader="$root/target/debug/shm_reader"
cpp_writer="$root/cpp/build/shm-writer-cpp"
cpp_reader="$root/cpp/build/shm-reader-cpp"

# The writer finishes every write before announcing readiness (no concurrent
# draining in this test — that's covered separately by the in-process
# stress tests), so capacity must be >= COUNT or the writer would lap itself
# before the reader ever connects. This test's job is proving the wire
# format is byte-compatible across languages, not proving lapping behavior.
CAPACITY=1024
SLOT_SIZE=64
COUNT=1000

run_pair() {
    local writer="$1" reader="$2" label="$3"

    coproc WRITER { "$writer" "$CAPACITY" "$SLOT_SIZE" "$COUNT"; }

    local line path=""
    read -r -u "${WRITER[0]}" line
    path="${line#READY }"
    if [ -z "$path" ] || [ "$path" = "$line" ]; then
        echo "FAIL ($label): writer didn't announce a path (got: '$line')" >&2
        exit 1
    fi

    local result
    result="$("$reader" "$path" "$COUNT")"

    printf '\n' >&"${WRITER[1]}" # unblock the writer's blocking read_line
    wait "$WRITER_PID" 2>/dev/null || true

    if [[ "$result" != OK* ]]; then
        echo "FAIL ($label): $result" >&2
        exit 1
    fi
    echo "OK ($label): $result"
}

run_pair "$rust_writer" "$cpp_reader" "rust writer -> cpp reader"
run_pair "$cpp_writer" "$rust_reader" "cpp writer -> rust reader"

echo "OK: memfd seqlock ring interop verified both directions ($COUNT messages each)"
