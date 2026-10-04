#!/usr/bin/env bash
# H15 SC: every real service runs inside its own Landlock sandbox
# (Architecture.md §8.2) and still does its whole job: C3 (models, store,
# backup, GC, forget, spool ingest), C5a (graph view render), C4 (real
# model), healthd, and the C5b cold worker -- all driven for real through
# neuroosctl. Plus one real denial: a symlink in C7's spool pointing at a
# private file outside C3's policy must not be readable by C3 (the
# confused-deputy case from BUGS.md M14).
#
# Needs the models in .dev-cache/models and a built workspace + cpp/build;
# skips cleanly otherwise. Uses temporary XDG dirs only.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
models="$root/.dev-cache/models"
bin="$root/target/debug"
c4="$root/cpp/build/neuroos-inference/neuroos-inference"
for f in "$models/bge-small-en-v1.5/model.onnx" "$models/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf" \
         "$bin/neuroos-storage" "$bin/neuroos-knowledge-query" "$bin/neuroos-healthd" "$bin/neuroosctl" "$c4"; do
    if [ ! -e "$f" ]; then
        echo "SKIP: missing $f (just fetch-models; cargo build --workspace; just build-cpp)"
        exit 0
    fi
done

tmp="$(mktemp -d)"
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT
fail() {
    echo "FAIL: $1" >&2
    for log in "$tmp"/*.log; do echo "--- $log"; tail -20 "$log"; done >&2
    exit 1
}

session_runtime="${XDG_RUNTIME_DIR:-}"
export XDG_RUNTIME_DIR="$tmp/run" XDG_DATA_HOME="$tmp/data" XDG_CONFIG_HOME="$tmp/config"
export NEUROOS_CONFIG="$tmp/config.toml" NEUROOS_LOG=info
mkdir -p "$XDG_RUNTIME_DIR" "$tmp/spool" "$tmp/outside"
chmod 700 "$XDG_RUNTIME_DIR"
cat > "$NEUROOS_CONFIG" <<EOF
[storage]
models_dir = "$models"
spool_dir = "$tmp/spool"

[inference]
model_path = "$models/bitnet-b1.58-2B-4T/ggml-model-i2_s.gguf"
EOF
echo '{"doc_id": "doc-1", "url": "https://example.org/a", "text": "landlock smoke test document"}' \
    > "$tmp/spool/doc-1.json"
echo '{"doc_id": "stolen", "url": "x", "text": "private file outside the sandbox"}' \
    > "$tmp/outside/secret.json"

sock="$XDG_RUNTIME_DIR/neuroos"
wait_for() { # path, seconds
    for _ in $(seq "$(( $2 * 10 ))"); do [ -S "$1" ] && return 0; sleep 0.1; done
    return 1
}

"$bin/neuroos-storage" > "$tmp/storage.log" 2>&1 &
pids+=($!)
wait_for "$sock/storage.sock" 60 || fail "sandboxed neuroos-storage never served storage.sock"
grep -q "Landlock sandbox enforced" "$tmp/storage.log" || fail "neuroos-storage did not report its sandbox"

# Spool: the regular document is ingested and removed (read + remove grant).
for _ in $(seq 50); do [ ! -e "$tmp/spool/doc-1.json" ] && break; sleep 0.1; done
[ ! -e "$tmp/spool/doc-1.json" ] || fail "C3 did not ingest+remove a spool document under its sandbox"
# ...but a symlink to a file outside C3's policy can't be read.
ln -s "$tmp/outside/secret.json" "$tmp/spool/evil.json"
sleep 2
[ -L "$tmp/spool/evil.json" ] || fail "C3 consumed a symlink pointing outside its sandbox"
grep -q "failed to ingest spool file" "$tmp/storage.log" || fail "C3 did not report the denied spool read"
echo "OK: C3 ingests its spool but cannot read through a symlink outside its sandbox"

"$bin/neuroosctl" storage backup > "$tmp/backup.out" || fail "storage backup failed under the sandbox"
backup_path="$(sed -n 's/^backup written to //p' "$tmp/backup.out")"
[ -f "$backup_path/meta.sqlite3" ] || fail "backup snapshot missing at $backup_path"
"$bin/neuroosctl" storage gc > /dev/null || fail "storage gc failed under the sandbox"
"$bin/neuroosctl" forget --app org.example.nothing > /dev/null || fail "forget failed under the sandbox"
echo "OK: C3 backup, GC and forget work inside its sandbox"

# C1 needs the real desktop session's Wayland socket; the rest of its
# world (sockets, config) stays in the temp dirs. Also proves H11 live:
# sandboxed C3 subscribes to sandboxed C1's monitor.sock.
if [ -n "${WAYLAND_DISPLAY:-}" ] && [ -n "$session_runtime" ]; then
    case "$WAYLAND_DISPLAY" in /*) wayland="$WAYLAND_DISPLAY" ;; *) wayland="$session_runtime/$WAYLAND_DISPLAY" ;; esac
    WAYLAND_DISPLAY="$wayland" "$bin/neuroos-monitor" > "$tmp/monitor.log" 2>&1 &
    pids+=($!)
    wait_for "$sock/monitor.control.sock" 20 || fail "sandboxed neuroos-monitor never served monitor.control.sock"
    subscribed=""
    for _ in $(seq 120); do
        "$bin/neuroosctl" monitor-status 2>/dev/null | grep -q "^subscribers: 1" && subscribed=1 && break
        sleep 0.1
    done
    [ -n "$subscribed" ] || fail "C3 never subscribed to C1's monitor.sock"
    echo "OK: C1 runs inside its sandbox and C3 is subscribed to its live telemetry"
else
    echo "SKIP: no Wayland session; C1 not exercised"
fi

"$bin/neuroos-knowledge-query" > "$tmp/knowledge.log" 2>&1 &
pids+=($!)
wait_for "$sock/knowledge.sock" 20 || fail "sandboxed neuroos-knowledge-query never served knowledge.sock"
html="$("$bin/neuroosctl" graph open 2>/dev/null | head -1)"
grep -q "Knowledge graph" "$html" || fail "graph_view.html was not rendered under C5a's sandbox"
echo "OK: C5a renders graph_view.html inside its sandbox"

"$c4" > "$tmp/inference.log" 2>&1 &
pids+=($!)
wait_for "$sock/inference.sock" 60 || fail "sandboxed neuroos-inference never served inference.sock"

"$bin/neuroos-healthd" > "$tmp/healthd.log" 2>&1 &
pids+=($!)
wait_for "$sock/healthd.sock" 20 || fail "sandboxed neuroos-healthd never served healthd.sock"
sleep 1
status="$("$bin/neuroosctl" status --json)"
echo "$status" > "$tmp/status.json"
for c in neuroos-storage neuroos-knowledge-query neuroos-inference; do
    python3 -c "import json,sys; s={c['name']: c['status'] for c in json.load(sys.stdin)['components']}; sys.exit(s.get('$c') not in ('OK', 'DEGRADED'))" \
        <<< "$status" || fail "$c is not reachable ($(python3 -c "import json,sys; print([(c['name'],c['status'],c['rss_bytes']) for c in json.load(sys.stdin)['components']])" < "$tmp/status.json")) in neuroosctl status under its sandbox"
done
# Reachable is the sandbox claim (DEGRADED = up but over 90% of its RSS
# budget, a separate concern from the sandbox).
echo "OK: C4 (real model) and healthd run inside their sandboxes; healthd reaches C3/C4/C5a"

(cd "$root/python/neuroos-knowledge-background" && uv run --offline neuroos-knowledge-background --once) \
    > "$tmp/background.log" 2>&1 || fail "the C5b cold worker failed under its sandbox"
grep -q "wrote .* edges" "$tmp/background.log" || fail "the C5b job did not run to completion"
echo "OK: the C5b cold worker runs a job inside its sandbox"

echo "OK: every service runs inside its Landlock sandbox"
