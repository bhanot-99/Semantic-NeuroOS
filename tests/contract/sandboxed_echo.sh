#!/usr/bin/env bash
# Phase 0 exit criterion (phases.md §3.4): "a sandboxed echo unit proves: no
# network, UDS works, peer UID enforced" — combined, not tested piecemeal.
# Runs the real echo binary as a real systemd --user unit with
# PrivateNetwork=true (the mode ADR-0002/D-11 recommends), connects a real
# client from outside the unit, and checks all three properties together.
set -euo pipefail

if ! systemctl --user status >/dev/null 2>&1; then
    echo "SKIP: no systemd --user session available (needs a real login session)"
    exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin="$root/target/debug/sandboxed_echo"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
sock="$tmp/echo.sock"
uid="$(id -u)"
unit="neuroos-sandboxed-echo-test-$$"

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

systemd-run --user -p PrivateNetwork=yes --unit="$unit" --pipe -- "$bin" "$sock" "$uid" \
    >"$tmp/stdout.log" 2>"$tmp/stderr.log" &
systemd_pid=$!

for _ in $(seq 1 100); do
    grep -q "^READY$" "$tmp/stdout.log" 2>/dev/null && break
    grep -q "^NETWORK_REACHABLE" "$tmp/stdout.log" 2>/dev/null &&
        fail "network was reachable inside the PrivateNetwork=true unit"
    sleep 0.1
done
grep -q "^NETWORK_BLOCKED$" "$tmp/stdout.log" || fail "never saw NETWORK_BLOCKED from inside the sandbox"
grep -q "^READY$" "$tmp/stdout.log" || fail "echo server never reported READY"

for _ in $(seq 1 50); do
    [ -S "$sock" ] && break
    sleep 0.1
done
[ -S "$sock" ] || fail "socket file never appeared"

# Connect from OUTSIDE the sandboxed unit: the UDS socket must still work
# even though the unit's network is isolated (Architecture.md §5.1: UDS is
# filesystem-based, unaffected by network namespace isolation).
payload="phase0 exit criterion: sandboxed echo"
reply="$(python3 -c "
import socket, struct, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
data = sys.argv[2].encode()
s.sendall(struct.pack('<I', len(data)) + data)
lenbuf = s.recv(4)
n = struct.unpack('<I', lenbuf)[0]
got = b''
while len(got) < n:
    got += s.recv(n - len(got))
sys.stdout.buffer.write(got)
" "$sock" "$payload")"
[ "$reply" = "$payload" ] || fail "echoed payload didn't match (got: $reply)"

wait "$systemd_pid" 2>/dev/null || true

grep -q "^PEER_UID=$uid (allowed=true)$" "$tmp/stdout.log" || fail "server did not recognize our real UID as allowed"
grep -q "^ECHO_OK$" "$tmp/stdout.log" || fail "server never reported ECHO_OK"

echo "OK: sandboxed echo unit proved together: network blocked, UDS works, peer UID ($uid) enforced"
