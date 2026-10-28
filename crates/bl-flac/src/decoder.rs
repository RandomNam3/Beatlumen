//! Native FLAC stream decoder: metadata parsing, buffered frame reading,
//! error recovery (resynchronisation) and sample accurate seeking.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

use crate::frame::{parse_header, FrameDecoder, FrameError, FrameHeader, MAX_HEADER_LEN};
use crate::metadata::{parse_seek_table, MetadataBlock, SeekPoint, StreamInfo};
use crate::Error;

/// Bytes requested from the reader per refill.
const READ_CHUNK: usize = 64 * 1024;
/// Maximum number of zero samples emitted in one `next_chunk` call.
const SILENCE_CHUNK: u64 = 4096;

/// A frame located (and successfully decoded) by [`FlacDecoder::next_frame`].
#[derive(Debug, Clone, Copy)]
struct Found {
    offset: u64,
    sample: u64,
    block_size: u32,
}

/// Outcome of a frame search.
struct Search {
    found: Option<Found>,
    /// Bytes had to be skipped (lost sync or corrupt frames).
    lost: bool,
    /// Frames with a valid header whose content failed to decode / CRC.
    bad_frames: u32,
}

/// Decoder for native FLAC streams (`fLaC` signature, optionally preceded by
/// ID3v2 tags).
///
/// Output is interleaved `f32` in [-1, 1) (integer samples divided by
/// 2^(bps-1)) in FLAC channel order.
///
/// Corrupt frames (CRC mismatch, undecodable content) are skipped: the
/// decoder resynchronises on the next valid frame and, using the frame /
/// sample numbers in the frame headers, fills the gap with silence so the
/// timeline is preserved.
pub struct FlacDecoder<R: Read + Seek> {
    reader: R,
    info: StreamInfo,
    blocks: Vec<MetadataBlock>,
    seek_table: Vec<SeekPoint>,
    first_frame_offset: u64,
    stream_len: u64,
    /// Block size used to turn frame numbers into sample numbers.
    fixed_block_size: u32,
    /// Blocking strategy of the stream once known.
    variable: Option<bool>,

    // Input buffering.
    buf: Vec<u8>,
    buf_pos: usize,
    /// Stream offset of `buf[0]`.
    buf_offset: u64,
    eof: bool,
    /// Bytes to have buffered before decoding a frame.
    want: usize,
    /// Upper bound for a single frame's size.
    frame_cap: usize,

    core: FrameDecoder,

    // Output state.
    /// Sample index of the next sample `next_chunk` returns.
    position: u64,
    /// Samples still to drop (after a seek).
    skip: u64,
    pending_silence: u64,
    frame_pending: bool,
    frame_start: usize,
    /// Sync was lost; the next valid frame's number decides the gap.
    resyncing: bool,
    /// Sample number (as coded in the stream) expected for the next frame.
    expected: u64,
    tail_done: bool,
    corrupt: u64,
}

impl<R: Read + Seek> FlacDecoder<R> {
    /// Opens a stream starting with "fLaC" (an ID3v2 tag before it is skipped).
    /// The reader may be positioned anywhere; the stream starts at its
    /// current position.
    pub fn new(mut reader: R) -> Result<Self, Error> {
        let mut pos = reader.stream_position()?;
        let mut magic = [0u8; 4];
        loop {
            read_exact(&mut reader, &mut magic, "not a FLAC stream")?;
            if &magic[..3] != b"ID3" {
                break;
            }
            let mut rest = [0u8; 6];
            read_exact(&mut reader, &mut rest, "truncated ID3v2 tag")?;
            let size = rest[2..6]
                .iter()
                .fold(0u64, |a, &b| (a << 7) | (b & 0x7f) as u64);
            let footer = if rest[1] & 0x10 != 0 { 10 } else { 0 };
            pos += 10 + size + footer;
            reader.seek(SeekFrom::Start(pos))?;
        }
        if &magic != b"fLaC" {
            return Err(Error::InvalidData("missing fLaC signature"));
        }

        let mut info = None;
        let mut blocks = Vec::new();
        let mut seek_table = Vec::new();
        loop {
            let mut h = [0u8; 4];
            read_exact(&mut reader, &mut h, "truncated metadata block header")?;
            let last = h[0] & 0x80 != 0;
            let block_type = h[0] & 0x7f;
            let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
            if block_type == 127 {
                return Err(Error::InvalidData("forbidden metadata block type"));
            }
            if info.is_none() && block_type != MetadataBlock::STREAMINFO {
                return Err(Error::InvalidData(
                    "STREAMINFO is not the first metadata block",
                ));
            }
            if block_type == MetadataBlock::PADDING {
                reader.seek(SeekFrom::Current(len as i64))?;
            } else {
                let mut data = Vec::new();
                (&mut reader).take(len).read_to_end(&mut data)?;
                if data.len() as u64 != len {
                    return Err(Error::InvalidData("truncated metadata block"));
                }
                match block_type {
                    MetadataBlock::STREAMINFO if info.is_none() => {
                        info = Some(StreamInfo::parse(&data)?)
                    }
                    MetadataBlock::SEEKTABLE => seek_table = parse_seek_table(&data),
                    _ => {}
                }
                blocks.push(MetadataBlock { block_type, data });
            }
            if last {
                break;
            }
        }
        let info = info.ok_or(Error::InvalidData("missing STREAMINFO"))?;
        let first_frame_offset = reader.stream_position()?;
        let stream_len = reader.seek(SeekFrom::End(0))?.max(first_frame_offset);
        reader.seek(SeekFrom::Start(first_frame_offset))?;

        let max_frame = info.max_frame_size as usize;
        let mut dec = FlacDecoder {
            reader,
            info,
            blocks,
            seek_table,
            first_frame_offset,
            stream_len,
            fixed_block_size: info.max_block_size as u32,
            variable: None,
            buf: Vec::new(),
            buf_pos: 0,
            buf_offset: first_frame_offset,
            eof: false,
            want: if max_frame > 0 {
                max_frame + MAX_HEADER_LEN
            } else {
                READ_CHUNK
            },
            frame_cap: if max_frame > 0 {
                (max_frame * 4).max(1 << 20)
            } else {
                16 << 20
            },
            core: FrameDecoder::new(),
            position: 0,
            skip: 0,
            pending_silence: 0,
            frame_pending: false,
            frame_start: 0,
            resyncing: false,
            expected: 0,
            tail_done: false,
            corrupt: 0,
        };
        // Learn the blocking strategy (and the block size of fixed block
        // size streams) from the first frame header.
        dec.fill(MAX_HEADER_LEN)?;
        if let Ok(h) = parse_header(&dec.buf) {
            dec.variable = Some(h.variable);
            if !h.variable
                && (info.min_block_size != info.max_block_size || info.max_block_size == 0)
            {
                dec.fixed_block_size = h.block_size;
            }
        }
        Ok(dec)
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

    /// Total number of PCM frames (samples per channel) from STREAMINFO, if
    /// known.
    pub fn total_frames(&self) -> Option<u64> {
        (self.info.total_samples > 0).then_some(self.info.total_samples)
    }

    /// The decoded STREAMINFO block.
    pub fn stream_info(&self) -> &StreamInfo {
        &self.info
    }

    /// All metadata blocks except PADDING, in stream order (STREAMINFO,
    /// SEEKTABLE, VORBIS_COMMENT, PICTURE, ...), used for VORBIS_COMMENT and
    /// PICTURE parsing by `bl-media`.
    pub fn metadata_blocks(&self) -> &[MetadataBlock] {
        &self.blocks
    }

    /// Index of the next PCM frame `next_chunk` will return.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Number of corruption events encountered so far (frames failing their
    /// CRC or decoding, and lost synchronisation).
    pub fn corrupt_frames(&self) -> u64 {
        self.corrupt
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Decodes the next frame (normally one FLAC frame, i.e. one block of
    /// samples, or a run of silence replacing corrupt data), appending
    /// interleaved `f32` samples to `out` (`out` is not cleared). Returns
    /// `Ok(false)` at end of stream. Errors are only returned for I/O
    /// failures; corrupt data is skipped (see the type documentation).
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        if !self.prepare()? {
            return Ok(false);
        }
        let nch = self.info.channels as usize;
        if self.pending_silence > 0 {
            let n = self.pending_silence.min(SILENCE_CHUNK);
            self.pending_silence -= n;
            self.position += n;
            out.resize(out.len() + n as usize * nch, 0.0);
        } else {
            self.core.write_f32(self.frame_start, out);
            self.position += (self.core.block_size - self.frame_start) as u64;
            self.frame_pending = false;
        }
        Ok(true)
    }

    /// Like [`next_chunk`](Self::next_chunk) but appends the raw integer
    /// samples (right-aligned, at the frame's bit depth, which is the
    /// stream's [`bits_per_sample`](Self::bits_per_sample) for all
    /// practical files). Useful for bit-exact output.
    pub fn next_chunk_i32(&mut self, out: &mut Vec<i32>) -> Result<bool, Error> {
        if !self.prepare()? {
            return Ok(false);
        }
        let nch = self.info.channels as usize;
        if self.pending_silence > 0 {
            let n = self.pending_silence.min(SILENCE_CHUNK);
            self.pending_silence -= n;
            self.position += n;
            out.resize(out.len() + n as usize * nch, 0);
        } else {
            self.core.write_i32(self.frame_start, out);
            self.position += (self.core.block_size - self.frame_start) as u64;
            self.frame_pending = false;
        }
        Ok(true)
    }

    /// Sample accurate seek (uses SEEKTABLE if present, otherwise bisection
    /// on frame headers), then decodes and discards up to the target.
    /// Returns the PCM frame actually positioned at (the target, or the end
    /// of the stream if the target lies beyond it).
    pub fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        self.pending_silence = 0;
        self.frame_pending = false;
        self.skip = 0;
        self.resyncing = false;
        self.tail_done = false;
        if let Some(total) = self.total_frames() {
            if frame >= total {
                self.reposition(self.stream_len)?;
                self.position = total;
                self.tail_done = true;
                return Ok(total);
            }
        }
        let target = frame;
        let (mut lo_off, mut lo_sample) = (self.first_frame_offset, 0u64);
        let mut hi_off = self.stream_len;
        // Sample number at `hi_off`, when known (used for interpolation).
        let mut hi_sample = self.total_frames();

        // Narrow the range with the seek table.
        let idx = self.seek_table.partition_point(|p| p.sample <= target);
        if idx > 0 {
            let p = self.seek_table[idx - 1];
            let off = self.first_frame_offset.saturating_add(p.offset);
            if p.sample > 0 && off < hi_off && self.frame_sample_at(off)? == Some(p.sample) {
                lo_off = off;
                lo_sample = p.sample;
            }
        }
        if let Some(p) = self.seek_table.get(idx) {
            let off = self.first_frame_offset.saturating_add(p.offset);
            if off > lo_off && off < hi_off {
                hi_off = off;
                hi_sample = Some(p.sample);
            }
        }

        // Search on frame headers until the remaining range is short.
        // Interpolation steps (which usually land right before the target)
        // alternate with bisection steps (which bound the step count).
        let threshold = if self.info.max_frame_size > 0 {
            (2 * self.info.max_frame_size as u64).max(16 * 1024)
        } else {
            64 * 1024
        };
        let mut step = 0u32;
        while hi_off - lo_off > threshold {
            let mid = match hi_sample {
                Some(hs) if step & 1 == 0 && hs > lo_sample => {
                    let span = (hi_off - lo_off) as u128;
                    let frac = span * (target - lo_sample) as u128 / (hs - lo_sample) as u128;
                    let est = lo_off + frac.min(span) as u64;
                    // Aim a little early so that the frame found precedes the target.
                    est.saturating_sub(threshold / 4)
                        .clamp(lo_off + 1, hi_off - 1)
                }
                _ => lo_off + (hi_off - lo_off) / 2,
            };
            step += 1;
            self.reposition(mid)?;
            match self.next_frame(true, hi_off)?.found {
                Some(f) if f.sample <= target && f.sample >= lo_sample => {
                    lo_off = f.offset;
                    lo_sample = f.sample;
                    // Target in this frame or the next one: decoding forward
                    // is cheaper than another search step.
                    if target < f.sample + 2 * f.block_size as u64 {
                        break;
                    }
                }
                // No frame starts in [mid, f.offset), so the target's frame
                // starts before f.offset.
                Some(f) if f.sample > target => {
                    hi_off = f.offset;
                    hi_sample = Some(f.sample);
                }
                // No frame starts in [mid, hi_off): the sample number at
                // `mid` equals the one at `hi_off`.
                _ => hi_off = mid,
            }
        }

        // Decode forward from the closest frame at or before the target.
        self.reposition(lo_off)?;
        self.position = lo_sample;
        self.expected = lo_sample;
        self.skip = target - lo_sample;
        self.prepare()?;
        self.skip = 0;
        Ok(self.position)
    }

    /// Ensures there is output pending (silence or a decoded frame) with any
    /// pending `skip` applied. Returns `false` at end of stream.
    fn prepare(&mut self) -> Result<bool, Error> {
        loop {
            if self.pending_silence > 0 {
                if self.skip == 0 {
                    return Ok(true);
                }
                let d = self.skip.min(self.pending_silence);
                self.skip -= d;
                self.pending_silence -= d;
                self.position += d;
                continue;
            }
            if self.frame_pending {
                let remaining = (self.core.block_size - self.frame_start) as u64;
                if self.skip == 0 && remaining > 0 {
                    return Ok(true);
                }
                let d = self.skip.min(remaining);
                self.skip -= d;
                self.frame_start += d as usize;
                self.position += d;
                if self.frame_start >= self.core.block_size {
                    self.frame_pending = false;
                }
                continue;
            }
            if !self.decode_next_frame()? {
                return Ok(false);
            }
        }
    }

    /// Decodes the next frame (with error recovery) into `core`, possibly
    /// scheduling silence for lost samples. Returns `false` at end of stream.
    fn decode_next_frame(&mut self) -> Result<bool, Error> {
        let s = self.next_frame(false, u64::MAX)?;
        if s.bad_frames > 0 || (s.lost && s.found.is_some()) {
            self.corrupt += 1;
        }
        if s.lost {
            self.resyncing = true;
        }
        match s.found {
            Some(f) => {
                if self.resyncing {
                    if f.sample > self.expected && f.sample - self.expected <= self.gap_limit() {
                        self.pending_silence = f.sample - self.expected;
                    }
                    self.resyncing = false;
                }
                self.expected = f.sample.saturating_add(f.block_size as u64);
                self.frame_pending = true;
                self.frame_start = 0;
                Ok(true)
            }
            None => {
                if self.resyncing && !self.tail_done {
                    self.tail_done = true;
                    if let Some(total) = self.total_frames() {
                        let limit = 2 * self.info.max_block_size.max(4096) as u64;
                        if total > self.position && total - self.position <= limit {
                            self.pending_silence = total - self.position;
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            }
        }
    }

    /// Largest gap (in samples) between the last good frame and the next one
    /// found after resynchronisation that is filled with silence.
    fn gap_limit(&self) -> u64 {
        (self.info.sample_rate as u64 * 60).max(1 << 20)
    }

    /// Searches for the next decodable frame starting at the current buffer
    /// position and decodes it into `core`. With `strict`, candidates must
    /// also agree with STREAMINFO (used when scanning from arbitrary
    /// offsets); after a failure the search is always strict. Frames starting
    /// at or beyond `limit` are not considered.
    fn next_frame(&mut self, strict: bool, limit: u64) -> Result<Search, Error> {
        let mut lost = false;
        let mut bad_frames = 0;
        loop {
            self.fill(MAX_HEADER_LEN)?;
            if self.buf_pos >= self.buf.len() {
                return Ok(Search {
                    found: None,
                    lost,
                    bad_frames,
                });
            }
            let offset = self.buf_offset + self.buf_pos as u64;
            if offset >= limit {
                return Ok(Search {
                    found: None,
                    lost,
                    bad_frames,
                });
            }
            if let Ok(h) = parse_header(&self.buf[self.buf_pos..]) {
                if self.acceptable(&h, strict || lost) {
                    match self.decode_at_pos(&h)? {
                        Ok(len) => {
                            self.buf_pos += len;
                            self.want = self.want.max(len + MAX_HEADER_LEN);
                            if self.variable.is_none() {
                                self.variable = Some(h.variable);
                            }
                            let sample = h.first_sample(self.fixed_block_size);
                            return Ok(Search {
                                found: Some(Found {
                                    offset,
                                    sample,
                                    block_size: h.block_size,
                                }),
                                lost,
                                bad_frames,
                            });
                        }
                        Err(_) => bad_frames += 1,
                    }
                }
            }
            // Not a (valid) frame here: skip to the next possible sync byte.
            lost = true;
            let from = (self.buf_pos + 1).min(self.buf.len());
            self.buf_pos = match self.buf[from..].iter().position(|&b| b == 0xff) {
                Some(i) => from + i,
                None => self.buf.len(),
            };
        }
    }

    /// Whether a parsed header can be decoded as part of this stream.
    fn acceptable(&self, h: &FrameHeader, strict: bool) -> bool {
        if h.channels != self.info.channels as u32 {
            return false;
        }
        if !strict {
            return true;
        }
        let si = &self.info;
        (h.bps == 0 || h.bps == si.bits_per_sample as u32)
            && (h.sample_rate == 0 || h.sample_rate == si.sample_rate)
            && !matches!(self.variable, Some(v) if v != h.variable)
            && (si.max_block_size == 0 || h.block_size <= si.max_block_size as u32)
            && (si.total_samples == 0 || h.first_sample(self.fixed_block_size) < si.total_samples)
    }

    /// Decodes the frame at the buffer position, reading more input as
    /// needed. The outer error is an I/O error, the inner one a frame error.
    fn decode_at_pos(&mut self, h: &FrameHeader) -> Result<Result<usize, FrameError>, Error> {
        let bps = if h.bps != 0 {
            h.bps
        } else {
            self.info.bits_per_sample as u32
        };
        let mut want = self.want;
        loop {
            self.fill(want)?;
            match self.core.decode(&self.buf[self.buf_pos..], h, bps) {
                Err(FrameError::Eof) if !self.eof => {
                    let avail = self.buf.len() - self.buf_pos;
                    if avail >= self.frame_cap {
                        return Ok(Err(FrameError::Eof));
                    }
                    want = (avail * 2).max(want).max(avail + 1).min(self.frame_cap);
                }
                r => return Ok(r),
            }
        }
    }

    /// Returns the sample number of a valid frame starting exactly at `off`.
    fn frame_sample_at(&mut self, off: u64) -> Result<Option<u64>, Error> {
        self.reposition(off)?;
        Ok(self
            .next_frame(true, off + 1)?
            .found
            .filter(|f| f.offset == off)
            .map(|f| f.sample))
    }

    /// Moves the read position to stream offset `off`.
    fn reposition(&mut self, off: u64) -> Result<(), Error> {
        let end = self.buf_offset + self.buf.len() as u64;
        if off >= self.buf_offset && off <= end && (off < end || self.eof) {
            self.buf_pos = (off - self.buf_offset) as usize;
            return Ok(());
        }
        self.reader.seek(SeekFrom::Start(off))?;
        self.buf.clear();
        self.buf_pos = 0;
        self.buf_offset = off;
        self.eof = false;
        Ok(())
    }

    /// Ensures at least `want` bytes are buffered after `buf_pos` (unless the
    /// stream ends first).
    fn fill(&mut self, want: usize) -> Result<(), Error> {
        let avail = self.buf.len() - self.buf_pos;
        if avail >= want || self.eof {
            return Ok(());
        }
        if self.buf_pos > 0 {
            self.buf.copy_within(self.buf_pos.., 0);
            self.buf.truncate(avail);
            self.buf_offset += self.buf_pos as u64;
            self.buf_pos = 0;
        }
        let target = want.max(READ_CHUNK);
        while self.buf.len() < want {
            let old = self.buf.len();
            self.buf.resize(target, 0);
            match self.reader.read(&mut self.buf[old..]) {
                Ok(0) => {
                    self.buf.truncate(old);
                    self.eof = true;
                    break;
                }
                Ok(n) => self.buf.truncate(old + n),
                Err(e) if e.kind() == ErrorKind::Interrupted => self.buf.truncate(old),
                Err(e) => {
                    self.buf.truncate(old);
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8], what: &'static str) -> Result<(), Error> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == ErrorKind::UnexpectedEof {
            Error::InvalidData(what)
        } else {
            Error::Io(e)
        }
    })
}
