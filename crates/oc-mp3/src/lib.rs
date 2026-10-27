//! MPEG-1/2/2.5 Layer I/II/III decoder (std only).
//!
//! API contract (implemented by this crate, consumed by `oc-media`):
//! * [`Mp3Decoder::new`] skips leading ID3v2 tags / junk, locates the first
//!   valid frame and parses Xing/Info/LAME/VBRI headers.
//! * Output is interleaved `f32` in [-1, 1], channel order L, R.
//!
//! Features: all MPEG versions and layers, mono/stereo/dual/joint stereo
//! (M/S and intensity incl. MPEG-2 LSF intensity), mixed/short blocks,
//! free-format bitrates, the Layer III bit reservoir (frames whose
//! reservoir data is missing, e.g. right after a resync, decode the
//! available granules and output silence for the rest), CRC skipping,
//! robust resynchronization over junk, trailing ID3v1/APEv2/Lyrics3 tag
//! detection, LAME gapless trimming (encoder delay + 529 samples at the
//! start, padding - 529 at the end, like LAME/ffmpeg) and sample-accurate
//! seeking.
//!
//! The DSP core is ported from minimp3 (CC0 1.0, see `NOTICE`).

#![forbid(unsafe_code)]
// Numeric constants are kept exactly as published in the reference tables.
#![allow(clippy::excessive_precision)]

mod bits;
mod decoder;
mod header;
mod huffman;
mod layer12;
mod layer3;
mod source;
mod synth;
mod tables;
mod tags;

use decoder::FrameDecoder;
use header::FrameHeader;
use source::Source;
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use tags::{VbriInfo, XingInfo};

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

/// How [`Mp3Decoder::seek`] locates the target frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SeekMode {
    /// Sample-accurate: output after a seek is identical to decoding
    /// continuously from the start. Frame positions come from an index
    /// built by scanning frame headers incrementally (already decoded or
    /// scanned regions are not rescanned); streams declared CBR by an Info
    /// header are positioned arithmetically instead. A jump that would
    /// scan more than 64 MiB of a VBR stream with a TOC falls back to
    /// [`SeekMode::Fast`] to bound the I/O.
    #[default]
    Exact,
    /// Jump using the Xing TOC / VBRI table or the CBR byte rate without
    /// scanning; the reported position is an estimate for VBR streams.
    Fast,
}

/// Frame offsets are indexed every `INDEX_STRIDE` frames (a power of two).
const INDEX_STRIDE: u64 = 16;
/// Upper bound of frames needed to refill the bit reservoir before a seek
/// target (worst case: 255-byte reservoir of 8 kbit/s MPEG-2 frames).
const MAX_RESERVOIR_FRAMES: usize = 128;
/// Bytes searched for the first frame (after ID3v2 tags).
const MAX_INITIAL_JUNK: u64 = 4 << 20;
/// Largest unscanned span read to locate a seek target exactly.
const MAX_EXACT_SCAN: u64 = 64 << 20;
/// Largest free-format frame searched for.
const MAX_FREE_FORMAT: usize = 4096;

pub struct Mp3Decoder<R: Read + Seek> {
    src: Source<R>,
    dec: FrameDecoder,
    /// Header of the first frame; later frames must be compatible.
    lock: FrameHeader,
    free_format_bytes: usize,
    /// The next frame is expected exactly at the cursor (no verification).
    contiguous: bool,
    sample_rate: u32,
    channels: u16,
    spf: u64,
    /// Offset of the Xing/VBRI frame (TOC base) or of the first frame.
    info_offset: u64,
    /// Offset of the first audio frame.
    first_audio: u64,
    xing: Option<XingInfo>,
    vbri: Option<VbriInfo>,
    /// Decoded samples dropped at the start (encoder delay + 529).
    start_skip: u64,
    /// Decoded-sample range dropped as end padding.
    discard: Option<(u64, u64)>,
    /// Average frame length in bytes (for CBR estimates).
    avg_frame_len: f64,
    /// Index of the next frame to decode.
    frame_no: u64,
    /// `frame_no` is exact (false after a [`SeekMode::Fast`] seek).
    exact: bool,
    /// Decoded-sample index of the next decoded sample.
    sample_pos: u64,
    /// Decoded samples before this index are not output.
    skip_until: u64,
    /// Offsets of frames 0, 16, 32, ...
    index: Vec<u64>,
    /// Exact number of audio frames, once the end has been reached.
    eof_frames: Option<u64>,
    seek_mode: SeekMode,
    frame_buf: Vec<u8>,
    pcm: Vec<f32>,
}

impl<R: Read + Seek> Mp3Decoder<R> {
    /// Opens a stream positioned anywhere before the first frame.
    pub fn new(mut reader: R) -> Result<Self, Error> {
        let start = reader.stream_position()?;
        let end = match reader.seek(SeekFrom::End(0)) {
            Ok(len) => {
                let end = tags::find_audio_end(&mut reader, start, len)?;
                reader.seek(SeekFrom::Start(start))?;
                end
            }
            Err(_) => {
                reader.seek(SeekFrom::Start(start))?;
                u64::MAX
            }
        };
        let mut src = Source::new(reader, start, end);
        let (h, len, free_format_bytes) = find_first_frame(&mut src, start)?;
        let frame_off = src.offset();
        let frame = &src.peek()[..len];
        let xing = tags::parse_xing(&h, frame);
        let vbri = if xing.is_none() {
            tags::parse_vbri(&h, frame)
        } else {
            None
        };
        let first_audio = if xing.is_some() || vbri.is_some() {
            frame_off + len as u64
        } else {
            frame_off
        };
        src.seek_to(first_audio)?;

        let spf = h.samples_per_frame() as u64;
        let mut start_skip = 0;
        let mut discard = None;
        if let Some(x) = &xing {
            if let Some((delay, padding)) = x.lame {
                start_skip = u64::from(delay) + 529;
                if let Some(frames) = x.frames {
                    let total = u64::from(frames) * spf;
                    let first = (total + 529).saturating_sub(u64::from(padding));
                    if first < total {
                        discard = Some((first, total));
                    }
                }
            }
        }
        let avg_frame_len = if h.is_free_format() {
            free_format_bytes as f64 + 0.5 * h.padding_len().max(1) as f64
        } else {
            spf as f64 * f64::from(h.bitrate_kbps()) * 125.0 / f64::from(h.sample_rate)
        };
        Ok(Mp3Decoder {
            src,
            dec: FrameDecoder::new(),
            lock: h,
            free_format_bytes,
            contiguous: true,
            sample_rate: h.sample_rate,
            channels: h.channels() as u16,
            spf,
            info_offset: frame_off,
            first_audio,
            xing,
            vbri,
            start_skip,
            discard,
            avg_frame_len,
            frame_no: 0,
            exact: true,
            sample_pos: 0,
            skip_until: start_skip,
            index: vec![first_audio],
            eof_frames: None,
            seek_mode: SeekMode::Exact,
            frame_buf: Vec::with_capacity(MAX_FREE_FORMAT),
            pcm: vec![0.0; 1152 * 2],
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// MPEG layer (1, 2 or 3) of the stream.
    pub fn layer(&self) -> u8 {
        self.lock.layer
    }

    /// Nominal bitrate of the first frame in kbit/s (0 for free format).
    pub fn bitrate_kbps(&self) -> u32 {
        self.lock.bitrate_kbps()
    }

    /// Encoder delay and padding (in samples) from a LAME/Lavc header.
    pub fn encoder_delay_padding(&self) -> Option<(u32, u32)> {
        self.xing.as_ref().and_then(|x| x.lame)
    }

    pub fn set_seek_mode(&mut self, mode: SeekMode) {
        self.seek_mode = mode;
    }

    /// Total number of decoded samples per channel before trimming, from
    /// the VBR header, the end of stream if reached, or `None`.
    fn exact_decoded_total(&self) -> Option<u64> {
        let frames = self
            .xing
            .as_ref()
            .and_then(|x| x.frames)
            .or_else(|| self.vbri.as_ref().map(|v| v.frames))
            .map(u64::from)
            .or(self.eof_frames)?;
        Some(frames * self.spf)
    }

    /// Number of output frames for `decoded` decoded samples per channel.
    fn output_len(&self, decoded: u64) -> u64 {
        let end = match self.discard {
            Some((first, last)) if decoded <= last => decoded.min(first),
            Some((first, last)) => first + (decoded - last),
            None => decoded,
        };
        end.saturating_sub(self.start_skip)
    }

    /// Total number of PCM frames (samples per channel) after gapless
    /// trimming, if known from a Xing/Info/VBRI header (or estimated for CBR).
    pub fn total_frames(&self) -> Option<u64> {
        if let Some(d) = self.exact_decoded_total() {
            return Some(self.output_len(d));
        }
        let end = self.src.end();
        if end == u64::MAX || self.avg_frame_len <= 0.0 {
            return None;
        }
        let frames =
            (end.saturating_sub(self.first_audio) as f64 / self.avg_frame_len).round() as u64;
        Some(self.output_len(frames * self.spf))
    }

    /// Finds the next frame (resynchronizing over junk and skipping ID3v2
    /// tags); on success the frame starts at the cursor.
    fn find_frame(&mut self) -> Result<Option<(FrameHeader, usize)>, Error> {
        loop {
            if self.src.fill(4)? < 4 {
                return Ok(None);
            }
            let b = self.src.peek();
            if &b[..3] == b"ID3" && self.src.fill(10)? >= 10 {
                if let Some(n) = tags::id3v2_len(self.src.peek()) {
                    self.src.consume(usize::try_from(n).unwrap_or(usize::MAX))?;
                    self.contiguous = false;
                    continue;
                }
            }
            if let Some(h) = FrameHeader::parse(self.src.peek()) {
                if h.compatible(&self.lock) {
                    if let Some(len) = self.check_frame(&h)? {
                        return Ok(Some((h, len)));
                    }
                }
            }
            self.src.consume(1)?;
            self.contiguous = false;
        }
    }

    /// Validates a candidate frame at the cursor and returns its length.
    fn check_frame(&mut self, h: &FrameHeader) -> Result<Option<usize>, Error> {
        let len = h.frame_len(self.free_format_bytes);
        if len <= 4 {
            return Ok(None);
        }
        if self.contiguous {
            return Ok(if self.src.fill(len)? >= len {
                Some(len)
            } else {
                None
            });
        }
        let avail = self.src.fill(len + 4)?;
        if avail < len {
            return Ok(None);
        }
        if avail < len + 4 {
            return Ok(if avail == len { Some(len) } else { None });
        }
        let next = &self.src.peek()[len..len + 4];
        let ok = match FrameHeader::parse(next) {
            Some(n) => n.compatible(h),
            None => {
                next.starts_with(b"ID3") || next.starts_with(b"TAG") || next.starts_with(b"APET")
            }
        };
        Ok(if ok { Some(len) } else { None })
    }

    /// Reads the frame at the cursor into `frame_buf` and advances.
    fn take_frame(&mut self, len: usize) -> Result<(), Error> {
        self.frame_buf.clear();
        self.frame_buf.extend_from_slice(&self.src.peek()[..len]);
        self.src.consume(len)?;
        self.contiguous = true;
        Ok(())
    }

    fn record_index(&mut self, frame_no: u64, offset: u64) {
        if self.exact
            && frame_no & (INDEX_STRIDE - 1) == 0
            && frame_no / INDEX_STRIDE == self.index.len() as u64
        {
            self.index.push(offset);
        }
    }

    /// Decodes the next MPEG frame(s), appending interleaved samples to
    /// `out`. Returns `Ok(false)` at end of stream.
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            let Some((h, len)) = self.find_frame()? else {
                if self.exact {
                    self.eof_frames = Some(self.frame_no);
                }
                return Ok(false);
            };
            let offset = self.src.offset();
            self.record_index(self.frame_no, offset);
            self.take_frame(len)?;
            let spf = h.samples_per_frame();
            let nch = h.channels();
            self.pcm.resize(spf * nch, 0.0);
            self.dec
                .decode(&h, &self.frame_buf, &mut self.pcm[..spf * nch])?;
            let s0 = self.sample_pos;
            self.sample_pos += spf as u64;
            self.frame_no += 1;
            if self.emit(s0, spf, nch, out) {
                return Ok(true);
            }
        }
    }

    /// Appends the samples of the frame starting at decoded-sample index
    /// `s0` that survive trimming. Returns whether anything was appended.
    fn emit(&self, s0: u64, n: usize, nch: usize, out: &mut Vec<f32>) -> bool {
        let lo = s0.max(self.skip_until);
        let hi = s0 + n as u64;
        if lo >= hi {
            return false;
        }
        let mut appended = false;
        let mut push = |a: u64, b: u64| {
            if a < b {
                self.push_samples((a - s0) as usize, (b - s0) as usize, nch, out);
                appended = true;
            }
        };
        match self.discard {
            Some((d0, d1)) => {
                push(lo, hi.min(d0));
                push(lo.max(d1), hi);
            }
            None => push(lo, hi),
        }
        appended
    }

    fn push_samples(&self, a: usize, b: usize, nch: usize, out: &mut Vec<f32>) {
        let pcm = &self.pcm;
        let clamp = |x: f32| x.clamp(-1.0, 1.0);
        match (self.channels as usize, nch) {
            (1, 1) | (2, 2) => out.extend(pcm[a * nch..b * nch].iter().map(|&x| clamp(x))),
            (2, 1) => {
                for &x in &pcm[a..b] {
                    let x = clamp(x);
                    out.push(x);
                    out.push(x);
                }
            }
            _ => out.extend(
                pcm[2 * a..2 * b]
                    .chunks_exact(2)
                    .map(|c| clamp(0.5 * (c[0] + c[1]))),
            ),
        }
    }

    /// Seeks to the given PCM frame (best effort accuracy is acceptable for
    /// VBR without TOC, exact for CBR / with TOC + decoding forward).
    /// Returns the PCM frame actually positioned at.
    pub fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let mut frame = frame;
        if let Some(d) = self.exact_decoded_total() {
            frame = frame.min(self.output_len(d));
        }
        let target = self.decoded_index(frame);
        let f = target / self.spf;
        let indexed = (f / INDEX_STRIDE) < self.index.len() as u64 || self.eof_frames.is_some();
        if indexed {
            return self.seek_exact(target);
        }
        if self.declared_cbr() {
            return self.seek_cbr(target);
        }
        match self.seek_mode {
            SeekMode::Fast => self.seek_fast(target),
            SeekMode::Exact => {
                let has_table = self.xing.as_ref().is_some_and(|x| x.toc.is_some())
                    || self.vbri.as_ref().is_some_and(|v| !v.table.is_empty());
                let last = self.index.last().copied().unwrap_or(self.first_audio);
                if has_table && self.estimate_offset(f).saturating_sub(last) > MAX_EXACT_SCAN {
                    self.seek_fast(target)
                } else {
                    self.seek_exact(target)
                }
            }
        }
    }

    /// The stream is constant-bitrate per its Info header, so frame offsets
    /// follow from the byte rate.
    fn declared_cbr(&self) -> bool {
        self.xing.as_ref().is_some_and(|x| x.is_info)
            && !self.lock.is_free_format()
            && self.avg_frame_len > 0.0
    }

    /// Positions a declared-CBR stream arithmetically (exact frame index).
    fn seek_cbr(&mut self, target: u64) -> Result<u64, Error> {
        let f = target / self.spf;
        let pre = if self.lock.layer == 3 {
            let overhead = 4 + if self.lock.crc { 2 } else { 0 } + self.lock.side_info_len();
            let main = (self.avg_frame_len - overhead as f64).max(1.0);
            (self.lock.max_reservoir() as f64 / main).ceil() as u64 + 3
        } else {
            2
        };
        let k = f.saturating_sub(pre);
        let est = self.first_audio + (k as f64 * self.avg_frame_len) as u64;
        self.src
            .seek_to(est.saturating_sub(2).max(self.first_audio))?;
        self.contiguous = false;
        if self.find_frame()?.is_none() {
            // Past the end: let the exact path find the true frame count.
            return self.seek_exact(target);
        }
        let off = self.src.offset();
        let k = ((off - self.first_audio) as f64 / self.avg_frame_len).round() as u64;
        if k > f {
            return self.seek_exact(target);
        }
        self.exact = true;
        self.reposition(off, k, true)?;
        self.skip_until = target;
        Ok(self.output_index(target))
    }

    /// Maps an output frame index to a decoded-sample index (inverse of
    /// the trimming applied in [`Self::emit`]).
    fn decoded_index(&self, out: u64) -> u64 {
        let d = out.saturating_add(self.start_skip);
        match self.discard {
            Some((d0, d1)) if d >= d0 => d.saturating_add(d1 - d0),
            _ => d,
        }
    }

    /// Inverse of [`Self::decoded_index`] for positions reported back.
    fn output_index(&self, decoded: u64) -> u64 {
        self.output_len(decoded)
    }

    /// Bytes of main data a frame contributes to the bit reservoir.
    fn main_len(&self, h: &FrameHeader, len: usize) -> usize {
        if h.layer == 3 {
            h.main_data_len(len)
        } else {
            0
        }
    }

    fn reposition(&mut self, offset: u64, frame_no: u64, contiguous: bool) -> Result<(), Error> {
        self.src.seek_to(offset)?;
        self.contiguous = contiguous;
        self.dec.reset();
        self.frame_no = frame_no;
        self.sample_pos = frame_no * self.spf;
        Ok(())
    }

    fn seek_exact(&mut self, target: u64) -> Result<u64, Error> {
        let f = target / self.spf;
        let from = f.saturating_sub(MAX_RESERVOIR_FRAMES as u64 + 2);
        let idx = ((from / INDEX_STRIDE) as usize).min(self.index.len() - 1);
        let mut n = idx as u64 * INDEX_STRIDE;
        self.src.seek_to(self.index[idx])?;
        self.contiguous = true;
        self.exact = true;
        // (offset, main data length) of the frames preceding `n`.
        let mut ring: VecDeque<(u64, usize)> = VecDeque::with_capacity(MAX_RESERVOIR_FRAMES + 4);
        while n < f {
            let Some((h, len)) = self.find_frame()? else {
                break;
            };
            let off = self.src.offset();
            self.record_index(n, off);
            if ring.len() == MAX_RESERVOIR_FRAMES + 3 {
                ring.pop_front();
            }
            ring.push_back((off, self.main_len(&h, len)));
            self.src.consume(len)?;
            self.contiguous = true;
            n += 1;
        }
        if n < f {
            // The stream ends before the target.
            self.eof_frames = Some(n);
            let end = self.src.offset();
            self.reposition(end, n, true)?;
            self.skip_until = target;
            return Ok(self.output_index(n * self.spf));
        }
        // Start early enough that frames f-2 and f-1 decode with a full
        // bit reservoir (Layer III) / the synthesis window is primed.
        let first_in_ring = n - ring.len() as u64;
        let k = if self.lock.layer == 3 {
            let need = self.lock.max_reservoir();
            let mut j = f.saturating_sub(2);
            let mut acc = 0;
            while j > first_in_ring && acc < need {
                j -= 1;
                acc += ring[(j - first_in_ring) as usize].1;
            }
            j
        } else {
            f.saturating_sub(1).max(first_in_ring)
        };
        let off = if k < n {
            ring.get((k - first_in_ring) as usize).map(|r| r.0)
        } else {
            None
        };
        let off = match off {
            Some(o) => o,
            None => self.src.offset(), // k == n == f (target in frame 0 range)
        };
        self.reposition(off, k, true)?;
        self.skip_until = target;
        Ok(self.output_index(target))
    }

    /// Estimated byte offset of frame `k` from the TOC / VBRI table / CBR rate.
    fn estimate_offset(&self, k: u64) -> u64 {
        if let Some(x) = &self.xing {
            if let (Some(toc), Some(frames)) = (&x.toc, x.frames) {
                let total = match x.bytes {
                    Some(b) => u64::from(b),
                    None if self.src.end() != u64::MAX => {
                        self.src.end().saturating_sub(self.info_offset)
                    }
                    None => 0,
                };
                if frames > 0 && total > 0 {
                    let pct = (k as f64 / f64::from(frames.max(1)) * 100.0).clamp(0.0, 100.0);
                    let i = (pct as usize).min(99);
                    let a = f64::from(toc[i]);
                    let b = if i < 99 { f64::from(toc[i + 1]) } else { 256.0 };
                    let frac = a + (b - a) * (pct - i as f64);
                    return self.info_offset + (frac / 256.0 * total as f64) as u64;
                }
            }
        }
        if let Some(v) = &self.vbri {
            if !v.table.is_empty() && v.frames_per_entry > 0 {
                let fpe = u64::from(v.frames_per_entry);
                let e = (k / fpe) as usize;
                let mut off = self.first_audio;
                for &sz in v.table.iter().take(e) {
                    off += u64::from(sz);
                }
                if let Some(&sz) = v.table.get(e) {
                    off += u64::from(sz) * (k % fpe) / fpe;
                }
                return off;
            }
        }
        self.first_audio
            .saturating_add((k as f64 * self.avg_frame_len) as u64)
    }

    fn seek_fast(&mut self, target: u64) -> Result<u64, Error> {
        let f = target / self.spf;
        let pre = match (self.lock.layer, self.lock.is_mpeg1()) {
            (3, true) => 4,
            (3, false) => 10,
            _ => 1,
        };
        let k = f.saturating_sub(pre);
        let est = self.estimate_offset(k).max(self.first_audio);
        self.src.seek_to(est)?;
        self.contiguous = false;
        if self.find_frame()?.is_none() {
            let end = self.src.offset();
            self.exact = false;
            self.reposition(end, k, false)?;
            self.skip_until = target;
            return Ok(self.output_index(target));
        }
        let off = self.src.offset();
        let is_cbr = self
            .xing
            .as_ref()
            .map_or(self.vbri.is_none(), |x| x.is_info);
        let k_est = if is_cbr && self.avg_frame_len > 0.0 {
            ((off - self.first_audio) as f64 / self.avg_frame_len).round() as u64
        } else {
            k
        };
        self.exact = false;
        self.reposition(off, k_est, true)?;
        self.skip_until = target.max(k_est * self.spf);
        Ok(self.output_index(self.skip_until))
    }

    pub fn into_inner(self) -> R {
        self.src.into_inner()
    }
}

/// Locates the first frame: skips ID3v2 tags and junk, requiring a chain
/// of compatible frames. Returns the header, frame length and free-format
/// frame size, with the cursor at the frame.
fn find_first_frame<R: Read + Seek>(
    src: &mut Source<R>,
    start: u64,
) -> Result<(FrameHeader, usize, usize), Error> {
    let mut junk_base = start;
    loop {
        if src.offset() - junk_base > MAX_INITIAL_JUNK {
            return Err(Error::InvalidData("no mpeg audio frame found"));
        }
        if src.fill(4)? < 4 {
            return Err(Error::InvalidData("no mpeg audio frame found"));
        }
        if &src.peek()[..3] == b"ID3" && src.fill(10)? >= 10 {
            if let Some(n) = tags::id3v2_len(src.peek()) {
                src.consume(usize::try_from(n).unwrap_or(usize::MAX))?;
                junk_base = src.offset();
                continue;
            }
        }
        if let Some(h) = FrameHeader::parse(src.peek()) {
            if let Some((len, ff)) = verify_chain(src, &h)? {
                return Ok((h, len, ff));
            }
        }
        src.consume(1)?;
    }
}

/// Checks that the frame at the cursor is followed by two more compatible
/// frames (or the end of data). Returns (frame length, free-format size).
fn verify_chain<R: Read + Seek>(
    src: &mut Source<R>,
    h: &FrameHeader,
) -> Result<Option<(usize, usize)>, Error> {
    let mut ff = 0;
    if h.is_free_format() {
        match detect_free_format(src, h)? {
            Some(n) => ff = n,
            None => return Ok(None),
        }
    }
    let len = h.frame_len(ff);
    if len <= 4 {
        return Ok(None);
    }
    let mut pos = 0;
    let mut cur = *h;
    for _ in 0..3 {
        let l = cur.frame_len(ff);
        if l <= 4 {
            return Ok(None);
        }
        let need = pos + l + 4;
        let avail = src.fill(need)?;
        if avail < pos + l {
            // Truncated: acceptable only if the first frame is complete.
            return Ok(if pos > 0 { Some((len, ff)) } else { None });
        }
        if avail < need {
            return Ok(Some((len, ff)));
        }
        match FrameHeader::parse(&src.peek()[pos + l..]) {
            Some(n) if n.compatible(h) => {
                pos += l;
                cur = n;
            }
            _ => return Ok(None),
        }
    }
    Ok(Some((len, ff)))
}

/// Determines the frame size (without padding) of a free-format stream by
/// locating the next two compatible headers.
fn detect_free_format<R: Read + Seek>(
    src: &mut Source<R>,
    h: &FrameHeader,
) -> Result<Option<usize>, Error> {
    let avail = src.fill(2 * MAX_FREE_FORMAT + 8)?;
    let b = src.peek();
    let min = if h.layer == 3 {
        4 + h.side_info_len()
    } else {
        8
    };
    for k in min..MAX_FREE_FORMAT.min(avail.saturating_sub(4)) {
        let Some(h2) = FrameHeader::parse(&b[k..]) else {
            continue;
        };
        if !h2.compatible(h) {
            continue;
        }
        let fb = k - h.padding_len();
        let next = k + fb + h2.padding_len();
        if next == avail {
            return Ok(Some(fb));
        }
        if next + 4 <= avail {
            if let Some(h3) = FrameHeader::parse(&b[next..]) {
                if h3.compatible(h) {
                    return Ok(Some(fb));
                }
            }
        }
    }
    Ok(None)
}

/// Returns true if `data` (the first bytes of a file) looks like MPEG audio
/// (an ID3v2 tag or a valid frame header followed by another frame header).
pub fn probe(data: &[u8]) -> bool {
    if tags::id3v2_len(data).is_some() {
        return true;
    }
    for i in 0..data.len().saturating_sub(3) {
        if data[i] != 0xFF {
            continue;
        }
        let Some(h) = FrameHeader::parse(&data[i..]) else {
            continue;
        };
        if h.is_free_format() {
            continue;
        }
        let mut pos = i;
        let mut cur = h;
        let mut matched = 0;
        loop {
            let next = pos + cur.frame_len(0);
            if next + 4 > data.len() {
                // Verified as far as the data goes.
                if matched >= 1 || (i == 0 && matched == 0) {
                    return true;
                }
                break;
            }
            match FrameHeader::parse(&data[next..]) {
                Some(n) if n.compatible(&h) => {
                    matched += 1;
                    if matched == 2 {
                        return true;
                    }
                    pos = next;
                    cur = n;
                }
                _ => break,
            }
        }
    }
    false
}
