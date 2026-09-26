# build / test / lint / ci / bench entry points
set shell := ["bash", "-euo", "pipefail", "-c"]

py_dir := "python/neuroos-knowledge-background"
cpp_build := "cpp/build"

# --- build ---------------------------------------------------------------

build: build-rust build-cpp build-py

build-rust:
    cargo build --workspace

build-cpp:
    cd cpp && cmake --preset default
    cd cpp && cmake --build --preset default

build-py:
    cd {{py_dir}} && uv sync

# --- test ------------------------------------------------------------------

test: test-rust test-py

test-rust:
    cargo nextest run --workspace --no-tests=warn

test-py:
    cd {{py_dir}} && uv run pytest || test "$?" -eq 5

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

lint-py:
    cd {{py_dir}} && uv run ruff check src
    cd {{py_dir}} && uv run mypy src

lint-sh:
    shellcheck scripts/*.sh

# --- aggregate ---------------------------------------------------------------

ci: fmt-check lint build test

bench:
    cargo bench --workspace

deny:
    cargo deny check
