//! Speech recognition with Whisper models (ggml files as published for
//! whisper.cpp), implemented here without external libraries: log-mel
//! spectrogram, transformer encoder and decoder, timestamped decoding.
//! Used to time song texts (see `align`).

pub mod align;
pub mod mel;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod models;
pub mod net;
pub mod ops;
pub mod sha256;
pub mod transcribe;
