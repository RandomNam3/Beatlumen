// A sound card in direct use: the samples reach it as they are. ALSA or PipeWire
// on Linux (audio_direct.cpp), WASAPI in exclusive mode on Windows
// (audio_direct_win.cpp), a USB device's bit-perfect mixer on Android 14 and newer
// (audio_direct_android.cpp).
#pragma once

#include "core.h"

class Bridge;

namespace directaudio {

void configure(BlCallbacks &cb, Bridge *bridge);
void shutdown();

} // namespace directaudio
