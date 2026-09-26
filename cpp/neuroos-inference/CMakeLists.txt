# neuroos-inference [C4] build definition
add_executable(neuroos-inference
    src/main.cpp
    src/engine.cpp
    src/lanes.cpp
    src/grammar.cpp
    src/ring.cpp
)
target_link_libraries(neuroos-inference PRIVATE libneuroos)
