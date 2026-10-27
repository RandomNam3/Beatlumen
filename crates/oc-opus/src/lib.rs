//! Opus decoder (RFC 6716 / RFC 7845, std only). Packet level; container
//! handling is done by the caller.
//!
//! Output is interleaved `f32` (nominally in [-1, 1]) at the configured
//! output rate (48 kHz recommended). Multichannel streams (mapping family 1)
//! are output in WAVE/SMPTE order (L R C LFE BL BR SL SR).
//!
//! The decoder is a port of the libopus 1.5.2 reference implementation
//! (float build; BSD-3-Clause, see `NOTICE`): SILK (NB/MB/WB, stereo, LBRR
//! skipping, resampling to the output rate), CELT (all frame sizes,
//! transients, post-filter, intensity/dual stereo, anti-collapse), hybrid
//! mode, mode transitions with redundancy frames and cross-fades, packet
//! loss concealment, multi-frame packets (codes 0-3, padding) and
//! multistream packets (channel mapping families 0, 1, 2 and 255).
//! Decoded output is bit-exact with libopus' float decoder on x86-64 (the
//! SSE accumulation order of its float kernels is reproduced in portable
//! scalar code); SILK is fixed point and bit-exact everywhere.
//!
//! Module layout: `range_dec` (range decoder), `silk` (SILK decoder),
//! `celt` (CELT decoder), `opus` (single-stream decoder with mode
//! switching), `packet` (TOC / framing), `multistream` (stream packing and
//! channel mapping).

#![forbid(unsafe_code)]
// The DSP code is a faithful port of fixed/float C code: keep the original
// literal constants, index-based loops and argument lists.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::precedence,
    clippy::explicit_counter_loop,
    clippy::assign_op_pattern
)]

mod celt;
mod multistream;
mod opus;
mod packet;
mod range_dec;
mod silk;

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
            (
                1,
                if channels == 2 { 1 } else { 0 },
                if channels == 2 { vec![0, 1] } else { vec![0] },
            )
        } else {
            if data.len() < 21 + channels as usize {
                return Err(Error::InvalidData("truncated channel mapping table"));
            }
            (
                data[19],
                data[20],
                data[21..21 + channels as usize].to_vec(),
            )
        };
        Ok(OpusHead {
            channels,
            pre_skip,
            input_sample_rate,
            output_gain,
            mapping_family,
            stream_count,
            coupled_count,
            mapping,
        })
    }
}

/// Vorbis channel order -> WAVE/SMPTE order (output channel i takes Vorbis
/// channel `VORBIS_TO_WAVE[n-1][i]`), as used for mapping family 1.
const VORBIS_TO_WAVE: [[usize; 8]; 8] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 2, 1, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 0, 0, 0, 0],
    [0, 2, 1, 3, 4, 0, 0, 0],
    [0, 2, 1, 5, 3, 4, 0, 0],
    [0, 2, 1, 6, 5, 3, 4, 0],
    [0, 2, 1, 7, 5, 6, 3, 4],
];

pub struct OpusDecoder {
    sample_rate: u32,
    channels: u16,
    gain: f32,
    ms: multistream::MultistreamDecoder,
}

impl OpusDecoder {
    /// Creates a decoder for a stream described by `head`; `sample_rate` is
    /// the output rate (one of 8000, 12000, 16000, 24000, 48000).
    pub fn new(head: &OpusHead, sample_rate: u32) -> Result<Self, Error> {
        if !matches!(sample_rate, 8000 | 12000 | 16000 | 24000 | 48000) {
            return Err(Error::Unsupported("output sample rate"));
        }
        let channels = head.channels as usize;
        if channels == 0 {
            return Err(Error::InvalidData("zero channels"));
        }
        let (streams, coupled, mut mapping) = match head.mapping_family {
            0 => {
                if channels > 2 {
                    return Err(Error::InvalidData("bad channel count for mapping family 0"));
                }
                (
                    1usize,
                    channels - 1,
                    (0..channels as u8).collect::<Vec<u8>>(),
                )
            }
            1 | 2 | 255 => {
                if head.mapping_family == 1 && channels > 8 {
                    return Err(Error::InvalidData("too many channels for mapping family 1"));
                }
                if head.mapping.len() < channels {
                    return Err(Error::InvalidData("truncated channel mapping table"));
                }
                (
                    head.stream_count as usize,
                    head.coupled_count as usize,
                    head.mapping[..channels].to_vec(),
                )
            }
            _ => return Err(Error::Unsupported("channel mapping family")),
        };
        if head.mapping_family == 1 && (3..=8).contains(&channels) {
            let order = &VORBIS_TO_WAVE[channels - 1];
            mapping = (0..channels).map(|i| head.mapping[order[i]]).collect();
        }
        let ms = multistream::MultistreamDecoder::new(
            sample_rate,
            channels,
            streams,
            coupled,
            &mapping,
        )?;
        let gain = if head.output_gain != 0 {
            celt::math::celt_exp2(6.488_140_81e-4_f32 * head.output_gain as f32)
        } else {
            1.0
        };
        Ok(OpusDecoder {
            sample_rate,
            channels: channels as u16,
            gain,
            ms,
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    /// Decodes one Opus packet (possibly a multistream packet), appending
    /// interleaved samples to `out` (output gain from the head already
    /// applied). Returns the number of PCM frames appended.
    ///
    /// An empty packet signals a lost packet: the decoder conceals it
    /// (outputting audio of the previous packet's duration).
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let start = out.len();
        let n = self.ms.decode(packet, out)?;
        if self.gain != 1.0 {
            for v in out[start..].iter_mut() {
                *v *= self.gain;
            }
        }
        Ok(n)
    }
    /// Resets decoder state (after seeking).
    pub fn reset(&mut self) {
        self.ms.reset();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn head(ch: u8, family: u8, rest: &[u8]) -> Vec<u8> {
        let mut h = b"OpusHead".to_vec();
        h.extend_from_slice(&[1, ch, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, family]);
        h.extend_from_slice(rest);
        h
    }

    #[test]
    fn decoder_creation() {
        let h = OpusHead::parse(&head(2, 0, &[])).unwrap();
        let d = OpusDecoder::new(&h, 48000).unwrap();
        assert_eq!((d.sample_rate(), d.channels()), (48000, 2));
        assert!(OpusDecoder::new(&h, 44100).is_err());
        let h = OpusHead::parse(&head(6, 1, &[4, 2, 0, 4, 1, 2, 3, 5])).unwrap();
        assert_eq!(OpusDecoder::new(&h, 24000).unwrap().channels(), 6);
        // mapping index out of range
        let h = OpusHead::parse(&head(2, 1, &[1, 0, 0, 3])).unwrap();
        assert!(OpusDecoder::new(&h, 48000).is_err());
        // 9 channels in family 1
        let h = OpusHead::parse(&head(9, 1, &[9, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8])).unwrap();
        assert!(OpusDecoder::new(&h, 48000).is_err());
        assert!(OpusHead::parse(&head(3, 1, &[3, 0, 0])).is_err());
    }

    #[test]
    fn silence_and_concealment_without_history() {
        let h = OpusHead::parse(&head(2, 0, &[])).unwrap();
        let mut d = OpusDecoder::new(&h, 48000).unwrap();
        let mut out = Vec::new();
        // Concealment before any packet: silence.
        let n = d.decode_packet(&[], &mut out).unwrap();
        assert!(n > 0 && out.iter().all(|&v| v == 0.0));
        // A CELT 20 ms frame with silence flag (0xFF 0xFF is the "silence" pattern).
        out.clear();
        let n = d.decode_packet(&[0xFC, 0xFF, 0xFF], &mut out).unwrap();
        assert_eq!(n, 960);
        assert_eq!(out.len(), 1920);
        assert_eq!(packet_samples_48k(&[0xFC]), Some(960));
    }
}
