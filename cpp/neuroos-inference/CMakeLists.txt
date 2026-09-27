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
# changes upstream. bitnet.cpp's own top-level `src/` directory (TL1/TL2
# kernel selection for ARM/older x86 quant formats) isn't needed: i2_s
# support lives in this fork's own ggml, not in bitnet.cpp/src's dead
# GGML_SOURCES_BITNET variable (verified never referenced anywhere in
# ggml's CMakeLists.txt).
set(LLAMA_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_SERVER OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_APP OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_COMMON OFF CACHE BOOL "" FORCE) # GBNF grammar is in core src/, not common/
set(GGML_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(GGML_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)

add_subdirectory(
    ${CMAKE_SOURCE_DIR}/third_party/bitnet.cpp/3rdparty/llama.cpp
    ${CMAKE_BINARY_DIR}/neuroos-llama
    EXCLUDE_FROM_ALL
)

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
target_link_libraries(neuroos-inference PRIVATE libneuroos llama spdlog::spdlog toml11::toml11)
