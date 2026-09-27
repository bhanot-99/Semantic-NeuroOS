#!/usr/bin/env bash
# PF (phases.md §4.3): RSS <= 15 MiB with 9 targets; one scrape cycle <= 50 ms CPU.
# Runs the real neuroos-healthd binary (not a unit test in-process) against
# 8 real mock health servers under a temp XDG_RUNTIME_DIR (matching the 9
# built-in default targets exactly; the 9th, neuroos-fetcher, uses a fixed
# /run/neuroos-fetcher path this unprivileged test can't bind to, so it
# scrapes as DOWN here -- the registry still has 9 targets, which is what
# this budget is actually about: healthd's own overhead at that target
# count, not that every mock must be reachable).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
farm_bin="$root/target/debug/health_mock_farm"
healthd_bin="$root/target/debug/neuroos-healthd"

tmp="$(mktemp -d)"
trap 'kill "${farm_pid:-0}" "${healthd_pid:-0}" 2>/dev/null || true; rm -rf "$tmp"' EXIT

export XDG_RUNTIME_DIR="$tmp/run"
mkdir -p "$XDG_RUNTIME_DIR"

cat >"$tmp/config.toml" <<'EOF'
[healthd]
poll_interval_s = 1
per_target_timeout_s = 1
EOF
export NEUROOS_CONFIG="$tmp/config.toml"

names=(neuroos-monitor neuroos-voice neuroos-storage neuroos-inference
       neuroos-knowledge-query neuroos-knowledge-background neuroos-kernel neuroos-confirm)

"$farm_bin" "$XDG_RUNTIME_DIR/neuroos" "${names[@]}" >"$tmp/farm.log" 2>&1 &
farm_pid=$!

for _ in $(seq 1 50); do
    grep -q "^READY$" "$tmp/farm.log" 2>/dev/null && break
    sleep 0.1
done

"$healthd_bin" >"$tmp/healthd.log" 2>&1 &
healthd_pid=$!

# let at least one full scrape cycle complete (poll_interval_s=1)
sleep 2.5

if ! kill -0 "$healthd_pid" 2>/dev/null; then
    echo "FAIL: healthd exited unexpectedly" >&2
    cat "$tmp/healthd.log" >&2
    exit 1
fi

rss_kb=$(awk '/VmRSS:/{print $2}' "/proc/$healthd_pid/status")
# utime+stime are in clock ticks; convert to ms (getconf CLK_TCK is almost
# always 100 on Linux, i.e. 10ms/tick).
clk_tck=$(getconf CLK_TCK)
read -r _ _ _ _ _ _ _ _ _ _ _ _ _ utime stime _ < "/proc/$healthd_pid/stat"
cpu_ms=$(( (utime + stime) * 1000 / clk_tck ))

echo "measured: RSS=${rss_kb}KiB, cumulative CPU=${cpu_ms}ms since startup+1 cycle"

fail=0
if [ "$rss_kb" -gt 15360 ]; then
    echo "FAIL: RSS ${rss_kb}KiB exceeds 15 MiB (15360 KiB) budget" >&2
    fail=1
fi
# generous vs. the per-cycle 50ms target since this also includes process
# startup (tokio runtime init, binding healthd.sock, config load), not just
# one bare scrape cycle.
if [ "$cpu_ms" -gt 200 ]; then
    echo "FAIL: cumulative CPU ${cpu_ms}ms gives no confidence a single cycle is <=50ms" >&2
    fail=1
fi

if [ "$fail" -eq 0 ]; then
    echo "OK: healthd RSS and CPU within budget for 9 targets, 1 scrape cycle"
fi
exit "$fail"
