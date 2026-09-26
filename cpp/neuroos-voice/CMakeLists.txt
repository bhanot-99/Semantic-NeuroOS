# neuroos-voice [C2] build definition
# no executable target yet: entry point/main added when Phase 6 wires the daemon
add_library(neuroos-voice STATIC
    src/capture.cpp
    src/vad.cpp
    src/wakeword.cpp
    src/stt.cpp
    src/kokoro.cpp
    src/clip_cache.cpp
    src/conversation.cpp
    src/playback.cpp
    src/bargein.cpp
)
target_link_libraries(neuroos-voice PRIVATE libneuroos)
