//! Vorbis I decoder (packet level, std only). Container handling (Ogg,
//! Matroska) is done by the caller.
//!
//! Output is interleaved `f32` in [-1, 1], remapped from Vorbis channel order
//! to WAVE/SMPTE order (L R C LFE BL BR SL SR) for 3..=8 channels.

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid vorbis data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported vorbis feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct VorbisDecoder {
    _priv: (),
}

impl VorbisDecoder {
    /// Creates a decoder from the three header packets.
    pub fn new(ident: &[u8], comment: &[u8], setup: &[u8]) -> Result<Self, Error> {
        let _ = (ident, comment, setup);
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn sample_rate(&self) -> u32 {
        0
    }
    pub fn channels(&self) -> u16 {
        0
    }
    /// User comments from the comment header as (key, value) pairs; keys are
    /// returned as stored (callers compare case-insensitively).
    pub fn comments(&self) -> &[(String, String)] {
        &[]
    }
    /// Decodes one audio packet, appending interleaved samples to `out`.
    /// Returns the number of PCM frames appended (0 for the first packet
    /// after creation/reset, as per the Vorbis overlap-add scheme).
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let _ = (packet, out);
        Err(Error::Unsupported("not implemented yet"))
    }
    /// Forgets overlap state (call after seeking).
    pub fn reset(&mut self) {}
}

/// Returns the number of PCM frames packet `packet` would produce given the
/// previous packet's blocksize flag (helper for granule bookkeeping); `None`
/// if it is not an audio packet.
pub fn packet_block_size(dec: &VorbisDecoder, packet: &[u8]) -> Option<usize> {
    let _ = (dec, packet);
    None
}
