//! Uncompressed PCM sample conversion and the PCM decoder shared by the WAV
//! and AIFF demuxers.

use std::io::{Read, Seek, SeekFrom};

use crate::util::read_up_to;
use crate::{AudioSpec, Decoder, Error};

/// Storage format of one PCM sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleFormat {
    U8,
    S8,
    S16Le,
    S16Be,
    S24Le,
    S24Be,
    S32Le,
    S32Be,
    F32Le,
    F32Be,
    F64Le,
    F64Be,
    ALaw,
    MuLaw,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        use SampleFormat::*;
        match self {
            U8 | S8 | ALaw | MuLaw => 1,
            S16Le | S16Be => 2,
            S24Le | S24Be => 3,
            S32Le | S32Be | F32Le | F32Be => 4,
            F64Le | F64Be => 8,
        }
    }

    /// Integer PCM format for a container size in bytes.
    pub fn int(bytes: usize, little_endian: bool, unsigned8: bool) -> Option<SampleFormat> {
        use SampleFormat::*;
        Some(match (bytes, little_endian) {
            (1, _) => {
                if unsigned8 {
                    U8
                } else {
                    S8
                }
            }
            (2, true) => S16Le,
            (2, false) => S16Be,
            (3, true) => S24Le,
            (3, false) => S24Be,
            (4, true) => S32Le,
            (4, false) => S32Be,
            _ => return None,
        })
    }
}

fn mulaw(b: u8) -> f32 {
    let u = !b;
    let mut t = (((u & 0x0f) as i32) << 3) + 0x84;
    t <<= (u & 0x70) >> 4;
    let v = if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 };
    v as f32 / 32768.0
}

fn alaw(b: u8) -> f32 {
    let a = b ^ 0x55;
    let mut t = ((a & 0x0f) as i32) << 4;
    let seg = (a & 0x70) >> 4;
    if seg == 0 {
        t += 8;
    } else {
        t = (t + 0x108) << (seg - 1);
    }
    let v = if a & 0x80 != 0 { t } else { -t };
    v as f32 / 32768.0
}

/// Converts tightly packed samples to `f32`.
fn convert_packed(fmt: SampleFormat, d: &[u8], out: &mut Vec<f32>) {
    use SampleFormat::*;
    const S16: f32 = 1.0 / 32768.0;
    const S24: f32 = 1.0 / 8388608.0;
    const S32: f32 = 1.0 / 2147483648.0;
    match fmt {
        U8 => out.extend(d.iter().map(|&b| (b as f32 - 128.0) * (1.0 / 128.0))),
        S8 => out.extend(d.iter().map(|&b| b as i8 as f32 * (1.0 / 128.0))),
        S16Le => out.extend(d.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 * S16)),
        S16Be => out.extend(d.chunks_exact(2).map(|c| i16::from_be_bytes([c[0], c[1]]) as f32 * S16)),
        S24Le => out.extend(d.chunks_exact(3).map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 * S24)),
        S24Be => out.extend(d.chunks_exact(3).map(|c| (i32::from_be_bytes([c[0], c[1], c[2], 0]) >> 8) as f32 * S24)),
        S32Le => out.extend(d.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 * S32)),
        S32Be => out.extend(d.chunks_exact(4).map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f32 * S32)),
        F32Le => out.extend(d.chunks_exact(4).map(|c| sanitize(f32::from_le_bytes([c[0], c[1], c[2], c[3]])))),
        F32Be => out.extend(d.chunks_exact(4).map(|c| sanitize(f32::from_be_bytes([c[0], c[1], c[2], c[3]])))),
        F64Le => out.extend(d.chunks_exact(8).map(|c| sanitize(f64::from_le_bytes(c.try_into().unwrap()) as f32))),
        F64Be => out.extend(d.chunks_exact(8).map(|c| sanitize(f64::from_be_bytes(c.try_into().unwrap()) as f32))),
        ALaw => out.extend(d.iter().map(|&b| alaw(b))),
        MuLaw => out.extend(d.iter().map(|&b| mulaw(b))),
    }
}

/// NaN/infinite floats in a corrupted file are replaced by silence.
#[inline]
fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Converts whole frames of `frame_bytes` bytes (which may contain padding
/// after the `channels` samples) to interleaved `f32`.
pub(crate) fn convert(fmt: SampleFormat, channels: usize, frame_bytes: usize, d: &[u8], out: &mut Vec<f32>) {
    let used = channels * fmt.bytes();
    if frame_bytes == used {
        let n = d.len() / frame_bytes * frame_bytes;
        convert_packed(fmt, &d[..n], out);
    } else if frame_bytes > used {
        for f in d.chunks_exact(frame_bytes) {
            convert_packed(fmt, &f[..used], out);
        }
    }
}

/// Description of a PCM data region.
#[derive(Clone, Debug)]
pub(crate) struct PcmStream {
    pub fmt: SampleFormat,
    pub channels: u16,
    pub sample_rate: u32,
    /// Bytes per frame (block align), at least `channels * fmt.bytes()`.
    pub frame_bytes: usize,
    pub data_start: u64,
    pub total_frames: u64,
}

impl PcmStream {
    pub fn validate(&self) -> Result<(), Error> {
        if self.channels == 0 || self.sample_rate == 0 {
            return crate::util::invalid("zero channels or sample rate");
        }
        if self.frame_bytes < self.channels as usize * self.fmt.bytes() || self.frame_bytes > 1 << 20 {
            return crate::util::invalid("bad block alignment");
        }
        Ok(())
    }
}

const CHUNK_FRAMES: usize = 4096;

/// Streaming decoder for raw PCM data.
pub(crate) struct PcmDecoder<R: Read + Seek + Send> {
    r: R,
    s: PcmStream,
    pos: u64,
    buf: Vec<u8>,
}

impl<R: Read + Seek + Send> PcmDecoder<R> {
    pub fn new(mut r: R, s: PcmStream) -> Result<Self, Error> {
        s.validate()?;
        r.seek(SeekFrom::Start(s.data_start))?;
        Ok(PcmDecoder { r, s, pos: 0, buf: Vec::new() })
    }
}

impl<R: Read + Seek + Send> Decoder for PcmDecoder<R> {
    fn spec(&self) -> AudioSpec {
        AudioSpec { sample_rate: self.s.sample_rate, channels: self.s.channels }
    }
    fn total_frames(&self) -> Option<u64> {
        Some(self.s.total_frames)
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let left = self.s.total_frames.saturating_sub(self.pos);
        if left == 0 {
            return Ok(false);
        }
        let frames = (CHUNK_FRAMES as u64).min(left) as usize;
        let fb = self.s.frame_bytes;
        self.buf.resize(frames * fb, 0);
        let got = read_up_to(&mut self.r, &mut self.buf)?;
        let whole = got / fb;
        if whole == 0 {
            // Truncated file: the header promised more data.
            self.s.total_frames = self.pos;
            return Ok(false);
        }
        if whole < frames {
            self.s.total_frames = self.pos + whole as u64;
        }
        convert(self.s.fmt, self.s.channels as usize, fb, &self.buf[..whole * fb], out);
        self.pos += whole as u64;
        Ok(true)
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let f = frame.min(self.s.total_frames);
        self.r.seek(SeekFrom::Start(self.s.data_start + f * self.s.frame_bytes as u64))?;
        self.pos = f;
        Ok(f)
    }
    fn codec_name(&self) -> &'static str {
        "pcm"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        let mut out = Vec::new();
        convert(SampleFormat::S16Le, 1, 2, &[0x00, 0x80, 0xff, 0x7f], &mut out);
        assert_eq!(out[0], -1.0);
        assert!((out[1] - 32767.0 / 32768.0).abs() < 1e-7);
        out.clear();
        convert(SampleFormat::S24Be, 1, 3, &[0x80, 0, 0, 0x40, 0, 0], &mut out);
        assert_eq!(out, vec![-1.0, 0.5]);
        out.clear();
        convert(SampleFormat::U8, 2, 3, &[0, 255, 9, 128, 128, 9], &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[2], 0.0);
        assert!((mulaw(0x00) + 32124.0 / 32768.0).abs() < 1e-6);
        assert!((alaw(0xd5) - 8.0 / 32768.0).abs() < 1e-6);
    }
}
