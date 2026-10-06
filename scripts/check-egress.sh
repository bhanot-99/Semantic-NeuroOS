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

# L9: this used to assert `getent hosts example.com` SUCCEEDS here, to show
# the leak before masking. That made the whole script require working
# internet DNS -- it failed on an offline machine with "test setup is
# wrong" -- which breaks rules.md §7.4 ("no test may use the internet").
#
# The leak being proved is a *socket*, not a name: glibc's `resolve` NSS
# module reaches systemd-resolved over /run/systemd/resolve/io.systemd.Resolve,
# a filesystem path the network namespace does not touch. So the check is
# that the socket is reachable before masking and gone after -- the same
# property, with no network at all.
resolve_sock=/run/systemd/resolve/io.systemd.Resolve

can_reach_resolved() {
    timeout 3 python3 -c "
import socket, sys
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(2)
    s.connect(sys.argv[1])
    s.close()
except OSError:
    sys.exit(1)
sys.exit(0)
" "$resolve_sock"
}

if [ ! -S "$resolve_sock" ]; then
    echo "SKIP: $resolve_sock does not exist (systemd-resolved is not running); nothing to mask" >&2
elif ! can_reach_resolved; then
    fail "$resolve_sock exists but is unreachable before masking (test setup is wrong)"
else
    # The leak is real: a netns-isolated unit can still talk to resolved.
    #
    # If this machine happens to be online, also show the leak end to end
    # by resolving a name through it. Offline that check is simply not
    # run -- it is never a failure (rules.md §7.4).
    if getent hosts example.com >/dev/null 2>&1; then
        resolved_a_name=1
    else
        resolved_a_name=0
        echo "note: no upstream DNS reachable, so only the resolved socket leak is shown" >&2
    fi

    mount -t tmpfs tmpfs /run/systemd/resolve

    if [ -S "$resolve_sock" ] && can_reach_resolved; then
        fail "$resolve_sock is still reachable after masking /run/systemd/resolve"
    fi
    if [ "$resolved_a_name" = 1 ] && getent hosts example.com >/dev/null 2>&1; then
        fail "DNS resolution still succeeded after masking /run/systemd/resolve"
    fi
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

echo "OK: curl/raw-TCP blocked by netns isolation; the systemd-resolved socket leak is closed only once /run/systemd/resolve is masked; UDS still works"
