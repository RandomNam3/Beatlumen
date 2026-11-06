// A sound card in direct use: the samples reach it as they are (Linux).
#pragma once

#include "core.h"

class Bridge;

namespace directaudio {

void configure(BlCallbacks &cb, Bridge *bridge);
void shutdown();

} // namespace directaudio
