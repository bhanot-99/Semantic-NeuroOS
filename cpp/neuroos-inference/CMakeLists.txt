# neuroos-inference [C4] build definition

# BitNet's own fork of llama.cpp/ggml (cpp/third_party/bitnet.cpp/3rdparty/
# llama.cpp — the i2_s quantization + AVX2 kernels ADR-0006's spike proved
# live on this machine, not vanilla upstream llama.cpp). Only C4 needs it,
# so it's built here rather than at the top level. `add_subdirectory`ing it
# (instead of building bitnet.cpp's own top CMakeLists.txt, which forces
# LLAMA_BUILD_SERVER=ON unconditionally) makes llama.cpp's own
# LLAMA_STANDALONE detection default every CLI/server/test/example target
# to OFF automatically, since this isn't the top-level CMake project — the
# explicit OFFs below are belt-and-suspenders in case that default ever
# changes upstream. This fork's ggml target compiles bitnet.cpp's
# src/ggml-bitnet-{mad,lut}.cpp (the i2_s kernels) directly, so bitnet.cpp's
# own top-level CMakeLists.txt isn't needed.
set(LLAMA_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_SERVER OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_APP OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_COMMON OFF CACHE BOOL "" FORCE) # GBNF grammar is in core src/, not common/
set(GGML_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(GGML_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)

# BUG-007(d): pinned to bitnet.cpp 01eb415 (llama.cpp merge-dev fork); the
# later July 2026 re-port computes wrong results for BitNet 2B4T (see
# BUGS.md). This fork's ggml lists include/bitnet-lut-kernels.h as a source.
# setup_env.py normally generates it, but its whole body is guarded by
# GGML_BITNET_X86_TL2, so an empty placeholder is exact for i2_s.
#
# C10: this writes into the submodule worktree, which is not something a build
# should do. It cannot be relocated: ggml's own CMakeLists.txt lists the header
# by a fixed relative path in `add_library(ggml ...)`
# (3rdparty/llama.cpp/ggml/src/CMakeLists.txt, "../../../../include/
# bitnet-lut-kernels.h"), and CMake hard-errors on a source file that does not
# exist, so the file has to be at exactly that path. Generating it elsewhere
# would mean patching the vendored tree, which is a bigger liability than one
# untracked placeholder. `.gitmodules` therefore sets `ignore = untracked` for
# this submodule so the write cannot show up as a dirty worktree; revisit if
# upstream ever makes the header optional.
set(BITNET_LUT_HEADER ${CMAKE_SOURCE_DIR}/third_party/bitnet.cpp/include/bitnet-lut-kernels.h)
if(NOT EXISTS ${BITNET_LUT_HEADER})
    file(WRITE ${BITNET_LUT_HEADER}
        "// placeholder written by cpp/neuroos-inference/CMakeLists.txt: TL2 kernels unused (i2_s)\n")
endif()

# SYSTEM (CMake 3.25+): llama/ggml's own headers are included by our
# translation units, and ggml-backend.h trips -Wshadow. Treating them as system
# headers keeps C6's -Werror on our code without making the build hostage to a
# vendored upstream we do not control.
add_subdirectory(
    ${CMAKE_SOURCE_DIR}/third_party/bitnet.cpp/3rdparty/llama.cpp
    ${CMAKE_BINARY_DIR}/neuroos-llama
    EXCLUDE_FROM_ALL
    SYSTEM
)
# Same flag bitnet.cpp's own top-level CMakeLists.txt adds for GCC:
# src/ggml-bitnet-mad.cpp drops a const qualifier in one pointer init.
target_compile_options(ggml PRIVATE $<$<COMPILE_LANG_AND_ID:CXX,GNU>:-fpermissive>)

find_package(spdlog REQUIRED)
find_package(toml11 REQUIRED)

add_executable(neuroos-inference
    src/main.cpp
    src/bench.cpp
    src/config.cpp
    src/engine.cpp
    src/grammar.cpp
    src/lanes.cpp
    src/ring.cpp
    src/server.cpp
)
target_link_libraries(neuroos-inference PRIVATE libneuroos llama spdlog::spdlog toml11::toml11 neuroos-warnings)
