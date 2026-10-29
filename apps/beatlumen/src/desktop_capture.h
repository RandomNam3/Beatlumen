// Other apps' playback on the desktop ("Visualize other apps"): records what
// the computer plays and follows the playing app's media session for
// title, cover and transport controls.
// Linux: the monitor source of the default output (PulseAudio / PipeWire)
// and MPRIS players on the session bus.
#pragma once

#include "core.h"

class Bridge;

namespace desktopcapture {

// Sets the core's capture callback where the platform supports it.
void configure(BlCallbacks &cb, Bridge *bridge);

} // namespace desktopcapture
