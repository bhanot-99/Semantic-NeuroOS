#!/usr/bin/env bash
# Phase 4 SC exit criterion (phases.md §7.3): "Landlock denies reads outside
# allowed paths" -- the systemd half. Architecture.md §8 has two
# independent layers: this script proves the unit-hardening layer
# (ProtectHome=tmpfs + BindPaths, which systemd implements with mount
# namespaces -- not Landlock) from deploy/systemd/neuroos-storage.service
# denies a read outside the allowed paths and allows one inside them, run
# for real via `systemd-run --user` (same precedent as
# tests/contract/sandboxed_echo.sh). The in-process Landlock layer (H15,
# crates/neuroos-sandbox) is proven by tests/contract/landlock_services.sh.
set -euo pipefail

if ! systemctl --user status >/dev/null 2>&1; then
    echo "SKIP: no systemd --user session available (needs a real login session)"
    exit 0
fi

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unit="neuroos-storage-landlock-test-$$"

# A file inside the one BindPaths directory this unit's real service
# config grants (%h/.local/share/neuroos/storage) must be readable...
allowed_dir="$HOME/.local/share/neuroos/storage"
mkdir -p "$allowed_dir"
echo "allowed-content" > "$allowed_dir/landlock-probe.txt"
trap 'rm -f "$allowed_dir/landlock-probe.txt"; rm -rf "$tmp"' EXIT

# ...while a real file in the user's actual home directory outside any
# BindPaths (ProtectHome=tmpfs replaces $HOME with an empty tmpfs for the
# unit) must not be.
outside_marker="$HOME/.neuroos-landlock-outside-probe-$$"
echo "must-not-be-readable" > "$outside_marker"
trap 'rm -f "$allowed_dir/landlock-probe.txt" "$outside_marker"; rm -rf "$tmp"' EXIT

systemd-run --user --unit="$unit" --pipe \
    -p ProtectSystem=strict \
    -p ProtectHome=tmpfs \
    -p "BindPaths=$allowed_dir" \
    -p NoNewPrivileges=true \
    -- bash -c "
        if cat '$allowed_dir/landlock-probe.txt' >/dev/null 2>&1; then
            echo ALLOWED_PATH_READABLE
        else
            echo ALLOWED_PATH_DENIED
        fi
        if cat '$outside_marker' >/dev/null 2>&1; then
            echo OUTSIDE_PATH_READABLE
        else
            echo OUTSIDE_PATH_DENIED
        fi
    " > "$tmp/stdout.log" 2>"$tmp/stderr.log" || true

cat "$tmp/stdout.log"

grep -q "^ALLOWED_PATH_READABLE$" "$tmp/stdout.log" ||
    fail "a file inside the unit's own BindPaths must be readable"
grep -q "^OUTSIDE_PATH_DENIED$" "$tmp/stdout.log" ||
    fail "a file outside BindPaths (real \$HOME, hidden by ProtectHome=tmpfs) must NOT be readable"

echo "OK: systemd unit hardening (ProtectHome=tmpfs + BindPaths) denies reads outside BindPaths, allows reads inside them"
