// Audio output. Native platforms pull audio from the core on a dedicated
// thread (Qt Multimedia); the browser build pushes it into Web Audio.
#pragma once

#include <cstdint>

#include "core.h"

class AudioOutput
{
public:
    static AudioOutput *instance();
    // Fills in the push callbacks on platforms that use push audio.
    static void configure(BlCallbacks &cb);
    // Opens the default output; returns the sample rate (0 on failure).
    uint32_t start();
    void stop();

private:
    AudioOutput() = default;
    struct Impl;
    Impl *d = nullptr;
};
