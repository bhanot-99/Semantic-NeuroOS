# build / test / lint / ci / bench entry points
set shell := ["bash", "-euo", "pipefail", "-c"]

py_dir := "python/neuroos-knowledge-background"
py_src_dir := py_dir / "src"
cpp_build := "cpp/build"
cpp_build_glob := "cpp/build*"

# --- proto -----------------------------------------------------------------

# Rust (prost, via build.rs) and C++ (via CMake custom command) regenerate
# automatically on every `cargo build` / `cmake --build`. Python does not have
# an equivalent hook, so it gets its own recipe. Output lands at src/neuroos/v1/
# (top-level, matching protoc's own import scheme), imported as `neuroos.v1.*`.
proto: proto-py

proto-py:
    protoc --proto_path=proto --python_out={{py_src_dir}} --pyi_out={{py_src_dir}} proto/neuroos/v1/*.proto

# --- build ---------------------------------------------------------------

build: build-rust build-cpp build-py

build-rust:
    cargo build --workspace

build-cpp:
    cd cpp && cmake --preset default
    cd cpp && cmake --build --preset default

build-py: proto-py
    cd {{py_dir}} && uv sync

# --- test ------------------------------------------------------------------

test: test-rust test-py test-contract test-security test-shm test-sandboxed-echo test-healthd-pf test-monitor-pf test-cpp-ipc test-inference

test-rust:
    cargo nextest run --workspace --no-tests=warn

test-py: proto-py
    cd {{py_dir}} && uv run pytest || test "$?" -eq 5

test-contract: build-rust build-cpp build-py
    bash tests/contract/roundtrip.sh

# SC (phases.md §3.3): PrivateNetwork-style isolation blocks curl/DNS/TCP, UDS still works.
test-security:
    bash scripts/check-egress.sh

# Phase 0 exit criterion (phases.md §3.4): a real systemd --user unit with
# PrivateNetwork=true proves no network / UDS works / peer UID enforced, together.
test-sandboxed-echo: build-rust
    bash tests/contract/sandboxed_echo.sh

# P1 PF (phases.md §4.3): real neuroos-healthd binary against 8 real mock
# health servers via health_mock_farm; RSS <= 15 MiB, one scrape cycle overhead low.
test-healthd-pf: build-rust
    bash tests/contract/healthd_pf.sh

# Phase 3 PF (phases.md §6.3): real neuroos-monitor release binary against
# the live Wayland/D-Bus session; RSS <= 25 MiB, idle CPU < 0.5%. Skips
# cleanly outside a graphical session (see the script).
test-monitor-pf:
    cargo build --release -p neuroos-monitor
    bash tests/contract/monitor_pf.sh

# Phase 2 prerequisite infra: libneuroos's C++ IPC framing, UDS server/client,
# SCM_RIGHTS fd passing (used to hand the token ring fd to C2) and the C++
# health server, all real UDS round trips (not mocked).
test-cpp-ipc: build-cpp
    ./cpp/build/cpp-ipc-smoke

# Phase 2 IT (phases.md §5.3): real neuroos-inference against the real
# downloaded BitNet model — GetInfo/AttachRing/Generate/Cancel over a real
# inference.sock, real tokens through a real memfd ring. Skips cleanly if
# the model hasn't been fetched yet (see the script).
test-inference: build-cpp
    bash tests/contract/inference_smoke.sh

# P0-S07 spike S-02: memfd seqlock ring, default scale + cross-language interop.
# The 10M-message and ThreadSanitizer runs are spike evidence, not routine CI
# (TSan on the Rust side needs a nightly toolchain and takes minutes; see
# memory.md tech debt and docs/adr/0005-shm-ring-race-freedom.md for how to
# run them and what they found).
test-shm: build-rust build-cpp
    cargo test -p neuroos-shm --release
    ./cpp/build/shm-stress-cpp 200000
    bash tests/contract/shm_interop.sh

# --- format ----------------------------------------------------------------

fmt:
    cargo fmt --all
    find cpp \( -path '{{cpp_build_glob}}' -o -path cpp/third_party \) -prune -o \( -name '*.cpp' -o -name '*.hpp' -o -name '*.h' \) -print | xargs -r clang-format-18 -i
    cd {{py_dir}} && uv run ruff format

fmt-check:
    cargo fmt --all -- --check
    find cpp \( -path '{{cpp_build_glob}}' -o -path cpp/third_party \) -prune -o \( -name '*.cpp' -o -name '*.hpp' -o -name '*.h' \) -print | xargs -r clang-format-18 --dry-run --Werror
    cd {{py_dir}} && uv run ruff format --check

# --- lint --------------------------------------------------------------------

lint: lint-rust lint-cpp lint-py lint-sh lint-systemd lint-manifest

lint-rust:
    cargo clippy --workspace --all-targets -- -D warnings

lint-cpp: build-cpp
    find cpp \( -path '{{cpp_build_glob}}' -o -path cpp/third_party \) -prune -o -name '*.cpp' -print | xargs -r clang-tidy -p {{cpp_build}}

lint-py: proto-py
    cd {{py_dir}} && uv run ruff check src
    cd {{py_dir}} && uv run mypy src

lint-sh:
    shellcheck scripts/*.sh tests/contract/*.sh

# P0-S09: manifest schema/field check, no network. Real fetch+verify against
# all 10 models is `just fetch-models` (several GB, not routine CI).
lint-manifest:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 - <<'PY'
    import sys, tomllib
    with open("models/manifest.toml", "rb") as f:
        manifest = tomllib.load(f)
    required = {"name", "component", "url", "dest", "sha256", "license"}
    for m in manifest["model"]:
        missing = required - m.keys()
        if missing:
            sys.exit(f"models/manifest.toml: {m.get('name', '?')} missing fields: {missing}")
        if len(m["sha256"]) != 64 and m["sha256"] != "PLACEHOLDER":
            sys.exit(f"models/manifest.toml: {m['name']} sha256 is not 64 hex chars")
    print(f"OK: {len(manifest['model'])} manifest entries well-formed")
    PY

# ExecStart binaries aren't installed on a dev checkout, so that one warning is expected
# and filtered out; anything else systemd-analyze reports fails the recipe.
lint-systemd:
    #!/usr/bin/env bash
    set -euo pipefail
    fail=0
    for f in deploy/systemd/*; do
        out="$(systemd-analyze verify --man=false "$f" 2>&1 | grep -v "not executable: No such file or directory" | grep -v "^/usr/lib/systemd/system/.*Invalid environment assignment" || true)"
        if [ -n "$out" ]; then
            echo "$out"
            fail=1
        fi
    done
    exit "$fail"

# --- aggregate ---------------------------------------------------------------

ci: fmt-check lint build test deny

bench:
    cargo bench --workspace

deny:
    cargo deny check

# P0-S09: downloads + sha256-verifies every model in models/manifest.toml.
# Several GB; not part of `just ci`. Set NEUROOS_MODELS_DIR to override the
# /opt/neuroos/models default (e.g. for a non-root local test).
fetch-models:
    bash scripts/fetch-models.sh
