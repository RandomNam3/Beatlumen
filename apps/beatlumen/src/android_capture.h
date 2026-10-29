// Other apps' playback on Android: recording what they play
// (android/src/.../CaptureService.java) for the visuals, and their media
// sessions (MediaSessions.java) for title, cover and transport controls.
#pragma once

#include "core.h"

class Bridge;
class QJniObject;

namespace capture {

// Sets the core's capture callback (Android 10+) and registers the native
// methods of the Java side.
void configure(BlCallbacks &cb, Bridge *bridge);

// The quick settings tile opened the app to start capture: takes the flag
// from the intent (true if it was set).
bool takeTileRequest(QJniObject &intent);

} // namespace capture
