# top-level C++ build (libneuroos, neuroos-inference, neuroos-voice)
cmake_minimum_required(VERSION 3.28)
project(neuroos-cpp CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

add_subdirectory(libneuroos)
add_subdirectory(neuroos-inference)
add_subdirectory(neuroos-voice)
