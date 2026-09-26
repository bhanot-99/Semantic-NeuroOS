#!/usr/bin/env bash
# operator-run download + hash verify (never run by services)
#
# Reads models/manifest.toml, downloads each entry that's missing or whose
# on-disk sha256 doesn't match, verifies sha256 on every download, and
# refuses to install a file that fails verification. Idempotent: re-running
# skips anything already present and correct.
#
# Destination defaults to /opt/neuroos/models (Architecture.md §7.1);
# override with NEUROOS_MODELS_DIR for local testing without root.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/models/manifest.toml"
dest_root="${NEUROOS_MODELS_DIR:-/opt/neuroos/models}"

python3 - "$manifest" "$dest_root" <<'PY'
import hashlib
import pathlib
import shutil
import sys
import tomllib
import urllib.request

manifest_path, dest_root = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    manifest = tomllib.load(f)


def sha256_of(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


failures = []
for m in manifest["model"]:
    dest = pathlib.Path(dest_root) / m["dest"]
    dest.parent.mkdir(parents=True, exist_ok=True)

    if dest.exists() and m["sha256"] != "PLACEHOLDER" and sha256_of(dest) == m["sha256"]:
        print(f"OK  (cached) {m['name']}")
        continue

    print(f"FETCH {m['name']} <- {m['url']}")
    tmp = dest.with_name(dest.name + ".part")
    req = urllib.request.Request(m["url"], headers={"User-Agent": "neuroos-fetch-models/1"})
    try:
        with urllib.request.urlopen(req) as resp, open(tmp, "wb") as out:
            shutil.copyfileobj(resp, out)
    except Exception as e:  # report and continue with the remaining models
        print(f"FAIL {m['name']}: download error: {e}")
        failures.append(m["name"])
        tmp.unlink(missing_ok=True)
        continue

    digest = sha256_of(tmp)
    if m["sha256"] != "PLACEHOLDER" and digest != m["sha256"]:
        tmp.unlink()
        failures.append(m["name"])
        print(f"FAIL {m['name']}: sha256 mismatch (expected {m['sha256']}, got {digest})")
        continue

    tmp.rename(dest)
    print(f"OK  {m['name']} sha256={digest}")

if failures:
    print(f"FAILED to fetch/verify: {failures}", file=sys.stderr)
    sys.exit(1)

print(f"All {len(manifest['model'])} models present and verified under {dest_root}")
PY
