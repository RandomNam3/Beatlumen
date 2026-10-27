//! MPEG-1/2/2.5 Layer I/II/III decoder (std only).
//!
//! API contract (implemented by this crate, consumed by `oc-media`):
//! * [`Mp3Decoder::new`] skips leading ID3v2 tags / junk, locates the first
//!   valid frame and parses Xing/Info/LAME/VBRI headers.
//! * Output is interleaved `f32` in [-1, 1], channel order L, R.

use std::io::{Read, Seek};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The stream is not (or no longer) a decodable MPEG audio stream.
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::InvalidData(s) => write!(f, "invalid mpeg audio data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported mpeg audio feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Mp3Decoder<R: Read + Seek> {
    _reader: R,
}

impl<R: Read + Seek> Mp3Decoder<R> {
    /// Opens a stream positioned anywhere before the first frame.
    pub fn new(reader: R) -> Result<Self, Error> {
        let _ = reader;
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn sample_rate(&self) -> u32 {
        0
    }
    pub fn channels(&self) -> u16 {
        0
    }
    /// Total number of PCM frames (samples per channel) after gapless
    /// trimming, if known from a Xing/Info/VBRI header (or estimated for CBR).
    pub fn total_frames(&self) -> Option<u64> {
        None
    }
    /// Decodes the next MPEG frame(s), appending interleaved samples to
    /// `out`. Returns `Ok(false)` at end of stream.
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let _ = out;
        Ok(false)
    }
    /// Seeks to the given PCM frame (best effort accuracy is acceptable for
    /// VBR without TOC, exact for CBR / with TOC + decoding forward).
    /// Returns the PCM frame actually positioned at.
    pub fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let _ = frame;
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn into_inner(self) -> R {
        self._reader
    }
}

/// Returns true if `data` (the first bytes of a file) looks like MPEG audio
/// (an ID3v2 tag or a valid frame header followed by another frame header).
pub fn probe(data: &[u8]) -> bool {
    let _ = data;
    false
}
