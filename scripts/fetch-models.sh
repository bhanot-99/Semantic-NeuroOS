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
#
# Entries with `archive = true` (e.g. ONNX Runtime's own release tarball —
# Architecture.md §12.3: "pinned release tarball with SHA-256", the same
# convention as every model here, just not itself a model) are additionally
# extracted into `extract_to` (relative to the same destination root) after
# the tarball's own sha256 verifies.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/models/manifest.toml"
dest_root="${NEUROOS_MODELS_DIR:-/opt/neuroos/models}"

python3 - "$manifest" "$dest_root" <<'PY'
import hashlib
import pathlib
import re
import sys
import tarfile
import time
import tomllib
import urllib.request

manifest_path, dest_root = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    manifest = tomllib.load(f)

# L10: `urlopen` with no timeout blocks forever on a server that accepts the
# connection and then stops sending, so an operator's `just fetch-models`
# could hang indefinitely with no output. SOCKET_TIMEOUT_S bounds any single
# socket operation (connect, and each read); TOTAL_TIMEOUT_S bounds the whole
# transfer, which a server trickling one byte per timeout window would
# otherwise evade. The largest entry is a few GB, so an hour is generous.
SOCKET_TIMEOUT_S = 60.0
TOTAL_TIMEOUT_S = 3600.0
CHUNK = 1 << 20

SHA256_RE = re.compile(r"\A[0-9a-f]{64}\Z")


class DownloadTimeout(Exception):
    pass


def sha256_of(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(CHUNK), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url: str, out_path: pathlib.Path) -> None:
    """Streams `url` to `out_path` under both timeouts (L10)."""
    req = urllib.request.Request(url, headers={"User-Agent": "neuroos-fetch-models/1"})
    started = time.monotonic()
    with urllib.request.urlopen(req, timeout=SOCKET_TIMEOUT_S) as resp:
        with open(out_path, "wb") as out:
            while True:
                if time.monotonic() - started > TOTAL_TIMEOUT_S:
                    raise DownloadTimeout(
                        f"download exceeded the {TOTAL_TIMEOUT_S:.0f}s total budget"
                    )
                chunk = resp.read(CHUNK)
                if not chunk:
                    return
                out.write(chunk)


failures = []
for m in manifest["model"]:
    # L10: a `PLACEHOLDER` (or otherwise malformed) sha256 used to mean
    # "download it and install it unverified" -- the one path in this
    # script where a model file reached /opt/neuroos/models with no
    # integrity check at all, which is exactly what it exists to prevent.
    # No manifest entry uses it any more, so it is now an error, and
    # `just lint-manifest` rejects it at the schema level too.
    if not SHA256_RE.match(m["sha256"]):
        print(f"FAIL {m['name']}: sha256 is not 64 lowercase hex chars ({m['sha256']!r})")
        failures.append(m["name"])
        continue

    dest = pathlib.Path(dest_root) / m["dest"]
    dest.parent.mkdir(parents=True, exist_ok=True)
    is_archive = m.get("archive", False)
    extract_dir = pathlib.Path(dest_root) / m["extract_to"] if is_archive else None

    already_cached = dest.exists() and sha256_of(dest) == m["sha256"]
    if already_cached and (not is_archive or (extract_dir / m["verify_file"]).exists()):
        print(f"OK  (cached) {m['name']}")
        continue

    if not already_cached:
        print(f"FETCH {m['name']} <- {m['url']}")
        tmp = dest.with_name(dest.name + ".part")
        try:
            download(m["url"], tmp)
        except Exception as e:  # report and continue with the remaining models
            print(f"FAIL {m['name']}: download error: {e}")
            failures.append(m["name"])
            tmp.unlink(missing_ok=True)
            continue

        digest = sha256_of(tmp)
        if digest != m["sha256"]:
            tmp.unlink()
            failures.append(m["name"])
            print(f"FAIL {m['name']}: sha256 mismatch (expected {m['sha256']}, got {digest})")
            continue

        tmp.rename(dest)
        print(f"OK  {m['name']} sha256={digest}")

    if is_archive:
        print(f"EXTRACT {m['name']} -> {extract_dir}")
        extract_dir.mkdir(parents=True, exist_ok=True)
        with tarfile.open(dest) as tar:
            tar.extractall(extract_dir, filter="data")
        if not (extract_dir / m["verify_file"]).exists():
            failures.append(m["name"])
            print(f"FAIL {m['name']}: expected file {m['verify_file']} missing after extraction")
            continue
        print(f"OK  {m['name']} extracted")

if failures:
    print(f"FAILED to fetch/verify: {failures}", file=sys.stderr)
    sys.exit(1)

print(f"All {len(manifest['model'])} entries present and verified under {dest_root}")
PY
