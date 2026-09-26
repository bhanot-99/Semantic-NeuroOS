#!/usr/bin/env bash
# Cross-language Envelope round-trip (P0-S02 CT test, Architecture.md §5.4):
# Rust encode -> C++ decode/re-encode -> Python decode/re-encode -> Rust decode/re-encode,
# asserting every hop is byte-identical to the original Rust encoding.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
rust_bin="$root/target/debug/contract_codec"
cpp_bin="$root/cpp/build/contract-codec-cpp"
py_dir="$root/python/neuroos-knowledge-background"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

"$rust_bin" encode-fixture > "$tmp/0_rust.bin"
"$cpp_bin" roundtrip < "$tmp/0_rust.bin" > "$tmp/1_cpp.bin"
(cd "$py_dir" && uv run python "$root/tests/contract/py_codec.py" roundtrip) < "$tmp/1_cpp.bin" > "$tmp/2_py.bin"
"$rust_bin" roundtrip < "$tmp/2_py.bin" > "$tmp/3_rust.bin"

for step in 1_cpp 2_py 3_rust; do
    if ! cmp -s "$tmp/0_rust.bin" "$tmp/${step}.bin"; then
        echo "FAIL: $tmp/${step}.bin differs from $tmp/0_rust.bin" >&2
        exit 1
    fi
done

echo "OK: Rust -> C++ -> Python -> Rust round-trip byte-identical ($(wc -c < "$tmp/0_rust.bin") bytes)"
