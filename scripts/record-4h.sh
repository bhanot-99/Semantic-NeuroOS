#!/usr/bin/env bash
# One 4-hour raw telemetry recording for the KPI-1 eval
# (crates/neuroos-knowledge-query/tests/kpi1_eval.rs). Waits until START
# (HH:MM, today, or tomorrow if already past), then records one dump for
# DURATION seconds with idle-sleep inhibited. Output is gitignored
# (.dev-cache/) and never committed.
#   scripts/record-4h.sh [START=19:00] [DURATION=14400]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

START="${1:-19:00}"
DURATION="${2:-14400}"
BIN=./target/release/neuroos-monitor
OUT_DIR=.dev-cache/telemetry-raw
OUT="$OUT_DIR/dump-$(date +%Y%m%d)-4h.bin"

[[ -x "$BIN" ]] || { echo "missing $BIN (cargo build --release -p neuroos-monitor)" >&2; exit 1; }
mkdir -p "$OUT_DIR"

target=$(date -d "$START" +%s)
now=$(date +%s)
((target > now)) || target=$(date -d "tomorrow $START" +%s)
echo "$(date -Is) waiting until $(date -d "@$target" -Is)"
sleep $((target - now))

echo "$(date -Is) recording ${DURATION}s to $OUT"
NEUROOS_LOG=warn systemd-inhibit --what=idle:sleep --who=neuroos-record --why="KPI-1 recording" \
    timeout "$DURATION" "$BIN" --record "$OUT" || [[ $? -eq 124 ]] # 124 = timeout reached, expected
echo "$(date -Is) done: $(stat -c %s "$OUT") bytes"
