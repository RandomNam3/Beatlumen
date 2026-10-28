//! FLAC decoder (native FLAC streams, std only), following RFC 9639.
//!
//! Output is interleaved `f32` in [-1, 1] in FLAC channel order (which equals
//! the WAVE/SMPTE order: L R C LFE BL BR SL SR).
//!
//! * [`FlacDecoder`] decodes native FLAC files/streams (`fLaC` signature,
//!   optionally preceded by ID3v2 tags): all subframe types, 4..=32 bits per
//!   sample, 1..=8 channels, fixed and variable block sizes, CRC checks with
//!   resynchronisation, and sample accurate seeking (SEEKTABLE and/or
//!   bisection over frame headers).
//! * [`FlacFrameDecoder`] decodes single frames delivered by a container
//!   demuxer (FLAC in Ogg / Matroska / MP4).
//! * [`probe`] recognises FLAC data from its first bytes.
//!
//! Internally samples are decoded with 32-bit integer arithmetic; 64-bit
//! accumulators are only used where a prediction could overflow 32 bits
//! (high-precision LPC, 24/32-bit audio) and for the 33-bit side channel of
//! 32-bit stereo.

#![forbid(unsafe_code)]

mod bitreader;
mod crc;
mod decoder;
mod frame;
mod metadata;
mod packet;

pub use decoder::FlacDecoder;
pub use metadata::{MetadataBlock, SeekPoint, StreamInfo};
pub use packet::FlacFrameDecoder;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
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
            Error::InvalidData(s) => write!(f, "invalid flac data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported flac feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Returns true if `data` (the first bytes of a file) looks like a native
/// FLAC stream: it starts with `fLaC`, or with an ID3v2 tag that is followed
/// by `fLaC` within `data`.
pub fn probe(data: &[u8]) -> bool {
    let mut d = data;
    while d.len() >= 10 && &d[..3] == b"ID3" {
        let size = d[6..10]
            .iter()
            .fold(0usize, |a, &b| (a << 7) | (b & 0x7f) as usize);
        let footer = if d[5] & 0x10 != 0 { 10 } else { 0 };
        match d.get(10 + size + footer..) {
            Some(rest) => d = rest,
            None => return false,
        }
    }
    d.starts_with(b"fLaC")
}

#[cfg(test)]
mod tests;
