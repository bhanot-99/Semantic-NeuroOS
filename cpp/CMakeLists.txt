# top-level C++ build (libneuroos, neuroos-inference, neuroos-voice)
cmake_minimum_required(VERSION 3.28)
project(neuroos-cpp CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

# neuroos.v1 proto bindings, generated at configure/build time into the build dir
find_package(Protobuf REQUIRED)
find_package(Threads REQUIRED)

set(NEUROOS_PROTO_DIR ${CMAKE_SOURCE_DIR}/../proto)
file(GLOB NEUROOS_PROTO_FILES ${NEUROOS_PROTO_DIR}/neuroos/v1/*.proto)
set(NEUROOS_PROTO_GEN_DIR ${CMAKE_BINARY_DIR}/proto-gen)
file(MAKE_DIRECTORY ${NEUROOS_PROTO_GEN_DIR})

set(NEUROOS_PROTO_SRCS "")
set(NEUROOS_PROTO_HDRS "")
foreach(proto_file ${NEUROOS_PROTO_FILES})
    get_filename_component(proto_name ${proto_file} NAME_WE)
    list(APPEND NEUROOS_PROTO_SRCS ${NEUROOS_PROTO_GEN_DIR}/neuroos/v1/${proto_name}.pb.cc)
    list(APPEND NEUROOS_PROTO_HDRS ${NEUROOS_PROTO_GEN_DIR}/neuroos/v1/${proto_name}.pb.h)
endforeach()

add_custom_command(
    OUTPUT ${NEUROOS_PROTO_SRCS} ${NEUROOS_PROTO_HDRS}
    COMMAND protobuf::protoc
    ARGS --proto_path=${NEUROOS_PROTO_DIR}
         --cpp_out=${NEUROOS_PROTO_GEN_DIR}
         ${NEUROOS_PROTO_FILES}
    DEPENDS ${NEUROOS_PROTO_FILES}
    COMMENT "Generating neuroos.v1 C++ proto bindings"
    VERBATIM
)

add_library(neuroos-proto-cpp STATIC ${NEUROOS_PROTO_SRCS})
target_include_directories(neuroos-proto-cpp PUBLIC ${NEUROOS_PROTO_GEN_DIR})
target_link_libraries(neuroos-proto-cpp PUBLIC protobuf::libprotobuf)

add_subdirectory(libneuroos)
add_subdirectory(neuroos-inference)
add_subdirectory(neuroos-voice)

# tests/contract/: cross-language proto round-trip helper (P0-S02)
add_executable(contract-codec-cpp ${CMAKE_SOURCE_DIR}/../tests/contract/cpp_codec.cpp)
target_link_libraries(contract-codec-cpp PRIVATE neuroos-proto-cpp)

# tests/contract/: cross-language memfd ring interop tools (P0-S07)
add_executable(shm-writer-cpp ${CMAKE_SOURCE_DIR}/../tests/contract/shm_writer.cpp)
target_link_libraries(shm-writer-cpp PRIVATE libneuroos)

add_executable(shm-reader-cpp ${CMAKE_SOURCE_DIR}/../tests/contract/shm_reader.cpp)
target_link_libraries(shm-reader-cpp PRIVATE libneuroos)

add_executable(shm-stress-cpp ${CMAKE_SOURCE_DIR}/../tests/contract/shm_stress.cpp)
target_link_libraries(shm-stress-cpp PRIVATE libneuroos Threads::Threads)
