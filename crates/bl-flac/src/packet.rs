//! Decoding of individual FLAC frames delivered by a container demuxer
//! (FLAC-in-Ogg, FLAC-in-Matroska, FLAC-in-MP4).

use crate::frame::{parse_header, FrameDecoder, FrameError};
use crate::metadata::StreamInfo;
use crate::Error;

/// Decoder for complete FLAC frames handed over one at a time, e.g. by an
/// Ogg or Matroska demuxer (each packet/block holds exactly one frame).
///
/// ```no_run
/// # fn demo(codec_private: &[u8], packets: &[Vec<u8>]) -> Result<(), bl_flac::Error> {
/// let mut dec = bl_flac::FlacFrameDecoder::new(codec_private)?;
/// let mut pcm = Vec::new();
/// for p in packets {
///     let frames = dec.decode(p, &mut pcm)?; // appends frames * channels samples
///     # let _ = frames;
/// }
/// # Ok(()) }
/// ```
pub struct FlacFrameDecoder {
    info: StreamInfo,
    core: FrameDecoder,
    corrupt: u64,
}

impl FlacFrameDecoder {
    /// Creates a decoder from the stream's STREAMINFO. Accepts the bare
    /// 34-byte payload, a metadata block (header + payload), `fLaC` followed
    /// by metadata blocks (Matroska `CodecPrivate`) or the first Ogg FLAC
    /// header packet (`0x7F "FLAC" ...`).
    pub fn new(streaminfo: &[u8]) -> Result<Self, Error> {
        let payload = StreamInfo::find_in_codec_private(streaminfo)
            .ok_or(Error::InvalidData("STREAMINFO not found in codec setup data"))?;
        let info = StreamInfo::parse(payload)?;
        Ok(FlacFrameDecoder { info, core: FrameDecoder::new(), corrupt: 0 })
    }

    pub fn stream_info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn sample_rate(&self) -> u32 {
        self.info.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.info.channels as u16
    }

    pub fn bits_per_sample(&self) -> u32 {
        self.info.bits_per_sample as u32
    }

    /// Number of frames whose content failed to decode or failed the CRC
    /// check (and were replaced by silence).
    pub fn corrupt_frames(&self) -> u64 {
        self.corrupt
    }

    /// Decodes one complete FLAC frame (`frame` starts with the frame sync
    /// code; trailing bytes after the frame are ignored) and appends its
    /// interleaved `f32` samples (normalised by 2^(bps-1)) to `out`.
    /// Returns the number of PCM frames (samples per channel) appended.
    ///
    /// If the frame header is valid but the frame content is corrupt
    /// (CRC-16 mismatch, truncated or undecodable), silence of the frame's
    /// block size is appended instead so the timeline is preserved. An
    /// unparseable header or a channel count different from STREAMINFO is an
    /// error.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        match self.decode_inner(frame)? {
            Ok(()) => {
                self.core.write_f32(0, out);
                Ok(self.core.block_size)
            }
            Err(n) => {
                out.resize(out.len() + n * self.info.channels as usize, 0.0);
                Ok(n)
            }
        }
    }

    /// Like [`decode`](Self::decode) but appends raw integer samples
    /// (right-aligned, at the frame's bit depth).
    pub fn decode_i32(&mut self, frame: &[u8], out: &mut Vec<i32>) -> Result<usize, Error> {
        match self.decode_inner(frame)? {
            Ok(()) => {
                self.core.write_i32(0, out);
                Ok(self.core.block_size)
            }
            Err(n) => {
                out.resize(out.len() + n * self.info.channels as usize, 0);
                Ok(n)
            }
        }
    }

    /// Inner result: `Ok(())` = decoded into `core`, `Err(n)` = corrupt frame
    /// of `n` samples.
    fn decode_inner(&mut self, frame: &[u8]) -> Result<Result<(), usize>, Error> {
        let h = match parse_header(frame) {
            Ok(h) => h,
            Err(FrameError::Eof) => return Err(Error::InvalidData("truncated frame header")),
            Err(FrameError::Invalid(why)) => return Err(Error::InvalidData(why)),
            Err(FrameError::Crc) => return Err(Error::InvalidData("frame CRC mismatch")),
        };
        if h.channels != self.info.channels as u32 {
            return Err(Error::Unsupported("channel count differs from STREAMINFO"));
        }
        let bps = if h.bps != 0 { h.bps } else { self.info.bits_per_sample as u32 };
        match self.core.decode(frame, &h, bps) {
            Ok(_) => Ok(Ok(())),
            Err(_) => {
                self.corrupt += 1;
                Ok(Err(h.block_size as usize))
            }
        }
    }
}
