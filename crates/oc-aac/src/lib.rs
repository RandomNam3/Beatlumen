//! AAC decoder (MPEG-4 AAC-LC; HE-AAC SBR/PS optional), std only. Packet
//! level: callers provide raw_data_block payloads (from MP4 samples or ADTS
//! frames with the header stripped).
//!
//! Output is interleaved `f32` in [-1, 1], WAVE/SMPTE channel order
//! (L R C LFE BL BR SL SR) — AAC channel configurations are remapped.

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid aac data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported aac feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parsed ADTS header.
#[derive(Debug, Clone, Copy)]
pub struct AdtsHeader {
    /// Audio object type (1 = Main, 2 = LC, ...), i.e. profile + 1.
    pub object_type: u8,
    pub sample_rate_index: u8,
    pub channel_config: u8,
    /// Total frame length including the header.
    pub frame_length: usize,
    pub header_length: usize,
    pub raw_blocks: u8,
}

impl AdtsHeader {
    pub fn parse(data: &[u8]) -> Option<AdtsHeader> {
        if data.len() < 7 || data[0] != 0xff || data[1] & 0xf6 != 0xf0 {
            return None;
        }
        let protection_absent = data[1] & 1 != 0;
        let object_type = (data[2] >> 6) + 1;
        let sample_rate_index = (data[2] >> 2) & 0xf;
        if sample_rate_index > 12 {
            return None;
        }
        let channel_config = ((data[2] & 1) << 2) | (data[3] >> 6);
        let frame_length = (((data[3] & 3) as usize) << 11) | ((data[4] as usize) << 3) | ((data[5] >> 5) as usize);
        let header_length = if protection_absent { 7 } else { 9 };
        if frame_length < header_length {
            return None;
        }
        Some(AdtsHeader { object_type, sample_rate_index, channel_config, frame_length, header_length, raw_blocks: data[6] & 3 })
    }

    /// Builds the equivalent 2-byte AudioSpecificConfig.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        [(self.object_type << 3) | (self.sample_rate_index >> 1), ((self.sample_rate_index & 1) << 7) | (self.channel_config << 3)]
    }
}

pub const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

pub struct AacDecoder {
    _priv: (),
}

impl AacDecoder {
    /// Creates a decoder from an MPEG-4 AudioSpecificConfig (e.g. from the
    /// MP4 `esds` box, or [`AdtsHeader::audio_specific_config`]).
    pub fn new(audio_specific_config: &[u8]) -> Result<Self, Error> {
        let _ = audio_specific_config;
        Err(Error::Unsupported("not implemented yet"))
    }
    /// Output sample rate (doubled when SBR is decoded).
    pub fn sample_rate(&self) -> u32 {
        0
    }
    pub fn channels(&self) -> u16 {
        0
    }
    /// Decodes one raw_data_block, appending interleaved samples to `out`.
    /// Returns the number of PCM frames appended.
    pub fn decode_frame(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let _ = (data, out);
        Err(Error::Unsupported("not implemented yet"))
    }
    /// Resets overlap state (after seeking).
    pub fn reset(&mut self) {}
}
