// Microphones on the desktop (karaoke): lists the computer's audio inputs
// and records the ones the user opens, each on its own thread, into the
// core (bl_mic_push). Nothing is recorded unless an input is opened.
// Linux: PulseAudio / PipeWire sources without the monitors, through
// libpulse loaded at run time. Windows: WASAPI capture endpoints.
#pragma once

#include "core.h"

class Bridge;

namespace micinput {

// Sets the core's microphone callback where the platform has inputs.
void configure(BlCallbacks &cb, Bridge *bridge);

} // namespace micinput
