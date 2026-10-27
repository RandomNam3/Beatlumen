//! Opus decoder (RFC 6716 / RFC 7845, std only). Packet level; container
//! handling is done by the caller.
//!
//! Output is interleaved `f32` (nominally in [-1, 1]) at the configured
//! output rate (48 kHz recommended). Multichannel streams (mapping family 1)
//! are output in WAVE/SMPTE order (L R C LFE BL BR SL SR).

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid opus data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported opus feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parsed "OpusHead" identification header (RFC 7845 section 5.1).
#[derive(Debug, Clone)]
pub struct OpusHead {
    pub channels: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    /// Output gain in Q7.8 dB.
    pub output_gain: i16,
    pub mapping_family: u8,
    pub stream_count: u8,
    pub coupled_count: u8,
    pub mapping: Vec<u8>,
}

impl OpusHead {
    pub fn parse(data: &[u8]) -> Result<OpusHead, Error> {
        if data.len() < 19 || &data[..8] != b"OpusHead" {
            return Err(Error::InvalidData("not an OpusHead packet"));
        }
        if data[8] >> 4 != 0 {
            return Err(Error::Unsupported("OpusHead major version"));
        }
        let channels = data[9];
        let pre_skip = u16::from_le_bytes([data[10], data[11]]);
        let input_sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let output_gain = i16::from_le_bytes([data[16], data[17]]);
        let mapping_family = data[18];
        let (stream_count, coupled_count, mapping) = if mapping_family == 0 {
            if channels == 0 || channels > 2 {
                return Err(Error::InvalidData("bad channel count for mapping family 0"));
            }
            (1, if channels == 2 { 1 } else { 0 }, if channels == 2 { vec![0, 1] } else { vec![0] })
        } else {
            if data.len() < 21 + channels as usize {
                return Err(Error::InvalidData("truncated channel mapping table"));
            }
            (data[19], data[20], data[21..21 + channels as usize].to_vec())
        };
        Ok(OpusHead { channels, pre_skip, input_sample_rate, output_gain, mapping_family, stream_count, coupled_count, mapping })
    }
}

pub struct OpusDecoder {
    _priv: (),
}

impl OpusDecoder {
    /// Creates a decoder for a stream described by `head`; `sample_rate` is
    /// the output rate (one of 8000, 12000, 16000, 24000, 48000).
    pub fn new(head: &OpusHead, sample_rate: u32) -> Result<Self, Error> {
        let _ = (head, sample_rate);
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn sample_rate(&self) -> u32 {
        0
    }
    pub fn channels(&self) -> u16 {
        0
    }
    /// Decodes one Opus packet (possibly a multistream packet), appending
    /// interleaved samples to `out` (output gain from the head already
    /// applied). Returns the number of PCM frames appended.
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let _ = (packet, out);
        Err(Error::Unsupported("not implemented yet"))
    }
    /// Resets decoder state (after seeking).
    pub fn reset(&mut self) {}
}

/// Number of samples (at 48 kHz) in a single-stream Opus packet, from its
/// TOC byte and frame count code (RFC 6716 section 3.1).
pub fn packet_samples_48k(packet: &[u8]) -> Option<usize> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config & 3) as usize],
        12..=15 => [480, 960][(config & 1) as usize],
        _ => [120, 240, 480, 960][(config & 3) as usize],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => (*packet.get(1)? & 0x3f) as usize,
    };
    Some(frame * count)
}
