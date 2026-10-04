"""H15: the cold worker's Landlock sandbox really denies what it doesn't
grant. Each check runs in a fresh interpreter, since restricting the test
process itself would break pytest."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from neuroos_bg import sandbox

SCRIPT = """
import sys
from neuroos_bg import sandbox
allowed, writable, outside = sys.argv[1:4]
sandbox.restrict_self(sandbox.python_read_only_paths() + [allowed], [writable])
results = []
for action in (
    lambda: open(allowed + "/a").read(),
    lambda: open(outside + "/secret").read(),
    lambda: open(allowed + "/new", "w").write("x"),
    lambda: open(writable + "/new", "w").write("x"),
):
    try:
        action()
        results.append("ok")
    except PermissionError:
        results.append("denied")
import json  # an import after restrict_self still works
print(",".join(results))
"""


def test_reads_and_writes_outside_the_policy_are_denied(tmp_path: Path) -> None:
    allowed, writable, outside = (tmp_path / n for n in ("ro", "rw", "outside"))
    for d in (allowed, writable, outside):
        d.mkdir()
    (allowed / "a").write_text("ok")
    (outside / "secret").write_text("no")

    out = subprocess.run(
        [sys.executable, "-c", SCRIPT, str(allowed), str(writable), str(outside)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()

    assert out == "ok,denied,denied,ok"


def test_python_read_only_paths_cover_the_interpreter_and_this_package() -> None:
    paths = sandbox.python_read_only_paths()
    assert sys.base_prefix in paths
    package_dir = Path(sandbox.__file__).resolve().parent
    assert any(package_dir.is_relative_to(p) for p in map(Path, paths))


def test_missing_paths_are_skipped_not_fatal() -> None:
    out = subprocess.run(
        [
            sys.executable,
            "-c",
            "from neuroos_bg import sandbox; "
            "sandbox.restrict_self(sandbox.python_read_only_paths() + ['/nonexistent/x'], "
            "['/nonexistent/y']); print('ok')",
        ],
        capture_output=True,
        text=True,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "ok"


@pytest.mark.parametrize("rights", [sandbox.READ_RIGHTS, sandbox.ALL_RIGHTS])
def test_rights_masks_stay_within_abi_v5(rights: int) -> None:
    assert rights & ~sandbox.ALL_RIGHTS == 0
