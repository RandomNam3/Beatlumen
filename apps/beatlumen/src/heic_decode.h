// HEIC pictures (HEVC in HEIF) decoded by the operating system: HEVC is
// patent-encumbered, so the core reads the container and hands every coded
// image to the decoder registered here (bl_set_heic_decoder). Linux: the
// system's libheif, if installed; Windows: Windows Imaging Component (with
// the HEIF and HEVC Video Extensions); Android 9 and newer: ImageDecoder
// (HeicDecoder.java). Elsewhere HEIC pictures are not shown.
#pragma once

// Registers the platform's decoder where there is one. Call once at start,
// before the core decodes pictures.
void registerHeicDecoder();
