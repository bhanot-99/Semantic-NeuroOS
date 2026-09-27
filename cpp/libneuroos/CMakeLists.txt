# libneuroos build definition
add_library(libneuroos STATIC
    src/libneuroos.cpp
    src/shm.cpp
    src/framing.cpp
    src/peercred.cpp
    src/uds.cpp
    src/health_server.cpp
    src/sha256.cpp
    src/paths.cpp
)
find_package(Threads REQUIRED)
find_package(OpenSSL REQUIRED)
target_link_libraries(libneuroos PUBLIC Threads::Threads OpenSSL::Crypto)
target_include_directories(libneuroos PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)
target_link_libraries(libneuroos PUBLIC neuroos-proto-cpp)
