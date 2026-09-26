# libneuroos build definition
add_library(libneuroos STATIC
    src/libneuroos.cpp
)
target_include_directories(libneuroos PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)
target_link_libraries(libneuroos PUBLIC neuroos-proto-cpp)
