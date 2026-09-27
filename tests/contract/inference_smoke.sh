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
