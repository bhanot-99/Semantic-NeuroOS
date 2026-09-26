# build / test / lint / ci / bench entry points
set shell := ["bash", "-euo", "pipefail", "-c"]

py_dir := "python/neuroos-knowledge-background"
py_src_dir := py_dir / "src"
cpp_build := "cpp/build"

# --- proto -----------------------------------------------------------------

# Rust (prost, via build.rs) and C++ (via CMake custom command) regenerate
# automatically on every `cargo build` / `cmake --build`. Python does not have
# an equivalent hook, so it gets its own recipe. Output lands at src/neuroos/v1/
# (top-level, matching protoc's own import scheme), imported as `neuroos.v1.*`.
proto: proto-py

proto-py:
    protoc --proto_path=proto --python_out={{py_src_dir}} proto/neuroos/v1/*.proto

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

test: test-rust test-py test-contract

test-rust:
    cargo nextest run --workspace --no-tests=warn

test-py: proto-py
    cd {{py_dir}} && uv run pytest || test "$?" -eq 5

test-contract: build-rust build-cpp build-py
    bash tests/contract/roundtrip.sh

# --- format ----------------------------------------------------------------

fmt:
    cargo fmt --all
    find cpp -path {{cpp_build}} -prune -o \( -name '*.cpp' -o -name '*.hpp' -o -name '*.h' \) -print | xargs -r clang-format-18 -i
    cd {{py_dir}} && uv run ruff format

fmt-check:
    cargo fmt --all -- --check
    find cpp -path {{cpp_build}} -prune -o \( -name '*.cpp' -o -name '*.hpp' -o -name '*.h' \) -print | xargs -r clang-format-18 --dry-run --Werror
    cd {{py_dir}} && uv run ruff format --check

# --- lint --------------------------------------------------------------------

lint: lint-rust lint-cpp lint-py lint-sh

lint-rust:
    cargo clippy --workspace --all-targets -- -D warnings

lint-cpp: build-cpp
    find cpp -path {{cpp_build}} -prune -o -name '*.cpp' -print | xargs -r clang-tidy -p {{cpp_build}}

lint-py: proto-py
    cd {{py_dir}} && uv run ruff check src
    cd {{py_dir}} && uv run mypy src

lint-sh:
    shellcheck scripts/*.sh tests/contract/*.sh

# --- aggregate ---------------------------------------------------------------

ci: fmt-check lint build test

bench:
    cargo bench --workspace

deny:
    cargo deny check
