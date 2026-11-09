// The bridge's sim build (pico/sim/main_sim.c) is written for emscripten. Built natively, the
// exported functions only need to stay visible in the shared library (on Windows, the build's
// export list does that).
#pragma once
#if defined(_WIN32)
#define EMSCRIPTEN_KEEPALIVE
#else
#define EMSCRIPTEN_KEEPALIVE __attribute__((visibility("default")))
#endif
