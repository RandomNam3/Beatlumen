//! On-the-fly transcoding of any `bl_media` decoder to an Ogg Opus byte
//! stream: downmix to stereo (more than two channels), resampling to
//! 48 kHz, CELT-only encoding and Ogg packaging, pulled through
//! [`std::io::Read`].
//!
//! Memory stays bounded: one decoder chunk, less than one Opus frame of
//! PCM and the bytes of the current Ogg page are buffered at a time.

use crate::ogg::{OggOpusWriter, OpusHeader};
use bl_media::{Decoder, Resampler};
use bl_opus::{Error, OpusEncoder};
use std::io::{self, Read};

/// Consecutive decoder errors after which the stream is aborted.
const MAX_DECODE_ERRORS: u32 = 16;
/// Consecutive empty decoder chunks after which the input is taken as ended.
const MAX_EMPTY_CHUNKS: u32 = 1000;

/// Options for [`Transcoder::with_options`].
#[derive(Debug, Clone)]
pub struct TranscodeOptions {
    /// Constant bitrate in bit/s (e.g. 48000, 64000, 96000).
    pub bitrate: u32,
    /// Encode mono even for multichannel input.
    pub mono: bool,
    /// `OpusTags` comments, e.g. `("TITLE", "...")`.
    pub tags: Vec<(String, String)>,
    /// Ogg stream serial (random when None). A fixed one makes the same
    /// input give the same bytes, e.g. when a client reconnects.
    pub serial: Option<u32>,
}

impl TranscodeOptions {
    pub fn new(bitrate: u32) -> Self {
        TranscodeOptions {
            bitrate,
            mono: false,
            tags: Vec::new(),
            serial: None,
        }
    }
}

#[derive(PartialEq, Eq)]
enum State {
    Decoding,
    Done,
}

/// Pull-based transcoder producing an Ogg Opus stream.
pub struct Transcoder {
    decoder: Box<dyn Decoder>,
    encoder: OpusEncoder,
    resampler: Resampler,
    writer: Option<OggOpusWriter<Vec<u8>>>,
    in_channels: usize,
    channels: usize,
    in_rate: u32,
    pre_skip: u64,
    /// Input frames read from the decoder.
    in_frames: u64,
    /// Frames (48 kHz) fed into the encoder, including padding.
    fed: u64,
    /// Resampled audio waiting for a full encoder frame.
    pcm: Vec<f32>,
    chunk: Vec<f32>,
    mixed: Vec<f32>,
    resampled: Vec<f32>,
    packet: Vec<u8>,
    out: Vec<u8>,
    out_pos: usize,
    state: State,
    errors: u32,
    empty: u32,
}

/// Transcodes `decoder` to an Ogg Opus stream at `bitrate` bit/s.
pub fn transcode(decoder: Box<dyn Decoder>, bitrate: u32) -> Result<Transcoder, Error> {
    Transcoder::with_options(decoder, &TranscodeOptions::new(bitrate))
}

impl Transcoder {
    pub fn new(decoder: Box<dyn Decoder>, bitrate: u32) -> Result<Transcoder, Error> {
        Self::with_options(decoder, &TranscodeOptions::new(bitrate))
    }

    pub fn with_options(
        decoder: Box<dyn Decoder>,
        opts: &TranscodeOptions,
    ) -> Result<Transcoder, Error> {
        let spec = decoder.spec();
        let in_channels = spec.channels as usize;
        if in_channels == 0 || spec.sample_rate == 0 {
            return Err(Error::InvalidData("input without channels or sample rate"));
        }
        let channels = if opts.mono { 1 } else { in_channels.min(2) };
        let encoder = OpusEncoder::new(channels as u8, opts.bitrate)?;
        let resampler = Resampler::new(spec.sample_rate, 48000, channels as u16);
        let pre_skip = OpusEncoder::LOOKAHEAD as u64 + resampler.latency() as u64;
        let pre_skip16 =
            u16::try_from(pre_skip).map_err(|_| Error::Unsupported("input sample rate"))?;
        let tags: Vec<(&str, &str)> = opts
            .tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let head = OpusHeader {
            channels: channels as u8,
            pre_skip: pre_skip16,
            input_sample_rate: spec.sample_rate,
            output_gain: 0,
        };
        let writer = OggOpusWriter::with_header(Vec::new(), head, &tags, opts.serial)
            .map_err(|_| Error::InvalidData("ogg header"))?;
        Ok(Transcoder {
            decoder,
            encoder,
            resampler,
            writer: Some(writer),
            in_channels,
            channels,
            in_rate: spec.sample_rate,
            pre_skip,
            in_frames: 0,
            fed: 0,
            pcm: Vec::new(),
            chunk: Vec::new(),
            mixed: Vec::new(),
            resampled: Vec::new(),
            packet: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            state: State::Decoding,
            errors: 0,
            empty: 0,
        })
    }

    /// Output channel count (1 or 2).
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Pre-skip of the stream (encoder look-ahead plus resampler delay).
    pub fn pre_skip(&self) -> u64 {
        self.pre_skip
    }

    /// Length of the original audio at 48 kHz for `in_frames` input frames.
    fn frames_48k(&self, in_frames: u64) -> u64 {
        (in_frames * 48000).div_ceil(self.in_rate as u64)
    }

    /// Converts a decoded chunk to the output layout and rate and appends
    /// it to the PCM queue.
    fn push_input(&mut self, frames: usize) {
        let ch = self.in_channels;
        let input: &[f32] = if self.channels == 1 && ch > 1 {
            self.mixed.clear();
            let g = 1.0 / ch as f32;
            for f in self.chunk[..frames * ch].chunks_exact(ch) {
                self.mixed.push(f.iter().sum::<f32>() * g);
            }
            &self.mixed
        } else if ch > 2 {
            self.mixed.clear();
            bl_media::downmix_to_stereo(&self.chunk[..frames * ch], ch as u16, &mut self.mixed);
            &self.mixed
        } else {
            &self.chunk[..frames * ch]
        };
        self.resampled.clear();
        self.resampler.process(input, &mut self.resampled);
        self.pcm.extend_from_slice(&self.resampled);
    }

    /// Encodes all complete frames in the PCM queue.
    fn encode_frames(&mut self) -> io::Result<()> {
        let n = OpusEncoder::FRAME_SIZE * self.channels;
        let mut pos = 0;
        while self.pcm.len() - pos >= n {
            self.packet.clear();
            self.encoder
                .encode(&self.pcm[pos..pos + n], &mut self.packet)
                .map_err(io::Error::other)?;
            if let Some(w) = self.writer.as_mut() {
                w.write_packet(&self.packet, OpusEncoder::FRAME_SIZE as u32)?;
                self.out.append(w.get_mut());
            }
            self.fed += OpusEncoder::FRAME_SIZE as u64;
            pos += n;
        }
        self.pcm.drain(..pos);
        Ok(())
    }

    /// Flushes the resampler and the encoder and ends the stream.
    fn finish(&mut self) -> io::Result<()> {
        let total = self.frames_48k(self.in_frames);
        // Push the resampler's delayed tail out with silence.
        let latency = self.resampler.latency() as u64;
        let mut guard = 0;
        let queued = |t: &Self| t.fed + (t.pcm.len() / t.channels) as u64;
        while queued(self) < total + latency && guard < 64 {
            let frames = (self.in_rate as usize / 50).max(1);
            self.chunk.clear();
            self.chunk.resize(frames * self.in_channels, 0.0);
            self.push_input(frames);
            guard += 1;
        }
        // Encode until every input sample has left the encoder.
        let target = self.pre_skip + total;
        let n = OpusEncoder::FRAME_SIZE;
        // Samples beyond the target are filter tail only.
        if queued(self) > target {
            let keep = target.saturating_sub(self.fed) as usize * self.channels;
            self.pcm.truncate(keep);
        }
        let have = queued(self);
        let frames = have.max(target).div_ceil(n as u64) * n as u64;
        let pad = (frames - have) as usize;
        self.pcm.resize(self.pcm.len() + pad * self.channels, 0.0);
        self.encode_frames()?;
        if let Some(w) = self.writer.take() {
            let rest = w.finish(Some(total))?;
            self.out.extend_from_slice(&rest);
        }
        self.state = State::Done;
        Ok(())
    }

    /// Runs the pipeline until output bytes are available or the stream
    /// has ended.
    fn fill(&mut self) -> io::Result<()> {
        while self.out_pos >= self.out.len() && self.state == State::Decoding {
            self.out.clear();
            self.out_pos = 0;
            if let Some(w) = self.writer.as_mut() {
                self.out.append(w.get_mut());
            }
            self.chunk.clear();
            match self.decoder.next_chunk(&mut self.chunk) {
                Ok(true) => {
                    let frames = self.chunk.len() / self.in_channels;
                    if frames == 0 {
                        // Tolerate a few empty chunks, not a stuck decoder.
                        self.empty += 1;
                        if self.empty > MAX_EMPTY_CHUNKS {
                            self.finish()?;
                        }
                        continue;
                    }
                    self.errors = 0;
                    self.empty = 0;
                    self.in_frames += frames as u64;
                    self.push_input(frames);
                    self.encode_frames()?;
                }
                Ok(false) => self.finish()?,
                Err(e) => {
                    // Damaged data: skip it, unless the decoder is stuck.
                    self.errors += 1;
                    if self.errors >= MAX_DECODE_ERRORS {
                        self.state = State::Done;
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Read for Transcoder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.fill()?;
        let avail = &self.out[self.out_pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.out_pos += n;
        Ok(n)
    }
}
