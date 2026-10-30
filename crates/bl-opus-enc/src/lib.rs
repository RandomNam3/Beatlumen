//! Ogg Opus encoding (RFC 7845) on top of the CELT-only encoder of
//! `bl-opus`, for streaming music at low bitrates (e.g. over mobile data).
//!
//! - [`OpusEncoder`] (re-exported from `bl-opus`): 48 kHz mono/stereo PCM
//!   to Opus packets, one 20 ms frame per packet, constant bitrate.
//! - [`OggOpusWriter`]: packets to an Ogg Opus stream (`OpusHead`,
//!   `OpusTags`, granule positions, end trimming, end-of-stream page).
//! - [`Transcoder`] / [`transcode`]: any `bl_media` decoder to an Ogg Opus
//!   byte stream, produced on the fly through [`std::io::Read`] with
//!   bounded memory (downmix to stereo, resampling to 48 kHz).

#![forbid(unsafe_code)]

mod ogg;
mod transcode;

pub use bl_opus::{Error, OpusEncoder, MAX_BITRATE, MIN_BITRATE};
pub use ogg::{OggOpusWriter, OpusHeader};
pub use transcode::{transcode, TranscodeOptions, Transcoder};
