"""L15: fail the wheel build when the generated protos are missing.

`src/neuroos/v1/*_pb2.py` is produced by `just proto-py` (protoc has no
build-time hook on the Python side the way Rust's `build.rs` and CMake's
custom command do) and is gitignored. The wheel's `packages` list included
`src/neuroos`, so a build on a tree where `proto-py` had not run produced a
perfectly valid wheel containing an empty `neuroos` package -- and the C5b
service installed from it died with an `ImportError` on its first
`neuroos.v1.*` import, in production, at startup.

This hook turns that into a build failure naming the fix.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

# hatchling is a build-system requirement, not a dev dependency, so mypy
# resolves it through the `[[tool.mypy.overrides]]` for `hatchling.*` in
# pyproject.toml and sees the base class as `Any`.
from hatchling.builders.hooks.plugin.interface import BuildHookInterface

# Relative to this file: the repo's proto directory and the generated output.
PROTO_DIR = Path(__file__).resolve().parents[2] / "proto" / "neuroos" / "v1"
GENERATED_DIR = Path(__file__).resolve().parent / "src" / "neuroos" / "v1"


class ProtoGenerationCheckHook(BuildHookInterface):  # type: ignore[misc]
    PLUGIN_NAME = "custom"

    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        protos = sorted(PROTO_DIR.glob("*.proto"))
        if not protos:
            raise RuntimeError(
                f"no .proto files found under {PROTO_DIR}; this hook's path assumption is wrong"
            )
        missing = [
            f"{proto.stem}_pb2.py"
            for proto in protos
            if not (GENERATED_DIR / f"{proto.stem}_pb2.py").is_file()
        ]
        if missing:
            raise RuntimeError(
                "refusing to build a wheel without the generated protos.\n"
                f"missing from {GENERATED_DIR}: {', '.join(missing)}\n"
                "run `just proto-py` (or `just build-py`, which runs it) first."
            )
