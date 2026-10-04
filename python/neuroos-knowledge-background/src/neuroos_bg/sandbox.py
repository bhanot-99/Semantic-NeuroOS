"""Landlock filesystem sandbox for the cold worker (Architecture.md AP-1,
§8.2's C5b row; BUGS.md H15).

Same semantics as `crates/neuroos-sandbox`: once `restrict_self` returns,
the kernel lets this process read only the Python installation it runs
from (stdlib, venv, its own package) plus the baseline system paths, and
write only its runtime dir -- it reaches stored data solely through
`storage.sock` (AB-1/AB-2), which Landlock does not restrict. Fail closed:
anything short of full ABI-v5 enforcement raises `SandboxError`.

Landlock restricts the calling thread and everything it creates
afterwards; the worker is single-threaded, so call this from `main`
before the job loop.
"""

from __future__ import annotations

import ctypes
import os
import sys
from pathlib import Path

# x86_64 and aarch64 share these numbers (the generic syscall table).
_SYS_CREATE_RULESET = 444
_SYS_ADD_RULE = 445
_SYS_RESTRICT_SELF = 446
_CREATE_RULESET_VERSION = 1 << 0
_RULE_PATH_BENEATH = 1
_PR_SET_NO_NEW_PRIVS = 38

_ABI_VERSION = 5  # Linux 6.10: every filesystem right up to IOCTL_DEV
_EXECUTE = 1 << 0
_WRITE_FILE = 1 << 1
_READ_FILE = 1 << 2
_READ_DIR = 1 << 3
_TRUNCATE = 1 << 14
_IOCTL_DEV = 1 << 15
ALL_RIGHTS = (1 << 16) - 1
READ_RIGHTS = _EXECUTE | _READ_FILE | _READ_DIR
_FILE_RIGHTS = _EXECUTE | _WRITE_FILE | _READ_FILE | _TRUNCATE | _IOCTL_DEV

BASELINE_READ_ONLY = (
    "/usr/lib",
    "/lib",
    "/lib64",
    "/etc/ld.so.cache",
    "/proc",
    "/sys/devices/system/cpu",
    "/sys/fs/cgroup",
    "/etc/localtime",
    "/usr/share/zoneinfo",
    "/dev/urandom",
)


class SandboxError(RuntimeError):
    """Landlock could not be fully enforced; the worker must not run."""


class _RulesetAttr(ctypes.Structure):
    _fields_ = [("handled_access_fs", ctypes.c_uint64)]


class _PathBeneathAttr(ctypes.Structure):
    _pack_ = 1
    _fields_ = [("allowed_access", ctypes.c_uint64), ("parent_fd", ctypes.c_int32)]


def _libc() -> ctypes.CDLL:
    return ctypes.CDLL(None, use_errno=True)


def _check(result: int, what: str) -> int:
    if result < 0:
        err = ctypes.get_errno()
        raise SandboxError(f"{what}: {os.strerror(err)}")
    return result


def python_read_only_paths() -> list[str]:
    """Where this interpreter and its imports live: prefixes plus every
    existing `sys.path` entry (stdlib, site-packages, the package itself)."""
    paths = {sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix}
    paths.update(p for p in sys.path if p)
    return sorted(p for p in paths if os.path.exists(p))


def restrict_self(read_only: list[str], read_write: list[str]) -> None:
    """Lock this thread (and later threads/children) to the given paths, on
    top of `BASELINE_READ_ONLY`. Missing paths are skipped."""
    libc = _libc()
    abi = libc.syscall(_SYS_CREATE_RULESET, None, 0, _CREATE_RULESET_VERSION)
    if abi < _ABI_VERSION:
        raise SandboxError(
            "Landlock is unavailable or older than ABI v5; refusing to run unsandboxed"
        )
    attr = _RulesetAttr(handled_access_fs=ALL_RIGHTS)
    ruleset = _check(
        libc.syscall(_SYS_CREATE_RULESET, ctypes.byref(attr), ctypes.sizeof(attr), 0),
        "landlock_create_ruleset",
    )
    try:
        for paths, rights in (
            ((*BASELINE_READ_ONLY, *read_only), READ_RIGHTS),
            (("/dev/null", *read_write), ALL_RIGHTS),
        ):
            for path in paths:
                _add_path(libc, ruleset, path, rights)
        _check(libc.prctl(_PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), "prctl(PR_SET_NO_NEW_PRIVS)")
        _check(libc.syscall(_SYS_RESTRICT_SELF, ruleset, 0), "landlock_restrict_self")
    finally:
        os.close(ruleset)


def _add_path(libc: ctypes.CDLL, ruleset: int, path: str, rights: int) -> None:
    try:
        fd = os.open(path, os.O_PATH | os.O_CLOEXEC)
    except FileNotFoundError:
        return
    try:
        if not Path(path).is_dir():
            rights &= _FILE_RIGHTS
        rule = _PathBeneathAttr(allowed_access=rights, parent_fd=fd)
        _check(
            libc.syscall(_SYS_ADD_RULE, ruleset, _RULE_PATH_BENEATH, ctypes.byref(rule), 0),
            f"landlock_add_rule {path}",
        )
    finally:
        os.close(fd)
