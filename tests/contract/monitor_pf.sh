#!/usr/bin/env bash
# PF (phases.md §6.3): RSS <= 25 MiB, idle CPU < 0.5%.
# Runs the real neuroos-monitor release binary against the real live
# Wayland/D-Bus session (no mocks -- there's no headless-compositor harness
# in this environment; see phases.md §6.3's IT row for the deferred one).
# Capture latency p99 < 1.5ms is measured separately, in-process, by
# `cargo test -p neuroos-monitor --test pf_latency` (no live desktop needed
# for that one).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin="$root/target/release/neuroos-monitor"

if [ ! -x "$bin" ]; then
    echo "SKIP: $bin not built (run: cargo build --release -p neuroos-monitor)" >&2
    exit 0
fi
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "SKIP: no WAYLAND_DISPLAY (no live desktop session in this environment)" >&2
    exit 0
fi

tmp="$(mktemp -d)"
trap 'kill "${mon_pid:-0}" 2>/dev/null || true; rm -rf "$tmp"' EXIT
export XDG_RUNTIME_DIR="$tmp/run"
mkdir -p "$XDG_RUNTIME_DIR"

NEUROOS_LOG=warn "$bin" >"$tmp/monitor.log" 2>&1 &
mon_pid=$!
sleep 2

if ! kill -0 "$mon_pid" 2>/dev/null; then
    echo "FAIL: neuroos-monitor exited unexpectedly" >&2
    cat "$tmp/monitor.log" >&2
    exit 1
fi

clk_tck="$(getconf CLK_TCK)"
read -r _ u1 s1 < <(awk '{print $1, $14, $15}' "/proc/$mon_pid/stat")
sleep 10
read -r _ u2 s2 < <(awk '{print $1, $14, $15}' "/proc/$mon_pid/stat")
rss_kb="$(awk '/VmRSS:/{print $2}' "/proc/$mon_pid/status")"
cpu_ticks=$(( (u2 - u1) + (s2 - s1) ))
cpu_pct="$(echo "scale=3; $cpu_ticks / $clk_tck / 10 * 100" | bc)"

echo "measured: RSS=${rss_kb}KiB (budget 25600KiB), idle CPU over 10s=${cpu_pct}% (budget 0.5%)"

if [ "$rss_kb" -gt 25600 ]; then
    echo "FAIL: RSS ${rss_kb}KiB exceeds 25 MiB budget" >&2
    exit 1
fi
if (( $(echo "$cpu_pct > 0.5" | bc -l) )); then
    echo "FAIL: idle CPU ${cpu_pct}% exceeds 0.5% budget" >&2
    exit 1
fi
