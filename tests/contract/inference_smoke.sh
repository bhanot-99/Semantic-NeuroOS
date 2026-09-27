#!/usr/bin/env bash
# IT (phases.md §5.3): starts the real neuroos-inference binary against the
# real downloaded BitNet model, then drives it with cpp_inference_smoke
# (GetInfo/AttachRing/Generate/Cancel over a real inference.sock, real
# tokens through a real memfd ring). Requires the model to already be
# fetched (scripts/fetch-models.sh, or the .dev-cache copy from P0-S09) —
# skips with a clear message rather than failing the whole CI run if it
# isn't, same policy as `just fetch-models` being a separate opt-in step.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
inference_bin="$root/cpp/build/neuroos-inference/neuroos-inference"
smoke_bin="$root/cpp/build/cpp-inference-smoke"

model_path="${NEUROOS_TEST_MODEL_PATH:-$root/.dev-cache/models/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf}"
if [ ! -f "$model_path" ]; then
    echo "SKIP: model not found at $model_path (run 'just fetch-models' or set NEUROOS_TEST_MODEL_PATH)"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'kill "${inference_pid:-0}" 2>/dev/null || true; rm -rf "$tmp"' EXIT

export XDG_RUNTIME_DIR="$tmp/run"
mkdir -p "$XDG_RUNTIME_DIR"
cat >"$tmp/config.toml" <<EOF
[inference]
model_path = "$model_path"
threads = 8
max_context_tokens = 512
EOF
export NEUROOS_CONFIG="$tmp/config.toml"

"$inference_bin" >"$tmp/inference.log" 2>&1 &
inference_pid=$!

sock="$XDG_RUNTIME_DIR/neuroos/inference.sock"
for _ in $(seq 1 100); do
    [ -S "$sock" ] && break
    sleep 0.2
done
if [ ! -S "$sock" ]; then
    echo "FAIL: inference.sock never appeared; neuroos-inference log:" >&2
    cat "$tmp/inference.log" >&2
    exit 1
fi

if ! "$smoke_bin"; then
    echo "neuroos-inference log:" >&2
    tail -n 100 "$tmp/inference.log" >&2
    exit 1
fi

kill "$inference_pid" 2>/dev/null || true
wait "$inference_pid" 2>/dev/null || true
rm -f "$sock"

# FI (phases.md §5.3): sha256 verification, both directions (FR-INF-01).
model_sha256="$(sha256sum "$model_path" | cut -d' ' -f1)"

cat >"$tmp/config-good-sha.toml" <<EOF
[inference]
model_path = "$model_path"
model_sha256 = "$model_sha256"
threads = 8
max_context_tokens = 128
EOF
NEUROOS_CONFIG="$tmp/config-good-sha.toml" "$inference_bin" >"$tmp/good-sha.log" 2>&1 &
good_pid=$!
for _ in $(seq 1 50); do
    grep -q "sha256 verified" "$tmp/good-sha.log" 2>/dev/null && break
    sleep 0.2
done
if ! grep -q "sha256 verified" "$tmp/good-sha.log"; then
    echo "FAIL: correct model_sha256 did not verify; log:" >&2
    cat "$tmp/good-sha.log" >&2
    kill "$good_pid" 2>/dev/null || true
    exit 1
fi
echo "OK: correct model_sha256 verified, model loaded"
kill "$good_pid" 2>/dev/null || true
wait "$good_pid" 2>/dev/null || true

cat >"$tmp/config-bad-sha.toml" <<EOF
[inference]
model_path = "$model_path"
model_sha256 = "0000000000000000000000000000000000000000000000000000000000000001"
threads = 8
max_context_tokens = 128
EOF
bad_sha_exit=0
NEUROOS_CONFIG="$tmp/config-bad-sha.toml" "$inference_bin" >"$tmp/bad-sha.log" 2>&1 || bad_sha_exit=$?
if [ "$bad_sha_exit" -eq 0 ]; then
    echo "FAIL: neuroos-inference must exit non-zero on a sha256 mismatch" >&2
    cat "$tmp/bad-sha.log" >&2
    exit 1
fi
if ! grep -q "sha256 mismatch" "$tmp/bad-sha.log"; then
    echo "FAIL: sha256 mismatch must produce a clear error message; log:" >&2
    cat "$tmp/bad-sha.log" >&2
    exit 1
fi
echo "OK: sha256 mismatch -> fatal exit ($bad_sha_exit) with a clear error, model never loaded"

# FI: corrupt/invalid model file -> fatal exit with a clear error (not a crash).
echo "not a real gguf file" >"$tmp/corrupt.gguf"
cat >"$tmp/config-corrupt.toml" <<EOF
[inference]
model_path = "$tmp/corrupt.gguf"
threads = 8
max_context_tokens = 128
EOF
corrupt_exit=0
NEUROOS_CONFIG="$tmp/config-corrupt.toml" "$inference_bin" >"$tmp/corrupt.log" 2>&1 || corrupt_exit=$?
if [ "$corrupt_exit" -eq 0 ]; then
    echo "FAIL: neuroos-inference must exit non-zero on a corrupt model file" >&2
    cat "$tmp/corrupt.log" >&2
    exit 1
fi
if ! grep -q "model load failed" "$tmp/corrupt.log"; then
    echo "FAIL: corrupt model file must produce a clear error message; log:" >&2
    cat "$tmp/corrupt.log" >&2
    exit 1
fi
echo "OK: corrupt model file -> fatal exit ($corrupt_exit) with a clear error, no crash"
