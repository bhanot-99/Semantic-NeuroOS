#!/usr/bin/env bash
# asserts no network in isolated units
#
# SC-level check (phases.md §3.3, Phase 0 exit criteria): inside an isolated
# network namespace (what PrivateNetwork=true gives a systemd unit), curl and
# a raw TCP connect to 1.1.1.1 must fail, while a Unix domain socket
# (filesystem-based, unaffected by the network namespace) must still work.
#
# DNS is checked separately and twice: netns isolation ALONE does not block
# it, because glibc's `resolve` NSS module talks to systemd-resolved over a
# local socket at /run/systemd/resolve/ rather than the network — confirmed
# empirically while building this script (see memory.md decisions log,
# 2026-09-26). The fix is masking that path (InaccessiblePaths= in the real
# units, Architecture.md §8.1); this script proves both the leak and the fix.
set -euo pipefail

if [ "${CHECK_EGRESS_INSIDE_NETNS:-}" != "1" ]; then
    export CHECK_EGRESS_INSIDE_NETNS=1
    exec unshare --user --net --mount --map-root-user -- "$0" "$@"
fi

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

if curl -s --max-time 2 https://1.1.1.1/ >/dev/null 2>&1; then
    fail "curl reached the network from inside an isolated netns"
fi

if timeout 2 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then
    fail "raw TCP connect succeeded from inside an isolated netns"
fi

if ! getent hosts example.com >/dev/null 2>&1; then
    fail "DNS resolved even though /run/systemd/resolve was not yet masked (test setup is wrong)"
fi

mount -t tmpfs tmpfs /run/systemd/resolve
if getent hosts example.com >/dev/null 2>&1; then
    fail "DNS resolution still succeeded after masking /run/systemd/resolve"
fi

sock="$(mktemp -u)"
trap 'rm -f "$sock"' EXIT

python3 - "$sock" <<'PY' &
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
s.listen(1)
conn, _ = s.accept()
conn.recv(5)
conn.close()
PY
server_pid=$!
sleep 0.3

if ! timeout 2 python3 -c "
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
s.sendall(b'hello')
" "$sock"; then
    fail "UDS connect failed inside an isolated netns (should be unaffected)"
fi
wait "$server_pid"

echo "OK: curl/raw-TCP blocked by netns isolation; DNS blocked only once /run/systemd/resolve is masked; UDS still works"
