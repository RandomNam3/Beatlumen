//! Opus encoder (RFC 6716), CELT-only: fullband, 48 kHz, 20 ms frames, one
//! frame per packet (TOC configuration 31, code 0), constant bitrate.
//!
//! Every packet has exactly `bitrate / 400` bytes, except frames of digital
//! silence, which are sent as 3-byte packets. The CELT layer is in
//! `celt::encoder`; the DC rejection follows libopus `src/opus_encoder.c`
//! (BSD-3-Clause, see NOTICE).

use crate::celt::encoder::{CeltEncoder, FRAME_SIZE};
use crate::Error;

/// Lowest accepted bitrate (bit/s).
pub const MIN_BITRATE: u32 = 6_000;
/// Highest accepted bitrate (bit/s): 1275-byte CELT frames.
pub const MAX_BITRATE: u32 = 510_000;

/// CELT-only Opus encoder.
pub struct OpusEncoder {
    channels: u8,
    bitrate: u32,
    packet_bytes: usize,
    celt: CeltEncoder,
    hp_mem: [f32; 2],
    pcm: Vec<f32>,
}

impl OpusEncoder {
    /// Samples per channel in each packet (20 ms at 48 kHz).
    pub const FRAME_SIZE: usize = FRAME_SIZE;
    /// Encoder delay in samples at 48 kHz: the `pre_skip` to put in the
    /// OpusHead.
    pub const LOOKAHEAD: usize = 120;

    /// Creates an encoder for 48 kHz input with 1 or 2 channels and a
    /// constant `bitrate` in bit/s (see [`MIN_BITRATE`], [`MAX_BITRATE`]).
    pub fn new(channels: u8, bitrate: u32) -> Result<OpusEncoder, Error> {
        if !(1..=2).contains(&channels) {
            return Err(Error::Unsupported("encoder channel count (1 or 2)"));
        }
        if !(MIN_BITRATE..=MAX_BITRATE).contains(&bitrate) {
            return Err(Error::Unsupported("encoder bitrate (6000..=510000)"));
        }
        let celt = CeltEncoder::new(channels as usize)
            .ok_or(Error::Unsupported("encoder channel count (1 or 2)"))?;
        // Packet bytes for 20 ms, including the TOC byte.
        let packet_bytes = ((bitrate as usize + 200) / 400).clamp(3, 1276);
        Ok(OpusEncoder {
            channels,
            bitrate,
            packet_bytes,
            celt,
            hp_mem: [0.0; 2],
            pcm: vec![0.0; FRAME_SIZE * channels as usize],
        })
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Size of a (non-silent) packet in bytes.
    pub fn packet_bytes(&self) -> usize {
        self.packet_bytes
    }

    /// Enables or disables transient detection (short blocks), on by
    /// default.
    pub fn set_transients(&mut self, on: bool) {
        self.celt.transients = on;
    }

    /// Encodes one frame of [`OpusEncoder::FRAME_SIZE`] interleaved samples
    /// (nominally in [-1, 1]) and appends the packet to `out`. Returns the
    /// packet length.
    pub fn encode(&mut self, pcm: &[f32], out: &mut Vec<u8>) -> Result<usize, Error> {
        let ch = self.channels as usize;
        if pcm.len() != FRAME_SIZE * ch {
            return Err(Error::InvalidData(
                "encoder input must be 960 samples per channel",
            ));
        }
        // Sanitise (NaN/inf, extreme values) and remove DC with a 3 Hz
        // high-pass, as libopus does for CELT.
        let coef = 6.3f32 * 3.0 / 48000.0;
        let coef2 = 1.0 - coef;
        for c in 0..ch {
            let mut m = self.hp_mem[c];
            for i in 0..FRAME_SIZE {
                let v = pcm[i * ch + c];
                let x = if v.is_finite() {
                    v.clamp(-2.0, 2.0)
                } else {
                    0.0
                };
                self.pcm[i * ch + c] = x - m;
                m = coef * x + 1e-30 + coef2 * m;
            }
            self.hp_mem[c] = m;
        }
        let frame = self.celt.encode(&self.pcm, self.packet_bytes - 1);
        // TOC: config 31 (CELT-only, fullband, 20 ms), stereo flag, code 0.
        out.push((31 << 3) | (((ch == 2) as u8) << 2));
        out.extend_from_slice(&frame);
        Ok(frame.len() + 1)
    }

    /// Final range coder state of the last packet (`OPUS_GET_FINAL_RANGE`),
    /// for checking against [`crate::OpusDecoder::final_range`].
    pub fn final_range(&self) -> u32 {
        self.celt.rng
    }

    /// Resets the encoder state (start of a new, independent stream).
    pub fn reset(&mut self) {
        self.celt.reset();
        self.hp_mem = [0.0; 2];
    }
}
